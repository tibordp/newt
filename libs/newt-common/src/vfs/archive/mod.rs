use std::sync::Arc;

use crate::Error;

use super::origin::build_origin_meta;
use super::{Vfs, VfsPath};

mod tree;

mod tar;
mod zip;

pub use self::tar::TarArchiveVfs;
pub use self::zip::ZipArchiveVfs;

/// Build an archive VFS from a `MountRequest::Archive`. Resolves the
/// upstream VFS holding the archive bytes via the registry and picks
/// `ZipArchiveVfs` or `TarArchiveVfs` based on the file extension. The
/// archive's display path (origin rendered through the upstream's
/// `format_path`) is stamped into `mount_meta` so the mounted VFS keeps
/// a stable label even after the origin is unmounted.
///
/// For ZIP archives the mount itself never prompts: the central
/// directory is always cleartext, so listing always works. The askpass
/// provider is plumbed into the mounted VFS so reading an encrypted
/// entry can prompt lazily and cache the password for subsequent reads.
pub async fn mount(
    origin: VfsPath,
    ctx: &crate::vfs::mount::MountContext<'_>,
) -> Result<Arc<dyn Vfs>, Error> {
    log::info!("mounting archive VFS for origin={}", origin);
    let (upstream_vfs, archive_path) = ctx.registry.resolve(&origin)?;
    let (mount_meta, display_path) = build_origin_meta(upstream_vfs.as_ref(), &origin);

    let vfs: Arc<dyn Vfs> = if is_zip_name(archive_path.as_wire_str()) {
        Arc::new(ZipArchiveVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            display_path,
            ctx.askpass_provider.cloned(),
            ctx.progress_reporter.clone(),
        ))
    } else {
        Arc::new(TarArchiveVfs::new(
            upstream_vfs,
            archive_path,
            origin,
            mount_meta,
            ctx.progress_reporter.clone(),
        ))
    };
    Ok(vfs)
}

// ---------------------------------------------------------------------------
// Archive format detection
// ---------------------------------------------------------------------------

const TAR_EXTENSIONS: &[&str] = &[
    "tar", "tar.gz", "tgz", "tar.bz2", "tbz2", "tbz", "tar.xz", "txz", "tar.zst", "tzst",
    "tar.zstd", "cpio", "cpio.gz", "cpio.bz2", "cpio.xz", "cpio.zst",
];

const ZIP_EXTENSIONS: &[&str] = &["zip", "jar", "war", "ear", "apk", "ipa"];

pub fn is_archive_name(name: &str) -> bool {
    is_tar_name(name) || is_zip_name(name)
}

fn is_tar_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    TAR_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
}

pub fn is_zip_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ZIP_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
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
    use super::{is_archive_name, is_zip_name};

    #[test]
    fn is_archive_name_tar_variants() {
        assert!(is_archive_name("file.tar"));
        assert!(is_archive_name("file.tar.gz"));
        assert!(is_archive_name("file.tgz"));
        assert!(is_archive_name("file.tar.bz2"));
        assert!(is_archive_name("file.tar.xz"));
        assert!(is_archive_name("file.tar.zst"));
    }

    #[test]
    fn is_archive_name_zip_variants() {
        assert!(is_zip_name("file.zip"));
        assert!(is_zip_name("app.jar"));
        assert!(is_zip_name("deploy.war"));
        assert!(is_zip_name("app.apk"));
    }

    #[test]
    fn is_archive_name_case_insensitive() {
        assert!(is_archive_name("FILE.TAR.GZ"));
        assert!(is_zip_name("FILE.ZIP"));
    }

    #[test]
    fn is_archive_name_non_archive() {
        assert!(!is_archive_name("file.txt"));
        assert!(!is_archive_name("file.rs"));
        assert!(!is_archive_name("tarfile"));
    }

    // ---------------------------------------------------------------------------
    // VfsPath
    // ---------------------------------------------------------------------------
}
