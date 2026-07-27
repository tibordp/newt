//! Decryption for ZIP entries: traditional PKWARE ("ZipCrypto") and WinZip
//! AES (AE-1/AE-2), the read-side mirror of `crate::crypto`.

use aes::cipher::{BlockEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use super::AesStrength;

pub(super) const ZIPCRYPTO_HEADER_LEN: usize = 12;
pub(super) const AES_VERIFIER_LEN: usize = 2;
pub(super) const AES_AUTH_LEN: usize = 10;

const PBKDF2_ROUNDS: u32 = 1000;

/// Password-derived key material for one entry, independent of stream
/// position — cursors clone this to restart without re-deriving (PBKDF2) or
/// re-verifying. Opaque to callers.
#[derive(Clone)]
pub struct EntryKey(pub(super) EntryKeyInner);

#[derive(Clone)]
pub(super) enum EntryKeyInner {
    /// ZipCrypto key registers after the password *and* the entry's 12-byte
    /// header have been mixed in — payload decryption starts here.
    ZipCrypto([u32; 3]),
    Aes {
        strength: AesStrength,
        aes_key: Vec<u8>,
        hmac_key: Vec<u8>,
    },
}

/// Derive AES keys and check the 2-byte password verifier. `None` means the
/// password is wrong.
pub(super) fn derive_aes_key(
    password: &[u8],
    salt: &[u8],
    verifier: &[u8],
    strength: AesStrength,
) -> Option<EntryKey> {
    let key_len = strength.key_len();
    let mut derived = vec![0u8; 2 * key_len + AES_VERIFIER_LEN];
    pbkdf2::pbkdf2_hmac::<Sha1>(password, salt, PBKDF2_ROUNDS, &mut derived);
    if &derived[2 * key_len..] != verifier {
        return None;
    }
    Some(EntryKey(EntryKeyInner::Aes {
        strength,
        aes_key: derived[..key_len].to_vec(),
        hmac_key: derived[key_len..2 * key_len].to_vec(),
    }))
}

/// Mix the password into fresh ZipCrypto key registers, decrypt the entry's
/// 12-byte header, and compare the check byte. The returned key holds the
/// post-header register state — exactly what payload decryption starts from.
/// `None` means the password is wrong (with 1/256 false-accept odds; the CRC
/// over a full read is the backstop).
pub(super) fn verify_zipcrypto(
    password: &[u8],
    header: &[u8; ZIPCRYPTO_HEADER_LEN],
    check: u8,
) -> Option<EntryKey> {
    let mut keys = ZipCryptoKeys::new();
    for &b in password {
        keys.update(b);
    }
    keys.verify_header(header, check)
        .then_some(EntryKey(EntryKeyInner::ZipCrypto(keys.0)))
}

/// A positioned decryption stream for one entry's ciphertext.
pub(super) enum Decryptor {
    ZipCrypto(ZipCryptoKeys),
    Aes(Box<AesCtr>),
}

impl Decryptor {
    /// Start a stream at ciphertext offset 0 (ZipCrypto: just past the
    /// 12-byte header; AES: just past salt+verifier).
    pub(super) fn new(key: &EntryKey) -> Decryptor {
        match &key.0 {
            EntryKeyInner::ZipCrypto(keys) => Decryptor::ZipCrypto(ZipCryptoKeys(*keys)),
            EntryKeyInner::Aes {
                strength,
                aes_key,
                hmac_key,
            } => Decryptor::Aes(Box::new(AesCtr::new(*strength, aes_key, hmac_key))),
        }
    }

    /// Decrypt in place, advancing the stream position.
    pub(super) fn decrypt(&mut self, data: &mut [u8]) {
        match self {
            Decryptor::ZipCrypto(keys) => keys.decrypt(data),
            Decryptor::Aes(ctr) => ctr.decrypt(data),
        }
    }
}

// ---------------------------------------------------------------------------
// ZipCrypto
// ---------------------------------------------------------------------------

pub(super) struct ZipCryptoKeys([u32; 3]);

impl ZipCryptoKeys {
    fn new() -> Self {
        ZipCryptoKeys([0x1234_5678, 0x2345_6789, 0x3456_7890])
    }

    fn update(&mut self, plain: u8) {
        self.0[0] = crc32_byte(self.0[0], plain);
        self.0[1] = self.0[1]
            .wrapping_add(self.0[0] & 0xFF)
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        self.0[2] = crc32_byte(self.0[2], (self.0[1] >> 24) as u8);
    }

    fn stream_byte(&self) -> u8 {
        let temp = (self.0[2] | 2) & 0xFFFF;
        ((temp.wrapping_mul(temp ^ 1)) >> 8) as u8
    }

    fn decrypt(&mut self, data: &mut [u8]) {
        for b in data {
            let plain = *b ^ self.stream_byte();
            self.update(plain);
            *b = plain;
        }
    }

    /// Decrypt the 12-byte header and compare its final byte with the
    /// check byte. A match is necessary but not sufficient (1/256
    /// false-accept); the CRC over a full read is the real backstop.
    pub(super) fn verify_header(&mut self, header: &[u8; ZIPCRYPTO_HEADER_LEN], check: u8) -> bool {
        let mut buf = *header;
        self.decrypt(&mut buf);
        buf[ZIPCRYPTO_HEADER_LEN - 1] == check
    }
}

/// Test-support encryption (the crate only decrypts ZipCrypto): 12-byte
/// header whose final byte is `check`, followed by the encrypted payload.
#[cfg(test)]
pub(super) fn zipcrypto_encrypt(password: &[u8], check: u8, payload: &[u8]) -> Vec<u8> {
    let mut keys = ZipCryptoKeys::new();
    for &b in password {
        keys.update(b);
    }
    let mut header = *b"newt-test-h\0";
    header[ZIPCRYPTO_HEADER_LEN - 1] = check;
    let mut out = Vec::with_capacity(ZIPCRYPTO_HEADER_LEN + payload.len());
    for &p in header.iter().chain(payload) {
        out.push(p ^ keys.stream_byte());
        keys.update(p);
    }
    out
}

fn crc32_byte(crc: u32, b: u8) -> u32 {
    CRC_TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8)
}

static CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

// ---------------------------------------------------------------------------
// WinZip AES-CTR
// ---------------------------------------------------------------------------

enum AnyAes {
    Aes128(aes::Aes128),
    Aes192(aes::Aes192),
    Aes256(aes::Aes256),
}

impl AnyAes {
    fn encrypt_block(&self, block: &mut [u8; 16]) {
        match self {
            AnyAes::Aes128(c) => c.encrypt_block(block.into()),
            AnyAes::Aes192(c) => c.encrypt_block(block.into()),
            AnyAes::Aes256(c) => c.encrypt_block(block.into()),
        }
    }
}

/// WinZip's nonstandard CTR: little-endian 64-bit counter starting at 1, no
/// nonce (see `crate::crypto`). The keystream depends only on the block
/// index, so a stream can start at any ciphertext offset — `seek_to` is what
/// makes AES range reads O(1). The HMAC (encrypt-then-MAC, over ciphertext)
/// is folded in only while the stream runs from offset 0 without seeking;
/// [`AesCtr::take_auth`] returns `None` otherwise.
pub(super) struct AesCtr {
    cipher: AnyAes,
    hmac: Option<Hmac<Sha1>>,
    counter: u64,
    keystream: [u8; 16],
    used: usize,
}

impl AesCtr {
    fn new(strength: AesStrength, aes_key: &[u8], hmac_key: &[u8]) -> Self {
        let cipher = match strength {
            AesStrength::Aes128 => AnyAes::Aes128(aes::Aes128::new_from_slice(aes_key).unwrap()),
            AesStrength::Aes192 => AnyAes::Aes192(aes::Aes192::new_from_slice(aes_key).unwrap()),
            AesStrength::Aes256 => AnyAes::Aes256(aes::Aes256::new_from_slice(aes_key).unwrap()),
        };
        let hmac = <Hmac<Sha1> as Mac>::new_from_slice(hmac_key).unwrap();
        AesCtr {
            cipher,
            hmac: Some(hmac),
            counter: 0,
            keystream: [0; 16],
            used: 16,
        }
    }

    /// Position the keystream at an arbitrary ciphertext offset. Forfeits
    /// authentication for this stream.
    pub(super) fn seek_to(&mut self, offset: u64) {
        self.hmac = None;
        self.counter = offset / 16;
        let rem = (offset % 16) as usize;
        if rem == 0 {
            self.used = 16;
        } else {
            self.counter += 1;
            let mut block = [0u8; 16];
            block[..8].copy_from_slice(&self.counter.to_le_bytes());
            self.cipher.encrypt_block(&mut block);
            self.keystream = block;
            self.used = rem;
        }
    }

    fn decrypt(&mut self, data: &mut [u8]) {
        if let Some(hmac) = &mut self.hmac {
            hmac.update(data);
        }
        for b in data {
            if self.used == 16 {
                self.counter += 1;
                let mut block = [0u8; 16];
                block[..8].copy_from_slice(&self.counter.to_le_bytes());
                self.cipher.encrypt_block(&mut block);
                self.keystream = block;
                self.used = 0;
            }
            *b ^= self.keystream[self.used];
            self.used += 1;
        }
    }

    /// Whether the stream is still authenticated (ran from offset 0 with no
    /// seek), i.e. `take_auth` will yield a code.
    pub(super) fn has_auth(&self) -> bool {
        self.hmac.is_some()
    }

    /// The accumulated authentication code, when the stream covered the
    /// whole ciphertext from offset 0.
    pub(super) fn take_auth(&mut self) -> Option<[u8; AES_AUTH_LEN]> {
        let tag = self.hmac.take()?.finalize().into_bytes();
        Some(tag[..AES_AUTH_LEN].try_into().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip against the write-side AES-256 encryptor: same PBKDF2
    /// inputs must yield the same verifier and keystream.
    #[test]
    fn aes_round_trip_with_encryptor() {
        let salt = [7u8; 16];
        let (prelude, mut enc) = crate::crypto::AesCtrEncryptor::with_salt("secret", &salt);
        let plain = b"the quick brown fox jumps over the lazy dog";
        let mut ciphertext = Vec::new();
        enc.encrypt(plain, &mut ciphertext);
        let auth = enc.finish();

        let key = derive_aes_key(
            b"secret",
            &prelude[..16],
            &prelude[16..],
            AesStrength::Aes256,
        )
        .expect("verifier must match");
        let mut dec = Decryptor::new(&key);
        let mut buf = ciphertext.clone();
        dec.decrypt(&mut buf);
        assert_eq!(buf.as_slice(), plain.as_slice());
        let Decryptor::Aes(ctr) = &mut dec else {
            unreachable!()
        };
        assert_eq!(ctr.take_auth(), Some(auth));

        assert!(derive_aes_key(b"wrong", &salt, &prelude[16..], AesStrength::Aes256).is_none());
    }

    #[test]
    fn aes_seek_matches_sequential() {
        let salt = [3u8; 16];
        let (prelude, mut enc) = crate::crypto::AesCtrEncryptor::with_salt("pw", &salt);
        let plain: Vec<u8> = (0..200u8).collect();
        let mut ciphertext = Vec::new();
        enc.encrypt(&plain, &mut ciphertext);

        let key = derive_aes_key(b"pw", &salt, &prelude[16..], AesStrength::Aes256).unwrap();
        for offset in [0usize, 1, 15, 16, 17, 31, 33, 160] {
            let mut dec = Decryptor::new(&key);
            let Decryptor::Aes(ctr) = &mut dec else {
                unreachable!()
            };
            ctr.seek_to(offset as u64);
            let mut buf = ciphertext[offset..].to_vec();
            ctr.decrypt(&mut buf);
            assert_eq!(buf.as_slice(), &plain[offset..], "offset {}", offset);
        }
    }

    /// Self-consistency: encrypt by running the cipher forward (encrypt =
    /// plain ^ stream, then update with plain), then verify + decrypt.
    #[test]
    fn zipcrypto_stream() {
        fn keys_for(password: &[u8]) -> ZipCryptoKeys {
            let mut keys = ZipCryptoKeys::new();
            for &b in password {
                keys.update(b);
            }
            keys
        }
        let mut enc_keys = keys_for(b"secret");
        let header = *b"abcdefghijk\x7f";
        let check = header[11];
        let mut encrypted = [0u8; 12];
        for (i, &p) in header.iter().enumerate() {
            encrypted[i] = p ^ enc_keys.stream_byte();
            enc_keys.update(p);
        }
        let plain = b"hello zipcrypto";
        let mut payload = [0u8; 15];
        for (i, &p) in plain.iter().enumerate() {
            payload[i] = p ^ enc_keys.stream_byte();
            enc_keys.update(p);
        }

        let key = verify_zipcrypto(b"secret", &encrypted, check).expect("right password verifies");
        let mut dec = Decryptor::new(&key);
        let mut buf = payload;
        dec.decrypt(&mut buf);
        assert_eq!(&buf, plain);

        assert!(verify_zipcrypto(b"hunter2", &encrypted, check).is_none());
    }
}
