//! Batch rename: one find & replace rule over a selection, previewed on
//! the host, performed as an operation next to the filesystem.

pub mod plan;
pub mod rule;

use caseless::Caseless;
use unicode_normalization::UnicodeNormalization;

/// Names that might be one file somewhere: Unicode's canonical caseless
/// key (NFD, full case folding, NFD). A superset of what APFS, HFS+, NTFS
/// and ext4 casefold equate — never the decision, which is the
/// filesystem's (`Vfs::same_file`), only what to be careful about.
pub fn fold(name: &str) -> String {
    name.nfd().default_case_fold().nfd().collect()
}

#[cfg(test)]
mod tests {
    use super::fold;

    #[test]
    fn fold_equates_case_and_normalization() {
        assert_eq!(fold("README"), fold("readme"));
        assert_eq!(fold("Straße"), fold("STRASSE"));
        // NFC `é` and NFD `e` + combining acute.
        assert_eq!(fold("caf\u{e9}"), fold("cafe\u{301}"));
        assert_ne!(fold("a"), fold("b"));
    }
}
