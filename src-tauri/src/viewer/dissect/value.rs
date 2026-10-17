//! A field's value on the wire: fillyfoal's text for it, and its parts by
//! kind for the viewer to show richly. Integers that may pass 2^53 travel
//! as decimal strings, byte strings as lowercase hex.

use fillyfoal::value::FlagDef;
use fillyfoal::{
    Addr, Color, ColorDepth, ColorModel, ColorSpace, Digest, Exactness, GuidOrder, Radix, Res,
    SizeUnit, Value, Verdict, Zone,
};
use serde::Serialize;

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectValue {
    /// The value as read: a name for an enum, the set flags, the number in
    /// the radix the format gives it.
    text: String,
    /// The number underneath, in the other radix (or the raw integer of an
    /// enum, flags or timestamp).
    raw: Option<String>,
    data: DissectValueData,
}

#[derive(Serialize, specta::Type, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DissectValueData {
    Bool,
    Number {
        value: String,
        bits: u8,
        signed: bool,
        /// How the format presents it.
        radix: DissectRadix,
    },
    Enum {
        raw: String,
        bits: u8,
        known: bool,
    },
    Flags {
        raw: String,
        bits: u8,
        /// Set bits no entry of the table accounts for.
        unknown: String,
        /// Every entry of the flag table, matched or not.
        table: Vec<DissectFlag>,
    },
    Float {
        /// The width the file stores it in: 16, 32, 64 or 80.
        bits: u8,
    },
    Time {
        /// Seconds since 1970-01-01 00:00:00: the UTC instant, or for a
        /// `local` zone the wall clock read as if it were UTC.
        secs: String,
        nanos: u32,
        zone: DissectZone,
        res: DissectRes,
    },
    Text {
        /// The text unescaped (`text` is escaped for display).
        value: String,
    },
    Bytes {
        len: usize,
    },
    Guid {
        order: DissectGuidOrder,
        /// RFC 4122 text, lowercase and unbraced, whatever the stored order.
        uuid: String,
    },
    Checksum {
        /// "CRC-32", "SHA-256", or the format's own name.
        algo: String,
        /// The stored value as the format writes it.
        stored: String,
        /// The width of an integer check (a CRC, a sum); `None` for a
        /// digest.
        bits: Option<u8>,
        verdict: DissectVerdict,
    },
    Size {
        n: String,
        /// Bytes per unit: 1 for a count of bytes, else the block size.
        unit: Option<u32>,
        /// `None` when it overflows 64 bits.
        bytes: Option<String>,
        bits: u8,
    },
    Duration {
        ticks: String,
        per_sec: String,
    },
    Addr {
        family: DissectAddrFamily,
        /// The address bytes in network order.
        hex: String,
    },
    Color(DissectColor),
    /// A value fillyfoal has and this build doesn't structure: `text` only.
    Other,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DissectRadix {
    Dec,
    Hex,
    Oct,
    Bin,
}

/// One entry of a flag table.
#[derive(Serialize, specta::Type, Clone)]
pub struct DissectFlag {
    mask: String,
    value: String,
    name: &'static str,
    set: bool,
    /// A multi-bit field holding a number (an offset, a count), never set:
    /// its value is `raw & mask`, shifted down.
    number: bool,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DissectZone {
    Utc,
    /// The file recorded its wall clock this many minutes east of UTC.
    Offset {
        minutes: i16,
    },
    /// A wall clock in a zone the file doesn't record: never converted.
    Local,
    Other,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DissectRes {
    Day,
    Minute,
    Second,
    /// Fractions of a second, to this many digits.
    Sub {
        digits: u8,
    },
    Other,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DissectGuidOrder {
    /// Microsoft's: the first three fields little-endian.
    Mixed,
    /// RFC 4122's.
    Big,
    Other,
}

#[derive(Serialize, specta::Type, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DissectVerdict {
    Valid,
    Mismatch {
        /// What the data gives, as the format would write it, when known.
        computed: Option<String>,
    },
    /// The dissector doesn't compute this check.
    NotChecked,
    Skipped {
        reason: String,
    },
    /// Verified where the data is decoded; a mismatch is reported there.
    Deferred,
    Other,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DissectAddrFamily {
    #[serde(rename = "ipv4")]
    Ipv4,
    #[serde(rename = "ipv6")]
    Ipv6,
    Mac,
    #[serde(rename = "eui64")]
    Eui64,
    Other,
}

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectColor {
    model: DissectColorModel,
    /// The model's components, normalised to 0..1 (Lab in its own ranges:
    /// L 0..100, a and b about ±128).
    components: Vec<f64>,
    depth: DissectColorDepth,
    alpha: Option<DissectAlpha>,
    space: DissectColorSpace,
    /// A spot colour's or swatch's name.
    name: Option<String>,
    /// 8-bit sRGB `rrggbbaa`, straight alpha; `None` when a component
    /// isn't a number.
    swatch: Option<String>,
    /// Whether `swatch` is the colour, not a preview of it (CMYK, Lab,
    /// device values, clipped wide-gamut colours).
    exact: bool,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DissectColorModel {
    Rgb,
    Gray,
    Cmyk,
    Lab,
    Hsb,
    Other,
}

#[derive(Serialize, specta::Type, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DissectColorDepth {
    /// Unsigned integers of this many bits each.
    Bits {
        bits: u8,
    },
    Float {
        bits: u8,
    },
    /// Bit fields packed into one number of `bits`, laid out as `layout`
    /// ("RGB565").
    Packed {
        raw: String,
        bits: u8,
        layout: &'static str,
    },
    Other,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
pub struct DissectAlpha {
    value: f64,
    premultiplied: bool,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum DissectColorSpace {
    Srgb,
    SrgbLinear,
    #[serde(rename = "display-p3")]
    DisplayP3,
    #[serde(rename = "rec2020")]
    Rec2020,
    Device,
    Other,
}

/// Bits of a signed value as an unsigned number of that width.
fn twos_complement(value: i64, bits: u8) -> u64 {
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    value as u64 & mask
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

impl From<Radix> for DissectRadix {
    fn from(radix: Radix) -> Self {
        match radix {
            Radix::Dec => Self::Dec,
            Radix::Hex => Self::Hex,
            Radix::Oct => Self::Oct,
            Radix::Bin => Self::Bin,
        }
    }
}

impl From<&FlagDef> for DissectFlag {
    fn from(def: &FlagDef) -> Self {
        DissectFlag {
            mask: def.mask.to_string(),
            value: def.value.to_string(),
            name: def.name,
            set: false,
            number: def.is_number(),
        }
    }
}

impl From<&Verdict> for DissectVerdict {
    fn from(verdict: &Verdict) -> Self {
        match verdict {
            Verdict::Valid => Self::Valid,
            Verdict::Mismatch { computed } => Self::Mismatch {
                computed: computed.as_ref().map(Digest::to_string),
            },
            Verdict::NotChecked => Self::NotChecked,
            Verdict::Skipped(why) => Self::Skipped {
                reason: why.to_string(),
            },
            Verdict::Deferred => Self::Deferred,
            _ => Self::Other,
        }
    }
}

impl From<&Color> for DissectColor {
    fn from(c: &Color) -> Self {
        let depth = match c.depth {
            ColorDepth::Bits(bits) => DissectColorDepth::Bits { bits },
            ColorDepth::Float(bits) => DissectColorDepth::Float { bits },
            ColorDepth::Packed { raw, bits, layout } => DissectColorDepth::Packed {
                raw: format!("{raw:#x}"),
                bits,
                layout,
            },
            _ => DissectColorDepth::Other,
        };
        let model = match c.model {
            ColorModel::Rgb => DissectColorModel::Rgb,
            ColorModel::Gray => DissectColorModel::Gray,
            ColorModel::Cmyk => DissectColorModel::Cmyk,
            ColorModel::Lab => DissectColorModel::Lab,
            ColorModel::Hsb => DissectColorModel::Hsb,
            _ => DissectColorModel::Other,
        };
        let swatch = c.to_srgb8();
        DissectColor {
            model,
            components: c.components[..c.model.components()].to_vec(),
            depth,
            alpha: c.alpha.map(|a| DissectAlpha {
                value: a.value,
                premultiplied: a.premultiplied,
            }),
            space: match c.space {
                ColorSpace::Srgb => DissectColorSpace::Srgb,
                ColorSpace::LinearSrgb => DissectColorSpace::SrgbLinear,
                ColorSpace::DisplayP3 => DissectColorSpace::DisplayP3,
                ColorSpace::Rec2020 => DissectColorSpace::Rec2020,
                ColorSpace::Device => DissectColorSpace::Device,
                _ => DissectColorSpace::Other,
            },
            name: c.name.as_ref().map(|n| n.to_string()),
            swatch: swatch.map(|(px, _)| hex(&px)),
            exact: swatch.is_some_and(|(_, e)| e == Exactness::Exact),
        }
    }
}

impl From<&Value> for DissectValue {
    fn from(value: &Value) -> Self {
        use DissectValueData as D;
        let (raw, data) = match value {
            Value::Bool(_) => (None, D::Bool),
            Value::UInt {
                value: n,
                bits,
                radix,
            } => (
                Some(match radix {
                    Radix::Dec => format!("{n:#x}"),
                    _ => n.to_string(),
                }),
                D::Number {
                    value: n.to_string(),
                    bits: *bits,
                    signed: false,
                    radix: (*radix).into(),
                },
            ),
            Value::Int { value: n, bits } => (
                Some(format!("{:#x}", twos_complement(*n, *bits))),
                D::Number {
                    value: n.to_string(),
                    bits: *bits,
                    signed: true,
                    radix: DissectRadix::Dec,
                },
            ),
            Value::Enum { raw, bits, name } => (
                Some(format!("{raw:#x}")),
                D::Enum {
                    raw: raw.to_string(),
                    bits: *bits,
                    known: name.is_some(),
                },
            ),
            Value::Flags { raw, bits, table } => {
                let (_, unknown) = fillyfoal::value::decode_flags(table, *raw);
                (
                    Some(format!("{raw:#x}")),
                    D::Flags {
                        raw: raw.to_string(),
                        bits: *bits,
                        unknown: unknown.to_string(),
                        table: table
                            .iter()
                            .map(|def| DissectFlag {
                                set: def.matches(*raw),
                                ..def.into()
                            })
                            .collect(),
                    },
                )
            }
            Value::Float { bits, .. } => (None, D::Float { bits: *bits }),
            Value::Timestamp(t) => (
                Some(t.secs.to_string()),
                D::Time {
                    secs: t.secs.to_string(),
                    nanos: t.nanos,
                    zone: match t.zone {
                        Zone::Utc => DissectZone::Utc,
                        Zone::Offset(minutes) => DissectZone::Offset { minutes },
                        Zone::Local => DissectZone::Local,
                        _ => DissectZone::Other,
                    },
                    res: match t.res {
                        Res::Day => DissectRes::Day,
                        Res::Minute => DissectRes::Minute,
                        Res::Second => DissectRes::Second,
                        Res::Sub(digits) => DissectRes::Sub { digits },
                        _ => DissectRes::Other,
                    },
                },
            ),
            Value::Text(s) => (None, D::Text { value: s.clone() }),
            Value::Bytes(b) => (
                Some(format!("{} bytes", b.len())),
                D::Bytes { len: b.len() },
            ),
            Value::Guid(g) => (
                None,
                D::Guid {
                    order: match g.order {
                        GuidOrder::Mixed => DissectGuidOrder::Mixed,
                        GuidOrder::Big => DissectGuidOrder::Big,
                        _ => DissectGuidOrder::Other,
                    },
                    uuid: g.uuid_text(),
                },
            ),
            Value::Checksum(c) => (
                None,
                D::Checksum {
                    algo: c.algo.to_string(),
                    stored: c.stored.to_string(),
                    bits: match c.stored {
                        Digest::Int { bits, .. } => Some(bits),
                        Digest::Bytes(_) => None,
                    },
                    verdict: (&c.verdict).into(),
                },
            ),
            Value::Size { n, unit, bits } => (
                Some(format!("{n:#x}")),
                D::Size {
                    n: n.to_string(),
                    unit: match unit {
                        SizeUnit::Bytes => Some(1),
                        SizeUnit::Blocks(b) => Some(*b),
                        _ => None,
                    },
                    bytes: value.size_bytes().map(|b| b.to_string()),
                    bits: *bits,
                },
            ),
            Value::Duration { ticks, per_sec } => (
                None,
                D::Duration {
                    ticks: ticks.to_string(),
                    per_sec: per_sec.to_string(),
                },
            ),
            Value::Addr(a) => (
                None,
                D::Addr {
                    family: match a {
                        Addr::V4(_) => DissectAddrFamily::Ipv4,
                        Addr::V6(_) => DissectAddrFamily::Ipv6,
                        Addr::Mac(_) => DissectAddrFamily::Mac,
                        Addr::Eui64(_) => DissectAddrFamily::Eui64,
                        _ => DissectAddrFamily::Other,
                    },
                    hex: hex(a.bytes()),
                },
            ),
            Value::Color(c) => (None, D::Color(c.as_ref().into())),
            _ => (None, D::Other),
        };
        let text = match value {
            Value::Enum { name, .. } => name.as_deref().unwrap_or("unknown").to_string(),
            Value::Flags { raw, table, .. } => {
                let (set, unknown) = fillyfoal::value::decode_flags(table, *raw);
                let mut parts: Vec<String> = set.iter().map(|s| s.to_string()).collect();
                if unknown != 0 {
                    parts.push(format!("{unknown:#x}"));
                }
                if parts.is_empty() {
                    "none".to_string()
                } else {
                    parts.join(" | ")
                }
            }
            Value::Text(s) => s.escape_debug().to_string(),
            _ => fillyfoal::render::value(value),
        };
        DissectValue { text, raw, data }
    }
}

#[cfg(test)]
mod tests {
    use fillyfoal::{Addr, Check, Color, Value};
    use serde_json::{Value as Json, json};

    use super::DissectValue;

    fn wire(value: Value) -> Json {
        serde_json::to_value(DissectValue::from(&value)).unwrap()
    }

    #[test]
    fn kinds_are_tagged_inline() {
        let colour = wire(Color::rgba8(255, 136, 0, 128).into());
        assert_eq!(colour["data"]["kind"], "color");
        assert_eq!(colour["data"]["model"], "rgb");
        assert_eq!(
            colour["data"]["depth"],
            json!({ "kind": "bits", "bits": 8 })
        );
        assert_eq!(colour["data"]["swatch"], "ff880080");
        assert_eq!(colour["data"]["exact"], true);

        let check = wire(Check::new("CRC-32", 0xae42_6082u32).verify(1u32).into());
        assert_eq!(check["data"]["stored"], "0xae426082");
        assert_eq!(check["data"]["bits"], 32);
        assert_eq!(
            check["data"]["verdict"],
            json!({ "kind": "mismatch", "computed": "0x1" })
        );

        let addr = wire(Addr::V4([192, 168, 0, 1]).into());
        assert_eq!(
            addr["data"],
            json!({ "kind": "addr", "family": "ipv4", "hex": "c0a80001" })
        );

        assert_eq!(wire(Value::Bool(true))["data"], json!({ "kind": "bool" }));
    }
}
