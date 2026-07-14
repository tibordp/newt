//! WinZip AES encryption (AE-1/AE-2) for zip entries.
//!
//! Per entry: `salt(16) ‖ password-verifier(2) ‖ AES-256-CTR ciphertext ‖
//! truncated HMAC-SHA1(10)`. Keys come from PBKDF2-HMAC-SHA1 (1000
//! iterations); the CTR counter is WinZip's nonstandard little-endian counter
//! starting at 1 with no nonce, which is why this is hand-rolled over the raw
//! block cipher instead of using the (big-endian) `ctr` crate.

use std::io;

use aes::Aes256;
use aes::cipher::{BlockEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha1::Sha1;

pub const SALT_LEN: usize = 16; // AES-256 strength
pub const VERIFIER_LEN: usize = 2;
pub const AUTH_LEN: usize = 10;

const PBKDF2_ROUNDS: u32 = 1000;
const KEY_LEN: usize = 32;

pub struct AesCtrEncryptor {
    cipher: Aes256,
    hmac: Hmac<Sha1>,
    counter: u64,
    keystream: [u8; 16],
    used: usize,
}

impl AesCtrEncryptor {
    /// Derives per-entry keys from the password and a fresh random salt.
    /// Returns the entry prelude (salt ‖ verifier) alongside the encryptor.
    pub fn new(password: &str) -> io::Result<(Vec<u8>, Self)> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(io::Error::other)?;
        Ok(Self::with_salt(password, &salt))
    }

    fn with_salt(password: &str, salt: &[u8; SALT_LEN]) -> (Vec<u8>, Self) {
        let mut derived = [0u8; 2 * KEY_LEN + VERIFIER_LEN];
        pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), salt, PBKDF2_ROUNDS, &mut derived);

        let cipher = Aes256::new_from_slice(&derived[..KEY_LEN]).unwrap();
        let hmac = <Hmac<Sha1> as Mac>::new_from_slice(&derived[KEY_LEN..2 * KEY_LEN]).unwrap();

        let mut prelude = salt.to_vec();
        prelude.extend_from_slice(&derived[2 * KEY_LEN..]);
        (
            prelude,
            AesCtrEncryptor {
                cipher,
                hmac,
                counter: 0,
                keystream: [0; 16],
                used: 16,
            },
        )
    }

    /// Encrypts `input`, appending ciphertext to `out` and folding it into the
    /// authentication code (encrypt-then-MAC).
    pub fn encrypt(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let start = out.len();
        for &byte in input {
            if self.used == 16 {
                self.counter += 1;
                let mut block = [0u8; 16];
                block[..8].copy_from_slice(&self.counter.to_le_bytes());
                self.cipher.encrypt_block((&mut block).into());
                self.keystream = block;
                self.used = 0;
            }
            out.push(byte ^ self.keystream[self.used]);
            self.used += 1;
        }
        self.hmac.update(&out[start..]);
    }

    /// The 10-byte authentication code that trails the ciphertext.
    pub fn finish(self) -> [u8; AUTH_LEN] {
        let tag = self.hmac.finalize().into_bytes();
        tag[..AUTH_LEN].try_into().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_derivation_and_stream() {
        let salt = [7u8; SALT_LEN];
        let (prelude_a, mut enc_a) = AesCtrEncryptor::with_salt("secret", &salt);
        let (prelude_b, mut enc_b) = AesCtrEncryptor::with_salt("secret", &salt);
        assert_eq!(prelude_a, prelude_b);
        assert_eq!(prelude_a.len(), SALT_LEN + VERIFIER_LEN);

        // Chunking must not affect the keystream.
        let plain = b"the quick brown fox jumps over the lazy dog";
        let mut whole = Vec::new();
        enc_a.encrypt(plain, &mut whole);
        let mut split = Vec::new();
        enc_b.encrypt(&plain[..7], &mut split);
        enc_b.encrypt(&plain[7..], &mut split);
        assert_eq!(whole, split);
        assert_ne!(whole.as_slice(), plain.as_slice());
        assert_eq!(enc_a.finish(), enc_b.finish());
    }

    #[test]
    fn different_password_differs() {
        let salt = [7u8; SALT_LEN];
        let (prelude_a, _) = AesCtrEncryptor::with_salt("secret", &salt);
        let (prelude_b, _) = AesCtrEncryptor::with_salt("hunter2", &salt);
        assert_ne!(prelude_a[SALT_LEN..], prelude_b[SALT_LEN..]);
    }
}
