//! In-file content search: the `SearchPattern`/`SearchMatch` wire types
//! for the `find_in_file` verb, and the chunked literal/regex byte scanner
//! that serves it.

use crate::Error;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum SearchPattern {
    Literal(Vec<u8>),
    Regex(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SearchMatch {
    pub offset: u64,
    pub length: u64,
}

pub(crate) const SEARCH_CHUNK_SIZE: usize = 256 * 1024;

/// Maximum bytes carried over between search chunks for regex patterns. The
/// regex engine has no way to bound match length up front, so we have to
/// guess; 64 KiB covers any realistic regex while keeping the per-chunk
/// re-scan small.
const REGEX_OVERLAP_LIMIT: usize = 64 * 1024;

pub(crate) fn compute_overlap(pattern: &SearchPattern) -> usize {
    match pattern {
        SearchPattern::Literal(pat) => pat.len().saturating_sub(1),
        SearchPattern::Regex(_) => std::cmp::min(REGEX_OVERLAP_LIMIT, SEARCH_CHUNK_SIZE / 2),
    }
}

pub(crate) fn find_in_buffer(
    buf: &[u8],
    pattern: &SearchPattern,
    compiled_regex: Option<&regex::bytes::Regex>,
) -> Option<(usize, usize)> {
    match pattern {
        SearchPattern::Literal(pat) => memchr::memmem::find(buf, pat).map(|pos| (pos, pat.len())),
        SearchPattern::Regex(_) => compiled_regex?.find(buf).map(|m| (m.start(), m.len())),
    }
}

pub(crate) fn compile_regex(pattern: &SearchPattern) -> Result<Option<regex::bytes::Regex>, Error> {
    match pattern {
        SearchPattern::Regex(pat) => {
            let re = regex::bytes::Regex::new(pat).map_err(|e| Error::custom(e.to_string()))?;
            Ok(Some(re))
        }
        _ => Ok(None),
    }
}
