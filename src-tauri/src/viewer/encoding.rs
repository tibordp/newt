//! Text-viewer character encodings: the Encoding menu catalogue, detection
//! over a file prefix, and the transcoding helpers behind copy and search.
//!
//! Encodings are named by their encoding_rs canonical name, which is also a
//! valid `TextDecoder` label — the frontend decodes with the same string.

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

pub struct EncodingGroup {
    pub label: &'static str,
    pub encodings: &'static [&'static str],
}

/// The Encoding menu, grouped by script. Every WHATWG encoding the webview
/// can decode is here except `replacement` and `x-user-defined`. UTF-32 has
/// no `TextDecoder` and is not offered.
pub const CATALOGUE: &[EncodingGroup] = &[
    EncodingGroup {
        label: "Unicode",
        encodings: &["UTF-8", "UTF-16LE", "UTF-16BE"],
    },
    EncodingGroup {
        label: "Western",
        encodings: &["windows-1252", "ISO-8859-15", "macintosh"],
    },
    EncodingGroup {
        label: "Central European",
        encodings: &["windows-1250", "ISO-8859-2", "ISO-8859-16"],
    },
    EncodingGroup {
        label: "Cyrillic",
        encodings: &[
            "windows-1251",
            "KOI8-R",
            "KOI8-U",
            "ISO-8859-5",
            "IBM866",
            "x-mac-cyrillic",
        ],
    },
    EncodingGroup {
        label: "Greek",
        encodings: &["windows-1253", "ISO-8859-7"],
    },
    EncodingGroup {
        label: "Turkish",
        encodings: &["windows-1254"],
    },
    EncodingGroup {
        label: "Hebrew",
        encodings: &["windows-1255", "ISO-8859-8", "ISO-8859-8-I"],
    },
    EncodingGroup {
        label: "Arabic",
        encodings: &["windows-1256", "ISO-8859-6"],
    },
    EncodingGroup {
        label: "Baltic",
        encodings: &["windows-1257", "ISO-8859-4", "ISO-8859-13"],
    },
    EncodingGroup {
        label: "Other ISO-8859",
        encodings: &["ISO-8859-3", "ISO-8859-10", "ISO-8859-14"],
    },
    EncodingGroup {
        label: "Vietnamese",
        encodings: &["windows-1258"],
    },
    EncodingGroup {
        label: "Thai",
        encodings: &["windows-874"],
    },
    EncodingGroup {
        label: "Japanese",
        encodings: &["Shift_JIS", "EUC-JP", "ISO-2022-JP"],
    },
    EncodingGroup {
        label: "Chinese",
        encodings: &["GBK", "gb18030", "Big5"],
    },
    EncodingGroup {
        label: "Korean",
        encodings: &["EUC-KR"],
    },
];

/// The catalogue entry for a menu id suffix, if it names one.
pub fn catalogue_name(name: &str) -> Option<&'static str> {
    CATALOGUE
        .iter()
        .flat_map(|g| g.encodings.iter().copied())
        .find(|n| *n == name)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, specta::Type)]
pub struct DetectedEncoding {
    pub encoding: String,
    /// Length of the byte-order mark the detection came from; 0 if none.
    pub bom_len: u32,
}

/// Detection state for the viewer's current file.
#[derive(Debug, Clone, Default, serde::Serialize, specta::Type)]
pub struct ViewerEncoding {
    /// `None` until the prefix read completes (or fails).
    pub detected: Option<DetectedEncoding>,
    /// Explicit pick from the Encoding menu; overrides `detected`.
    pub selected: Option<String>,
}

impl ViewerEncoding {
    pub fn effective(&self) -> Option<&str> {
        self.selected
            .as_deref()
            .or(self.detected.as_ref().map(|d| d.encoding.as_str()))
    }
}

/// Detect the encoding of a file from its prefix. `eof` says whether the
/// prefix is the whole file; a cut-off trailing sequence is otherwise not
/// held against UTF-8.
pub fn detect(prefix: &[u8], eof: bool) -> DetectedEncoding {
    const BOMS: [(&[u8], &Encoding); 3] = [
        (b"\xEF\xBB\xBF", UTF_8),
        (b"\xFF\xFE", UTF_16LE),
        (b"\xFE\xFF", UTF_16BE),
    ];
    for (bom, enc) in BOMS {
        if prefix.starts_with(bom) {
            return DetectedEncoding {
                encoding: enc.name().to_string(),
                bom_len: bom.len() as u32,
            };
        }
    }

    // NUL bytes are valid UTF-8, so BOM-less UTF-16 must be ruled out first.
    let encoding = if let Some(enc) = utf16_without_bom(prefix) {
        enc
    } else if is_utf8(prefix, eof) {
        UTF_8
    } else {
        let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
        detector.feed(prefix, eof);
        detector.guess(None, Utf8Detection::Deny)
    };
    DetectedEncoding {
        encoding: encoding.name().to_string(),
        bom_len: 0,
    }
}

fn is_utf8(prefix: &[u8], eof: bool) -> bool {
    match std::str::from_utf8(prefix) {
        Ok(_) => true,
        Err(e) => !eof && e.error_len().is_none(),
    }
}

/// Latin-script UTF-16 has a zero high byte in most code units. Text with
/// the mirror pattern does not exist, so a clear majority on one side with
/// almost nothing on the other is UTF-16 of that endianness.
fn utf16_without_bom(prefix: &[u8]) -> Option<&'static Encoding> {
    let (pairs, _) = prefix.as_chunks::<2>();
    let n = pairs.len();
    let (mut le, mut be) = (0usize, 0usize);
    for &[lo, hi] in pairs {
        match (lo, hi) {
            (0, 0) => {}
            (_, 0) => le += 1,
            (0, _) => be += 1,
            _ => {}
        }
    }
    if le * 2 > n && be * 4 < le {
        Some(UTF_16LE)
    } else if be * 2 > n && le * 4 < be {
        Some(UTF_16BE)
    } else {
        None
    }
}

fn encoding(name: &str) -> &'static Encoding {
    Encoding::for_label(name.as_bytes()).expect("encoding name minted by the viewer catalogue")
}

pub fn decode(bytes: &[u8], name: &str) -> String {
    encoding(name)
        .decode_without_bom_handling(bytes)
        .0
        .into_owned()
}

/// `text` as bytes in `name`. encoding_rs encodes UTF-16 as UTF-8 (WHATWG
/// output-encoding rule), so UTF-16 is spelled out here.
pub fn encode(text: &str, name: &str) -> Vec<u8> {
    let enc = encoding(name);
    if enc == UTF_16LE {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    } else if enc == UTF_16BE {
        text.encode_utf16().flat_map(u16::to_be_bytes).collect()
    } else {
        enc.encode(text).0.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_names_are_canonical() {
        for group in CATALOGUE {
            for name in group.encodings {
                let enc = Encoding::for_label(name.as_bytes())
                    .unwrap_or_else(|| panic!("{name} is not a WHATWG label"));
                assert_eq!(enc.name(), *name);
            }
        }
    }

    #[test]
    fn boms() {
        assert_eq!(
            detect(b"\xEF\xBB\xBFhello", true),
            DetectedEncoding {
                encoding: "UTF-8".into(),
                bom_len: 3
            }
        );
        assert_eq!(
            detect(b"\xFF\xFEh\0i\0", true),
            DetectedEncoding {
                encoding: "UTF-16LE".into(),
                bom_len: 2
            }
        );
        assert_eq!(
            detect(b"\xFE\xFF\0h\0i", true),
            DetectedEncoding {
                encoding: "UTF-16BE".into(),
                bom_len: 2
            }
        );
    }

    #[test]
    fn utf8_including_cut_sequence() {
        assert_eq!(detect(b"", true).encoding, "UTF-8");
        assert_eq!(detect("plain ascii\n".as_bytes(), true).encoding, "UTF-8");
        assert_eq!(detect("Grüße 日本\n".as_bytes(), true).encoding, "UTF-8");
        let cut = &"日本語".as_bytes()[..4];
        assert_eq!(detect(cut, false).encoding, "UTF-8");
        assert_ne!(detect(cut, true).encoding, "UTF-8");
    }

    #[test]
    fn utf16_without_bom() {
        let le = encode("Hello, world!\nSecond line\n", "UTF-16LE");
        assert_eq!(detect(&le, true).encoding, "UTF-16LE");
        let be = encode("Hello, world!\nSecond line\n", "UTF-16BE");
        assert_eq!(detect(&be, true).encoding, "UTF-16BE");
    }

    #[test]
    fn legacy_codepages() {
        let ru = encode(
            "Привет, мир! Это тестовый текст на русском языке.\n",
            "windows-1251",
        );
        assert_eq!(detect(&ru, true).encoding, "windows-1251");
        let de = encode("Grüße aus Köln, schöne Straße.\n", "windows-1252");
        assert_eq!(detect(&de, true).encoding, "windows-1252");
        let jp = encode(
            "これは日本語のテキストです。漢字も含まれています。\n",
            "Shift_JIS",
        );
        assert_eq!(detect(&jp, true).encoding, "Shift_JIS");
    }

    #[test]
    fn transcode_round_trip() {
        for (name, text) in [
            ("windows-1252", "abc é\n"),
            ("UTF-16LE", "abc é\n"),
            ("UTF-16BE", "abc é\n"),
            ("Shift_JIS", "abc 日本\n"),
        ] {
            assert_eq!(decode(&encode(text, name), name), text, "{name}");
        }
        assert_eq!(encode("a", "UTF-16LE"), b"a\0");
        assert_eq!(encode("a", "UTF-16BE"), b"\0a");
    }
}
