use std::sync::Arc;

use crate::Error;

use super::origin::build_origin_meta;
use super::{Vfs, VfsPath};

mod stream;
mod tree;

mod compressed;
mod sevenz;
mod tar;
mod zip;

pub use self::compressed::CompressedFileVfs;
pub use self::sevenz::SevenZArchiveVfs;
pub use self::tar::TarArchiveVfs;
pub use self::zip::ZipArchiveVfs;

/// Which reader a `MountRequest::Archive` opens the file with. The host
/// picks it from its file associations; the mount never second-guesses
/// it from the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Zip,
    #[serde(rename = "7z")]
    SevenZ,
    /// tar, cpio or ar, plain or compressed.
    Tar,
    /// A single compressed file: gzip, bzip2, xz or zstd.
    Compressed,
}

/// Build an archive VFS from a `MountRequest::Archive`. Resolves the
/// upstream VFS holding the archive bytes via the registry and opens it
/// with the reader `format` names. The archive's display path (origin rendered through the upstream's
/// `format_path`) is stamped into `mount_meta` so the mounted VFS keeps
/// a stable label even after the origin is unmounted.
///
/// For ZIP archives the mount itself never prompts: the central
/// directory is always cleartext, so listing always works. The askpass
/// provider is plumbed into the mounted VFS so reading an encrypted
/// entry can prompt lazily and cache the password for subsequent reads.
/// A 7z archive with an encrypted header prompts at mount time instead,
/// since nothing can be listed without the password.
pub async fn mount(
    origin: VfsPath,
    format: ArchiveFormat,
    ctx: &crate::vfs::mount::MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    log::info!("mounting archive VFS for origin={}", origin);
    let (upstream_vfs, archive_path) = ctx.registry.resolve(&origin)?;
    let (mount_meta, display_path) = build_origin_meta(upstream_vfs.as_ref(), &origin);

    let vfs: Arc<dyn Vfs> = match format {
        ArchiveFormat::Zip => Arc::new(ZipArchiveVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            display_path,
            ctx.askpass_provider.cloned(),
            ctx.progress_reporter.clone(),
        )),
        ArchiveFormat::SevenZ => Arc::new(SevenZArchiveVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            display_path,
            ctx.askpass_provider.cloned(),
            ctx.progress_reporter.clone(),
        )),
        ArchiveFormat::Tar => Arc::new(TarArchiveVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            ctx.progress_reporter.clone(),
        )),
        ArchiveFormat::Compressed => Arc::new(CompressedFileVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            display_path,
            ctx.progress_reporter.clone(),
        )),
    };
    Ok(vfs)
}

// ---------------------------------------------------------------------------
// Archive format detection
// ---------------------------------------------------------------------------

/// Suffixes of a bare compressed file, which mounts as a one-entry
/// filesystem; the same suffixes on a tar or cpio name mean the container.
const COMPRESSED_EXTENSIONS: &[&str] = &["gz", "bz2", "xz", "zst", "zstd"];

/// The compression a tar-family archive or a compressed file is stored
/// with: from its name when the name says, else from its first bytes, so
/// a file browsed under a name no extension describes still decompresses.
async fn detect_compression(
    upstream: &Arc<dyn Vfs>,
    path: &super::path::Path,
) -> Result<iluvatar::CompressionFormat, Error> {
    Ok(match detect_compression_from_name(path.as_wire_str()) {
        iluvatar::CompressionFormat::None => {
            compression_from_magic(&upstream.read_range(path, 0, 6).await?.data)
        }
        named => named,
    })
}

fn compression_from_magic(header: &[u8]) -> iluvatar::CompressionFormat {
    use iluvatar::CompressionFormat::*;
    match header {
        [0x1f, 0x8b, ..] => Gzip,
        [b'B', b'Z', b'h', ..] => Bzip2,
        [0xfd, b'7', b'z', b'X', b'Z', 0x00, ..] => Xz,
        [0x28, 0xb5, 0x2f, 0xfd, ..] => Zstd,
        _ => None,
    }
}

/// The archive reader a file's first bytes call for, or `None` when
/// nothing there is recognisable. A compressed stream reads as a single
/// compressed file: what is inside is unknown until it is decompressed.
pub fn sniff_format(header: &[u8]) -> Option<ArchiveFormat> {
    match header {
        [b'P', b'K', 3, 4, ..] | [b'P', b'K', 5, 6, ..] | [b'P', b'K', 7, 8, ..] => {
            Some(ArchiveFormat::Zip)
        }
        [b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c, ..] => Some(ArchiveFormat::SevenZ),
        [b'!', b'<', b'a', b'r', b'c', b'h', b'>', b'\n', ..] => Some(ArchiveFormat::Tar),
        [b'0', b'7', b'0', b'7', b'0', b'1' | b'2' | b'7', ..] => Some(ArchiveFormat::Tar),
        _ if header.get(257..262) == Some(b"ustar") => Some(ArchiveFormat::Tar),
        _ if compression_from_magic(header) != iluvatar::CompressionFormat::None => {
            Some(ArchiveFormat::Compressed)
        }
        _ => None,
    }
}

/// Detect compression format from filename extension.
fn detect_compression_from_name(name: &str) -> iluvatar::CompressionFormat {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".gz") || lower.ends_with(".tgz") {
        iluvatar::CompressionFormat::Gzip
    } else if lower.ends_with(".bz2") || lower.ends_with(".tbz2") || lower.ends_with(".tbz") {
        iluvatar::CompressionFormat::Bzip2
    } else if lower.ends_with(".xz") || lower.ends_with(".txz") {
        iluvatar::CompressionFormat::Xz
    } else if lower.ends_with(".zst") || lower.ends_with(".zstd") || lower.ends_with(".tzst") {
        iluvatar::CompressionFormat::Zstd
    } else {
        iluvatar::CompressionFormat::None
    }
}

#[cfg(test)]
mod name_tests {
    use super::{ArchiveFormat, compression_from_magic, sniff_format};
    use iluvatar::CompressionFormat;

    #[test]
    fn compressed_entry_names_drop_the_suffix() {
        use super::compressed::entry_name;
        assert_eq!(entry_name("notes.txt.gz"), "notes.txt");
        assert_eq!(entry_name("disk.img.XZ"), "disk.img");
        assert_eq!(entry_name(".gz"), ".gz");
        assert_eq!(entry_name("plain"), "plain");
    }

    #[test]
    fn compression_is_sniffed_from_magic() {
        assert_eq!(
            compression_from_magic(&[0x1f, 0x8b, 8]),
            CompressionFormat::Gzip
        );
        assert_eq!(compression_from_magic(b"BZh91AY"), CompressionFormat::Bzip2);
        assert_eq!(
            compression_from_magic(&[0xfd, b'7', b'z', b'X', b'Z', 0]),
            CompressionFormat::Xz
        );
        assert_eq!(
            compression_from_magic(&[0x28, 0xb5, 0x2f, 0xfd]),
            CompressionFormat::Zstd
        );
        assert_eq!(compression_from_magic(b"ustar"), CompressionFormat::None);
        assert_eq!(compression_from_magic(&[]), CompressionFormat::None);
    }

    #[test]
    fn archive_formats_are_sniffed_from_magic() {
        assert_eq!(sniff_format(b"PK\x03\x04rest"), Some(ArchiveFormat::Zip));
        assert_eq!(sniff_format(b"PK\x05\x06"), Some(ArchiveFormat::Zip));
        assert_eq!(
            sniff_format(&[b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c, 0]),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(sniff_format(b"!<arch>\ndebian"), Some(ArchiveFormat::Tar));
        assert_eq!(sniff_format(b"070701000"), Some(ArchiveFormat::Tar));
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(sniff_format(&tar), Some(ArchiveFormat::Tar));
        assert_eq!(
            sniff_format(&[0x1f, 0x8b, 8, 0]),
            Some(ArchiveFormat::Compressed)
        );
        assert_eq!(sniff_format(b"plain text, not an archive"), None);
        assert_eq!(sniff_format(&[0u8; 512]), None);
    }

    // ---------------------------------------------------------------------------
    // VfsPath
    // ---------------------------------------------------------------------------
}
