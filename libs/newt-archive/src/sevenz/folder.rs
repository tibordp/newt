//! Folders: coder graphs linearized into iluvatar codec chains, plus the 7z
//! flavour of AES key derivation.

use std::ops::Range;
use std::sync::Arc;

use iluvatar::{Bcj2Streams, BcjArch, Codec, CodecSpec, DecompressStatus};
use sha2::{Digest, Sha256};

use super::{Result, SevenZError, corrupt};

#[derive(Debug, Clone)]
pub(super) struct Coder {
    pub id: Vec<u8>,
    pub num_in: usize,
    pub num_out: usize,
    pub props: Vec<u8>,
}

/// A folder as the header describes it.
#[derive(Debug, Clone)]
pub(super) struct RawFolder {
    /// Unpacked side first: coder 0 produces the folder's output.
    pub coders: Vec<Coder>,
    /// `(in_index, out_index)`: input stream `in_index` (numbered across
    /// all coders) is fed by output stream `out_index`.
    pub bind_pairs: Vec<(usize, usize)>,
    /// Input streams fed from packed streams, in packed-stream order.
    pub packed_streams: Vec<usize>,
    /// One per output stream.
    pub unpack_sizes: Vec<u64>,
    pub crc32: Option<u32>,
}

impl RawFolder {
    pub(super) fn num_out_streams(&self) -> usize {
        self.coders.iter().map(|c| c.num_out).sum()
    }

    /// Index of the output stream no bind pair consumes: the folder's
    /// unpacked output.
    fn main_out_stream(&self) -> Result<usize> {
        (0..self.num_out_streams())
            .find(|o| !self.bind_pairs.iter().any(|&(_, out)| out == *o))
            .ok_or_else(|| corrupt("folder has no unbound output stream"))
    }

    pub(super) fn unpack_size(&self) -> Result<u64> {
        let main = self.main_out_stream()?;
        self.unpack_sizes
            .get(main)
            .copied()
            .ok_or_else(|| corrupt("folder unpack size missing"))
    }

    fn coder_of_out_stream(&self, index: usize) -> Option<(usize, usize)> {
        let mut base = 0;
        for (i, c) in self.coders.iter().enumerate() {
            if index < base + c.num_out {
                return Some((i, index - base));
            }
            base += c.num_out;
        }
        None
    }

    fn first_in_stream(&self, coder: usize) -> usize {
        self.coders[..coder].iter().map(|c| c.num_in).sum()
    }

    fn first_out_stream(&self, coder: usize) -> usize {
        self.coders[..coder].iter().map(|c| c.num_out).sum()
    }

    /// The output stream bound to input stream `input`.
    fn source_of(&self, input: usize) -> std::result::Result<usize, String> {
        self.bind_pairs
            .iter()
            .find(|&&(i, _)| i == input)
            .map(|&(_, o)| o)
            .ok_or_else(|| "input stream fed by nothing".to_string())
    }

    /// Walk from output stream `top` back to a packed stream, one
    /// single-in single-out coder per hop.
    fn chain_from(&self, top: usize) -> std::result::Result<Line, String> {
        let mut chain = Vec::new();
        let mut aes = None;
        let mut out = top;
        for _ in 0..=self.coders.len() {
            let (ci, _) = self
                .coder_of_out_stream(out)
                .ok_or_else(|| "output stream out of range".to_string())?;
            let coder = &self.coders[ci];
            if coder.num_in != 1 || coder.num_out != 1 {
                return Err(format!(
                    "coder {} with {} inputs and {} outputs",
                    describe_id(&coder.id),
                    coder.num_in,
                    coder.num_out
                ));
            }
            let unpack_size = self.unpack_sizes[self.first_out_stream(ci)];
            let (codec, params) = codec_for(coder, unpack_size)?;
            if let Some(p) = params {
                aes = Some(p);
            }
            chain.push(codec);
            let in_stream = self.first_in_stream(ci);
            if let Some(packed) = self.packed_streams.iter().position(|&i| i == in_stream) {
                chain.reverse();
                return Ok(Line {
                    chain,
                    packed,
                    unpacked_len: Some(self.unpack_sizes[top]),
                    aes,
                });
            }
            out = self.source_of(in_stream)?;
        }
        Err("coder graph loops".to_string())
    }

    /// The line feeding input stream `input`: a packed stream directly
    /// (its length is the packed size, known only to the caller) or a
    /// coder chain.
    fn line_into(&self, input: usize) -> std::result::Result<Line, String> {
        match self.packed_streams.iter().position(|&i| i == input) {
            Some(packed) => Ok(Line {
                chain: vec![Codec::Copy],
                packed,
                unpacked_len: None,
                aes: None,
            }),
            None => self.chain_from(self.source_of(input)?),
        }
    }

    /// The folder as a main line ending at the unpacked output, plus
    /// BCJ2's three side lines when the top coder is BCJ2. Anything else
    /// with several inputs is unsupported.
    fn linearize(&self) -> std::result::Result<Graph, String> {
        let main_out = self.main_out_stream().map_err(|e| e.to_string())?;
        let (ci, _) = self
            .coder_of_out_stream(main_out)
            .ok_or_else(|| "output stream out of range".to_string())?;
        let coder = &self.coders[ci];
        if coder.id != BCJ2_ID {
            return Ok(Graph {
                main: self.chain_from(main_out)?,
                sides: None,
            });
        }
        if coder.num_in != 4 || coder.num_out != 1 {
            return Err(format!(
                "BCJ2 with {} inputs and {} outputs",
                coder.num_in, coder.num_out
            ));
        }
        let first_in = self.first_in_stream(ci);
        let mut main = self.line_into(first_in)?;
        main.chain.push(Codec::Bcj2 { streams: None });
        let call = self.line_into(first_in + 1)?;
        let jump = self.line_into(first_in + 2)?;
        let rc = self.line_into(first_in + 3)?;
        // One password derives one key: every encrypted line must agree
        // on salt and rounds (7-Zip's do; only their IVs differ).
        let params: Vec<&AesParams> = [&main, &call, &jump, &rc]
            .into_iter()
            .filter_map(|l| l.aes.as_ref())
            .collect();
        if let Some(first) = params.first()
            && params
                .iter()
                .any(|p| p.salt != first.salt || p.cycles != first.cycles)
        {
            return Err("BCJ2 with differing AES parameters".to_string());
        }
        Ok(Graph {
            main,
            sides: Some([call, jump, rc]),
        })
    }
}

const BCJ2_ID: [u8; 4] = [0x03, 0x03, 0x01, 0x1B];

/// A BCJ2 side stream may be held in memory up to this, decoded.
pub const MAX_BCJ2_SIDE: u64 = 128 << 20;

/// One decoding line of a folder's coder graph.
struct Line {
    chain: Vec<Codec>,
    /// Index into the folder's packed streams.
    packed: usize,
    /// `None` for a packed stream fed straight in.
    unpacked_len: Option<u64>,
    aes: Option<AesParams>,
}

struct Graph {
    main: Line,
    /// BCJ2's CALL, JUMP and range-coder lines.
    sides: Option<[Line; 3]>,
}

/// Everything needed to decode one folder.
#[derive(Debug, Clone)]
pub struct FolderInfo {
    /// Absolute byte range of the main packed stream.
    pub packed: Range<u64>,
    pub unpacked_len: u64,
    /// Number of entries stored back to back in the unpacked stream.
    pub num_entries: usize,
    /// CRC of the whole unpacked stream, when recorded.
    pub crc32: Option<u32>,
    /// Encryption parameters when a stage is AES; `codec_spec` then needs
    /// a key.
    pub aes: Option<AesParams>,
    /// BCJ2's side streams, to decode whole before the folder can be read.
    pub bcj2: Option<Bcj2Sides>,
    /// Bytes `verify_spec` yields: the folder's unpacked length, or for
    /// BCJ2 the main stream's before conversion.
    pub verify_len: u64,
    chain: std::result::Result<Vec<Codec>, String>,
}

/// A BCJ2 side stream: where its packed bytes are and how to decode them.
#[derive(Debug, Clone)]
pub struct SideStream {
    pub packed: Range<u64>,
    pub unpacked_len: u64,
    chain: Vec<Codec>,
}

impl SideStream {
    /// Decode the whole stream from its packed bytes; an encrypted one
    /// needs the folder's key.
    pub fn decode(&self, packed: &[u8], key: Option<[u8; 32]>) -> Result<Vec<u8>> {
        let mut spec = CodecSpec(self.chain.clone());
        if self.chain.iter().any(|c| matches!(c, Codec::AesCbc { .. })) {
            spec.set_aes_key(key.ok_or(SevenZError::PasswordRequired)?);
        }
        decode_all(&spec, packed, self.unpacked_len)
    }
}

#[derive(Debug, Clone)]
pub struct Bcj2Sides {
    pub call: SideStream,
    pub jump: SideStream,
    pub rc: SideStream,
}

impl FolderInfo {
    /// `packed` are the folder's packed streams' byte ranges, in the
    /// header's packed-stream order.
    pub(super) fn new(raw: &RawFolder, packed: &[Range<u64>], num_entries: usize) -> Result<Self> {
        let range_of = |line: &Line| {
            packed
                .get(line.packed)
                .cloned()
                .ok_or_else(|| corrupt("folder refers to a missing packed stream"))
        };
        let side_of = |line: Line| -> Result<SideStream> {
            let packed = range_of(&line)?;
            Ok(SideStream {
                unpacked_len: line.unpacked_len.unwrap_or(packed.end - packed.start),
                packed,
                chain: line.chain,
            })
        };
        let mut info = FolderInfo {
            packed: packed.first().cloned().unwrap_or(0..0),
            unpacked_len: raw.unpack_size()?,
            num_entries,
            crc32: raw.crc32,
            aes: None,
            bcj2: None,
            verify_len: 0,
            chain: Err(String::new()),
        };
        info.verify_len = info.unpacked_len;
        let graph = match raw.linearize() {
            Ok(graph) => graph,
            Err(why) => {
                info.chain = Err(why);
                return Ok(info);
            }
        };
        info.packed = range_of(&graph.main)?;
        info.verify_len = graph
            .main
            .unpacked_len
            .unwrap_or(info.packed.end - info.packed.start);
        info.aes = graph.main.aes;
        info.chain = Ok(graph.main.chain);
        if let Some([call, jump, rc]) = graph.sides {
            if info.aes.is_none() {
                info.aes = [&call, &jump, &rc].into_iter().find_map(|l| l.aes.clone());
            }
            let sides = Bcj2Sides {
                call: side_of(call)?,
                jump: side_of(jump)?,
                rc: side_of(rc)?,
            };
            if [&sides.call, &sides.jump, &sides.rc]
                .iter()
                .any(|side| side.unpacked_len > MAX_BCJ2_SIDE)
            {
                info.chain = Err(format!(
                    "BCJ2 side stream larger than {} MiB",
                    MAX_BCJ2_SIDE >> 20
                ));
            }
            info.bcj2 = Some(sides);
        }
        Ok(info)
    }

    /// Whether the folder can be decoded at all; `Err` names what is
    /// missing.
    pub fn supported(&self) -> Result<()> {
        self.chain
            .as_ref()
            .map(|_| ())
            .map_err(|why| SevenZError::Unsupported(why.clone()))
    }

    /// Bytes a checkpoint costs in this folder: an LZMA checkpoint carries
    /// its live dictionary, a zstd one its window. Sizing the checkpoint
    /// interval from this keeps a folder's index proportionate.
    pub fn checkpoint_cost(&self) -> u64 {
        let Ok(chain) = &self.chain else {
            return 0;
        };
        chain
            .iter()
            .map(|c| match c {
                Codec::Lzma { dict_size, .. } => u64::from(*dict_size),
                Codec::Lzma2 { dict_prop } => lzma2_dict_size(*dict_prop),
                Codec::Zstd => 8 << 20,
                Codec::Bzip2 => 1 << 20,
                Codec::Deflate { .. } => 64 << 10,
                _ => 4 << 10,
            })
            .max()
            .unwrap_or(0)
            .min(self.unpacked_len)
    }

    /// The codec chain, with `key` filled into the AES stages and `sides`
    /// into the BCJ2 stage. Errors for unsupported coders and for a
    /// missing key; a BCJ2 folder's caller supplies its decoded sides.
    pub fn codec_spec(
        &self,
        key: Option<[u8; 32]>,
        sides: Option<Arc<Bcj2Streams>>,
    ) -> Result<CodecSpec> {
        let mut spec = self.verify_spec(key)?;
        if self.bcj2.is_some() {
            spec.0.push(Codec::Bcj2 {
                streams: Some(sides.expect("BCJ2 folder decoded without its side streams")),
            });
        }
        Ok(spec)
    }

    /// The main chain up to BCJ2, if any: enough to trial-decode a key
    /// against without the side streams.
    pub fn verify_spec(&self, key: Option<[u8; 32]>) -> Result<CodecSpec> {
        let chain = self
            .chain
            .as_ref()
            .map_err(|why| SevenZError::Unsupported(why.clone()))?;
        let stages = chain
            .iter()
            .filter(|c| !matches!(c, Codec::Bcj2 { .. }))
            .cloned()
            .collect();
        let mut spec = CodecSpec(stages);
        if self.aes.is_some() {
            spec.set_aes_key(key.ok_or(SevenZError::PasswordRequired)?);
        }
        Ok(spec)
    }
}

/// Decode a whole packed stream in memory.
pub(super) fn decode_all(spec: &CodecSpec, packed: &[u8], unpacked_len: u64) -> Result<Vec<u8>> {
    let mut dec = spec
        .create()
        .map_err(|e| SevenZError::Unsupported(e.to_string()))?;
    let mut out = Vec::with_capacity(unpacked_len as usize);
    let mut buf = vec![0u8; 64 * 1024];
    let mut pos = 0;
    let mut rounds = 0;
    loop {
        rounds += 1;
        if rounds > super::MAX_ROUNDS * 64 {
            return Err(corrupt("packed stream does not terminate"));
        }
        let input = &packed[pos..];
        let r = dec
            .decompress(input, &mut buf)
            .map_err(|e| corrupt(format!("packed stream: {}", e)))?;
        pos += r.bytes_consumed;
        out.extend_from_slice(&buf[..r.bytes_produced]);
        if out.len() as u64 > unpacked_len {
            return Err(corrupt("packed stream unpacks larger than declared"));
        }
        if r.status == DecompressStatus::StreamEnd || out.len() as u64 == unpacked_len {
            break;
        }
        if input.is_empty() && r.bytes_produced == 0 {
            return Err(corrupt("packed stream truncated"));
        }
    }
    if out.len() as u64 != unpacked_len {
        return Err(corrupt("packed stream unpacks short"));
    }
    Ok(out)
}

/// The 7z AES coder's properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AesParams {
    pub salt: Vec<u8>,
    pub iv: [u8; 16],
    /// `2^cycles` SHA-256 rounds; `0x3F` means the password is the key.
    pub cycles: u8,
}

impl AesParams {
    fn parse(props: &[u8]) -> Result<Self> {
        let b0 = *props
            .first()
            .ok_or_else(|| corrupt("AES coder without properties"))?;
        let cycles = b0 & 0x3F;
        let (salt_len, iv_len) = if b0 & 0xC0 == 0 {
            (0, 0)
        } else {
            let b1 = *props
                .get(1)
                .ok_or_else(|| corrupt("AES properties truncated"))?;
            (
                usize::from((b0 >> 7) & 1) + usize::from(b1 >> 4),
                usize::from((b0 >> 6) & 1) + usize::from(b1 & 0x0F),
            )
        };
        let start = if salt_len + iv_len == 0 { 1 } else { 2 };
        let body = props
            .get(start..start + salt_len + iv_len)
            .ok_or_else(|| corrupt("AES properties truncated"))?;
        let mut iv = [0u8; 16];
        let iv_src = &body[salt_len..];
        iv[..iv_src.len().min(16)].copy_from_slice(&iv_src[..iv_src.len().min(16)]);
        Ok(AesParams {
            salt: body[..salt_len].to_vec(),
            iv,
            cycles,
        })
    }
}

/// 7z's password-to-key derivation: SHA-256 over `salt ‖ UTF-16LE(password)
/// ‖ counter` repeated `2^cycles` times in one running hash.
pub fn derive_key(password: &str, params: &AesParams) -> [u8; 32] {
    let pw: Vec<u8> = password
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let mut key = [0u8; 32];
    if params.cycles == 0x3F {
        for (slot, &b) in key.iter_mut().zip(params.salt.iter().chain(&pw)) {
            *slot = b;
        }
        return key;
    }
    let mut hasher = Sha256::new();
    let rounds = 1u64 << params.cycles;
    for round in 0..rounds {
        hasher.update(&params.salt);
        hasher.update(&pw);
        hasher.update(round.to_le_bytes());
    }
    key.copy_from_slice(&hasher.finalize());
    key
}

fn lzma2_dict_size(prop: u8) -> u64 {
    if prop >= 40 {
        return u64::from(u32::MAX);
    }
    (2 | u64::from(prop & 1)) << (u64::from(prop) / 2 + 11)
}

fn describe_id(id: &[u8]) -> String {
    let hex: Vec<String> = id.iter().map(|b| format!("{:02x}", b)).collect();
    match id {
        [0x03, 0x04, 0x01] => "PPMd".to_string(),
        [0x03, 0x03, 0x01, 0x1B] => "BCJ2".to_string(),
        [0x04, 0x01, 0x09] => "Deflate64".to_string(),
        _ => format!("coder {}", hex.join("")),
    }
}

fn codec_for(
    coder: &Coder,
    unpack_size: u64,
) -> std::result::Result<(Codec, Option<AesParams>), String> {
    let p = &coder.props;
    let codec = match coder.id.as_slice() {
        [0x00] => Codec::Copy,
        [0x03] => Codec::Delta {
            distance: *p.first().ok_or("Delta without properties")?,
        },
        [0x03, 0x03, 0x01, 0x03] => Codec::Bcj(BcjArch::X86),
        [0x03, 0x03, 0x02, 0x05] => Codec::Bcj(BcjArch::PowerPc),
        [0x03, 0x03, 0x04, 0x01] => Codec::Bcj(BcjArch::Ia64),
        [0x03, 0x03, 0x05, 0x01] => Codec::Bcj(BcjArch::Arm),
        [0x03, 0x03, 0x07, 0x01] => Codec::Bcj(BcjArch::ArmThumb),
        [0x03, 0x03, 0x08, 0x05] => Codec::Bcj(BcjArch::Sparc),
        [0x0A] => Codec::Bcj(BcjArch::Arm64),
        [0x0B] => Codec::Bcj(BcjArch::RiscV),
        [0x03, 0x01, 0x01] => {
            if p.len() < 5 {
                return Err("LZMA properties truncated".to_string());
            }
            Codec::Lzma {
                props: p[0],
                dict_size: u32::from_le_bytes([p[1], p[2], p[3], p[4]]),
                unpacked_len: Some(unpack_size),
            }
        }
        [0x21] => Codec::Lzma2 {
            dict_prop: *p.first().ok_or("LZMA2 without properties")?,
        },
        [0x04, 0x01, 0x08] => Codec::Deflate { raw: true },
        [0x04, 0x02, 0x02] => Codec::Bzip2,
        [0x04, 0xF7, 0x11, 0x01] => Codec::Zstd,
        [0x06, 0xF1, 0x07, 0x01] => {
            let params = AesParams::parse(p).map_err(|e| e.to_string())?;
            return Ok((
                Codec::AesCbc {
                    key: None,
                    iv: params.iv,
                    len: Some(unpack_size),
                },
                Some(params),
            ));
        }
        other => return Err(describe_id(other)),
    };
    Ok((codec, None))
}
