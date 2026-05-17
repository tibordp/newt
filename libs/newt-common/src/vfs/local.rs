use std::io::{Read, Write};
use std::path::{Path as StdPath, PathBuf as StdPathBuf};
use std::sync::Arc;

use crate::vfs::path::{Path, PathBuf};

use log::{debug, warn};
use notify::event::RemoveKind;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;

#[cfg(unix)]
use std::os::unix::prelude::MetadataExt;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode, UidGidCache, UserGroup};
use crate::{Error, ToUnix};

use super::{
    Breadcrumb, DisplayPathMatch, PathStyle, RegisteredDescriptor, Vfs, VfsDescriptor, VfsMetadata,
    VfsSpaceInfo,
};

/// Bytes read from a file head when sniffing for a MIME type without an
/// extension match. Bigger reads catch more formats but cost more I/O per
/// directory listing entry; 8 KiB is enough for every magic-number signature
/// in `mimetype-detector` while staying inside one filesystem block.
const MIME_SNIFF_BUFFER_SIZE: usize = 8192;

/// Files-per-batch streamed to the host during a directory listing. Smaller
/// batches reduce first-paint latency on huge directories; larger batches
/// reduce IPC overhead. 500 lands in the sweet spot for both.
const LIST_FILES_BATCH_SIZE: usize = 500;

// ---------------------------------------------------------------------------
// LocalVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LocalVfsDescriptor;

impl VfsDescriptor for LocalVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "local"
    }
    fn display_name(&self) -> &'static str {
        "Local"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read_sync(&self) -> bool {
        true
    }
    fn can_read_async(&self) -> bool {
        false
    }
    fn can_overwrite_sync(&self) -> bool {
        true
    }
    fn can_overwrite_async(&self) -> bool {
        false
    }
    fn can_create_directory(&self) -> bool {
        true
    }
    fn can_create_symlink(&self) -> bool {
        true
    }
    fn can_touch(&self) -> bool {
        true
    }
    fn can_truncate(&self) -> bool {
        true
    }
    fn can_set_metadata(&self) -> bool {
        true
    }
    fn can_remove(&self) -> bool {
        true
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_stat_directories(&self) -> bool {
        true
    }
    fn can_fs_stats(&self) -> bool {
        true
    }
    fn can_rename(&self) -> bool {
        true
    }
    fn can_copy_within(&self) -> bool {
        true
    }
    fn can_hard_link(&self) -> bool {
        true
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        local_display_path(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        local_breadcrumbs(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn navigable_parent(&self, path: &Path, mount_meta: &[u8]) -> Option<PathBuf> {
        navigable_parent(path, PathStyle::from_mount_meta(mount_meta))
    }

    fn try_parse_display_path(&self, _input: &str, _mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        None
    }

    fn roots(&self, mount_meta: &[u8]) -> Vec<PathBuf> {
        roots_from_meta(mount_meta)
    }
    // `has_unified_root` / `initial_path` use the trait defaults, which
    // are now roots-aware (split-root → land on the first drive).
}

/// FS roots from `mount_meta`, defaulting to a single `/` when none were
/// recorded (a style-only mount, e.g. a remote Unix root). Shared by
/// `LocalVfsDescriptor` and `RemoteVfsDescriptor`.
pub fn roots_from_meta(mount_meta: &[u8]) -> Vec<PathBuf> {
    let roots = super::mount_roots(mount_meta);
    if roots.is_empty() {
        vec![PathBuf::root()]
    } else {
        roots
    }
}

/// Logical parent of a LocalVfs path, honouring Windows drive/share
/// roots. Shared by `LocalVfsDescriptor` and `RemoteVfsDescriptor` (the
/// path shape is identical; only the `mount_meta`-derived style differs).
pub fn navigable_parent(path: &Path, style: PathStyle) -> Option<PathBuf> {
    match style {
        PathStyle::Unix => path.parent().map(Path::to_owned),
        PathStyle::Windows => {
            // `/?`, `/?/C:`, and `/?/UNC/server/share` are all "roots".
            // Anything above them isn't a navigable location in our
            // current model (no "This PC" view, no "shares on server"
            // view), so refuse to go up past them.
            let comps: Vec<&str> = path.components().collect();
            // A sentinel-less path isn't a real Windows path; treat it
            // like Unix rather than misapplying drive-root rules.
            if comps.first().copied() != Some("?") {
                return path.parent().map(Path::to_owned);
            }
            let root_depth = match comps.get(1).copied() {
                Some("UNC") => 4,
                Some(_) => 2,
                None => return None,
            };
            if comps.len() <= root_depth {
                None
            } else {
                Some(PathBuf::from_components(
                    comps[..comps.len() - 1].iter().copied(),
                ))
            }
        }
    }
}

/// Root paths of the local filesystem, enumerated once on the side that
/// owns it (the host for a local session, the agent for a remote one;
/// also the host for its FS exposed into a remote session). Unix has the
/// single `/`; Windows has one per logical drive (`\\?\C:`, …). Baked
/// into `mount_meta` at mount time — a drive added afterwards needs a
/// restart, the accepted tradeoff for keeping this RPC-free.
#[cfg(unix)]
pub fn local_roots() -> Vec<PathBuf> {
    vec![PathBuf::root()]
}

#[cfg(windows)]
pub fn local_roots() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDriveStringsW;

    // First call with a zero length returns the required buffer size.
    let needed = unsafe { GetLogicalDriveStringsW(0, std::ptr::null_mut()) };
    if needed == 0 {
        return vec![PathBuf::root()];
    }
    let mut buf = vec![0u16; needed as usize];
    let written = unsafe { GetLogicalDriveStringsW(buf.len() as u32, buf.as_mut_ptr()) };
    if written == 0 {
        return vec![PathBuf::root()];
    }
    // Buffer is a sequence of NUL-terminated `X:\` strings, double-NUL
    // terminated. Decode each into the `["?","X:"]` sentinel form.
    let roots: Vec<PathBuf> = buf[..written as usize]
        .split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(|s| local_path_from_native(StdPath::new(&String::from_utf16_lossy(s))))
        .collect();
    if roots.is_empty() {
        vec![PathBuf::root()]
    } else {
        roots
    }
}

pub static LOCAL_VFS_DESCRIPTOR: LocalVfsDescriptor = LocalVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&LOCAL_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// LocalVfs — wraps existing filesystem::Local + file_reader::Local logic
// ---------------------------------------------------------------------------

pub struct LocalVfs {
    fs_cache: Arc<crate::filesystem::UidGidCache>,
}

impl LocalVfs {
    pub fn new() -> Self {
        Self {
            fs_cache: Arc::new(crate::filesystem::UidGidCache::new()),
        }
    }
}

impl Default for LocalVfs {
    fn default() -> Self {
        Self::new()
    }
}

/// Render LocalVfs path components into the conventional host display
/// form.
///
/// * Unix: `["Users", "tibor"]` → `/Users/tibor`.
/// * Windows: strips the `"?"` sentinel — `["?", "C:", "Users", "Tibor"]`
///   → `C:\Users\Tibor`; `["?", "UNC", "server", "share", "foo"]` →
///   `\\server\share\foo`.
fn comps_display(comps: &[&str], style: PathStyle) -> String {
    match style {
        PathStyle::Unix => {
            if comps.is_empty() {
                String::from("/")
            } else {
                format!("/{}", comps.join("/"))
            }
        }
        PathStyle::Windows => {
            // `[]` and `["?"]` both correspond to the "above any
            // drive/share" position (`\\?\`). Navigation rules normally
            // prevent landing here — see `navigable_parent` — but render
            // something defensively rather than panic.
            if comps.is_empty() || (comps.len() == 1 && comps[0] == "?") {
                return String::from(r"\\?\");
            }
            match comps[0] {
                "?" => {
                    if comps.len() >= 2 && comps[1] == "UNC" {
                        let mut s = String::from(r"\\");
                        s.push_str(&comps[2..].join(r"\"));
                        s
                    } else {
                        let mut s = comps[1].to_string();
                        if comps.len() > 2 {
                            s.push('\\');
                            s.push_str(&comps[2..].join(r"\"));
                        } else {
                            // Bare drive root: `C:\`, not `C:`.
                            s.push('\\');
                        }
                        s
                    }
                }
                // Defensive fallback for non-sentinel components.
                _ => comps.join(r"\"),
            }
        }
    }
}

/// User-facing rendering of a LocalVfs path. See [`comps_display`].
pub fn local_display_path(path: &Path, style: PathStyle) -> String {
    let comps: Vec<&str> = path.components().collect();
    comps_display(&comps, style)
}

/// Breadcrumbs for a LocalVfs path. Each breadcrumb's `nav_path` is the
/// display form of the path up to that segment, suitable for the
/// path-input dialog.
pub fn local_breadcrumbs(path: &Path, style: PathStyle) -> Vec<Breadcrumb> {
    if style == PathStyle::Unix {
        return super::unix_breadcrumbs(path);
    }
    let comps: Vec<&str> = path.components().collect();
    if comps.first().copied() != Some("?") {
        // Defensive — render unstructured components Unix-style.
        return super::unix_breadcrumbs(path);
    }
    let mut crumbs = Vec::new();
    // Root depth: 2 (`?/C:`) for drives, 4 (`?/UNC/server/share`)
    // for UNC. The root crumb covers through that point.
    let root_depth = if comps.get(1).copied() == Some("UNC") {
        4
    } else {
        2
    };
    if comps.len() < root_depth {
        crumbs.push(Breadcrumb {
            label: comps_display(&comps, style),
            nav_path: comps_display(&comps, style),
        });
        return crumbs;
    }
    crumbs.push(Breadcrumb {
        label: comps_display(&comps[..root_depth], style),
        nav_path: comps_display(&comps[..root_depth], style),
    });
    for i in root_depth..comps.len() {
        let is_last = i + 1 == comps.len();
        let label = if is_last {
            comps[i].to_string()
        } else {
            format!("{}\\", comps[i])
        };
        crumbs.push(Breadcrumb {
            label,
            nav_path: comps_display(&comps[..i + 1], style),
        });
    }
    crumbs
}

/// Render a LocalVfs path into a host-native `std::path::PathBuf` safe to
/// feed to `std::fs` / `opener` / any Win32 or POSIX consumer.
///
/// * Unix: `/foo/bar` → `/foo/bar`.
/// * Windows: `/?/C:/Users/Tibor` → `\\?\C:\Users\Tibor`;
///   `/?/UNC/server/share/foo` → `\\?\UNC\server\share\foo`.
///
/// Native (`std::path`) form, so it must only ever run in the process
/// that owns the files — never across the RPC boundary. A `LocalVfs`
/// satisfies that: it executes on whichever side physically holds the FS
/// (the host locally, the agent in a remote session), each compiled for
/// its own platform.
pub fn to_native(path: &Path) -> StdPathBuf {
    #[cfg(windows)]
    {
        let comps: Vec<&str> = path.components().collect();
        let mut s = String::from(r"\\");
        for (i, c) in comps.iter().enumerate() {
            if i > 0 {
                s.push('\\');
            }
            s.push_str(c);
        }
        // `\\?\C:` / `\\?\UNC\server\share` name the *volume*, not its
        // root directory — `std::fs` and the change watcher reject those.
        // The root dir needs a trailing separator (`\\?\C:\`). Deeper
        // paths must NOT have one.
        if comps.first() == Some(&"?") {
            let root_depth = if comps.get(1) == Some(&"UNC") { 4 } else { 2 };
            if comps.len() == root_depth {
                s.push('\\');
            }
        }
        StdPathBuf::from(s)
    }
    #[cfg(not(windows))]
    {
        StdPathBuf::from(path.as_wire_str())
    }
}

/// Decode a host-native `std::path::Path` into LocalVfs path components.
///
/// * Unix: walks `Normal` components — `/home/user` → `["home", "user"]`.
/// * Windows: emits the `"?"` sentinel then drive (`["?", "C:", …]`) or
///   UNC (`["?", "UNC", "server", "share", …]`) info, then `Normal`
///   components. Verbatim (`\\?\…`) and conventional forms collapse to
///   the same components.
///
/// Used at the boundary between native-path APIs
/// (`std::env::current_dir`, `dirs::*`, drag-and-drop) and `VfsPath`.
pub fn local_path_from_native(path: &StdPath) -> PathBuf {
    PathBuf::from_components(local_segments_from_native(path))
}

pub fn local_segments_from_native(path: &StdPath) -> Vec<String> {
    use std::path::Component;

    let mut segments = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => segments.push(s.to_string_lossy().into_owned()),
            Component::Prefix(_prefix) => {
                #[cfg(windows)]
                {
                    use std::path::Prefix;
                    segments.push("?".to_string());
                    match _prefix.kind() {
                        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                            // `d` is the drive letter byte (e.g. `b'C'`).
                            segments.push(format!("{}:", char::from(d).to_ascii_uppercase()));
                        }
                        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                            segments.push("UNC".to_string());
                            segments.push(server.to_string_lossy().into_owned());
                            segments.push(share.to_string_lossy().into_owned());
                        }
                        Prefix::Verbatim(seg) => {
                            // `\\?\<seg>\…` for forms we don't specifically
                            // recognise (e.g. `Volume{GUID}`). Pass through
                            // verbatim so volume-GUID paths still work.
                            segments.push(seg.to_string_lossy().into_owned());
                        }
                        Prefix::DeviceNS(_) => {
                            // `\\.\…` device-namespace paths — outside the
                            // file-manager scope. Drop the prefix and hope
                            // the remaining segments are usable.
                        }
                    }
                }
            }
            // Drop `RootDir`, `CurDir`, `ParentDir`. Absolute paths land at
            // `RootDir` after the prefix (if any); `.` and `..` shouldn't
            // appear in a canonicalised path the caller hands us, and if
            // they do we drop them since segments are meant to be literal.
            _ => {}
        }
    }
    segments
}

#[async_trait::async_trait]
impl Vfs for LocalVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &LOCAL_VFS_DESCRIPTOR
    }

    /// A `LocalVfs` always serves the filesystem of whatever process it
    /// runs in — the host for a local session / client-local hairpin, the
    /// agent for a remote session. So it stamps this binary's host style
    /// and the roots enumerated here, carried to whichever side renders.
    fn mount_meta(&self) -> Vec<u8> {
        super::encode_mount_meta(PathStyle::host(), &local_roots())
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::VfsFileList, Error> {
        let path = to_native(path);
        let files: Vec<File> = tokio::task::spawn_blocking({
            let cache = self.fs_cache.clone();
            move || -> Result<Vec<File>, Error> {
                const BATCH_SIZE: usize = LIST_FILES_BATCH_SIZE;

                let mut ret = Vec::new();
                let mut batch = Vec::new();

                if let Some(parent) = path.parent() {
                    let metadata = parent.symlink_metadata()?;
                    let (mode_field, user_field, group_field) = unix_owner_bits(&metadata, &cache);
                    let file = File {
                        name: "..".to_string(),
                        size: None,
                        is_dir: true,
                        is_symlink: metadata.is_symlink(),
                        symlink_target: None,
                        is_hidden: false,
                        user: user_field,
                        group: group_field,
                        mode: mode_field,
                        modified: metadata.modified().map(|t| t.to_unix()).ok(),
                        accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                        created: metadata.created().map(|t| t.to_unix()).ok(),
                        key: None,
                        source: None,
                    };
                    batch.push(file.clone());
                    ret.push(file);
                }

                for maybe_entry in std::fs::read_dir(&path)? {
                    let entry = maybe_entry?;
                    let metadata = entry.metadata()?;
                    let file_type = metadata.file_type();

                    // Best-effort UTF-8 conversion: a non-UTF-8 filename gets
                    // U+FFFD replacement chars. The entry shows up in the UI
                    // but file ops on it (rename / delete / touch / etc.) will
                    // fail with NotFound — when the frontend echoes the name
                    // back, `path.join(&name)` builds a path with the
                    // replacements that doesn't exist on disk. Acceptable
                    // trade-off vs. panicking the entire listing.
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let mut is_dir = file_type.is_dir();

                    let symlink_target = if file_type.is_symlink() {
                        let target_metadata = std::fs::metadata(entry.path());
                        if let Ok(target_metadata) = target_metadata {
                            is_dir = target_metadata.is_dir();
                        }
                        std::fs::read_link(entry.path())
                            .ok()
                            .map(|t| t.to_string_lossy().into_owned())
                    } else {
                        None
                    };

                    let (mode_field, user_field, group_field) = unix_owner_bits(&metadata, &cache);
                    let file = File {
                        name: name.clone(),
                        size: (!is_dir).then_some(metadata.len()),
                        is_dir,
                        is_symlink: file_type.is_symlink(),
                        symlink_target,
                        is_hidden: is_hidden(&name, &metadata),
                        user: user_field,
                        group: group_field,
                        mode: mode_field,
                        modified: metadata.modified().map(|t| t.to_unix()).ok(),
                        accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                        created: metadata.created().map(|t| t.to_unix()).ok(),
                        key: None,
                        source: None,
                    };
                    batch.push(file.clone());
                    ret.push(file);

                    if batch.len() >= BATCH_SIZE {
                        if let Some(ref tx) = batch_tx {
                            if tx.blocking_send(std::mem::take(&mut batch)).is_err() {
                                // Receiver dropped — cancelled
                                return Ok(ret);
                            }
                        } else {
                            batch.clear();
                        }
                    }
                }

                // Send any remaining entries as a final batch
                if let Some(ref tx) = batch_tx
                    && !batch.is_empty()
                {
                    let _ = tx.blocking_send(batch);
                }

                Ok(ret)
            }
        })
        .await??;
        Ok(files.into())
    }

    async fn fs_stats(&self, path: &Path) -> Result<Option<FsStats>, Error> {
        let path = to_native(path);
        Ok(tokio::task::spawn_blocking(move || platform_fs_stats(&path)).await?)
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = Arc::new(Mutex::new(Some(tx)));

        let mut watcher = {
            let path = path.clone();
            RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    match res {
                        Ok(event) => {
                            let should_notify = match event.kind {
                                EventKind::Remove(RemoveKind::Folder) => event
                                    .paths
                                    .iter()
                                    .any(|p| path.starts_with(p) || p.starts_with(&path)),
                                EventKind::Access(_) => false,
                                _ => event.paths.iter().any(|p| p.starts_with(&path)),
                            };

                            if should_notify && let Some(s) = tx.lock().take() {
                                debug!("{:?} (while watching {})", event, path.display());
                                let _ = s.send(());
                            }
                        }
                        Err(e) => warn!("watch error: {:?}", e),
                    };
                },
                Config::default().with_follow_symlinks(false),
            )?
        };

        let mut watch_path = path;
        loop {
            watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;
            if !watch_path.pop() {
                break;
            }
        }

        let _ = rx.await;
        Ok(())
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let path = to_native(path);
        let cache = self.fs_cache.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;

            let symlink_meta = std::fs::symlink_metadata(&path)?;
            let is_symlink = symlink_meta.is_symlink();
            let symlink_target = if is_symlink {
                std::fs::read_link(&path)
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned())
            } else {
                None
            };

            let meta = if is_symlink {
                std::fs::metadata(&path).unwrap_or(symlink_meta)
            } else {
                symlink_meta
            };

            let is_dir = meta.is_dir();
            let size = meta.len();
            let (mode_field, user_field, group_field) = unix_owner_bits(&meta, &cache);

            // MIME detection for files: try extension first, then content sniffing
            let mime_type = if is_dir {
                None
            } else {
                let from_extension = crate::file_reader::guess_mime_type(&path);
                if from_extension.is_some() {
                    from_extension
                } else {
                    let file = std::fs::File::open(&path)?;
                    let mut buf = vec![0u8; MIME_SNIFF_BUFFER_SIZE.min(size as usize)];
                    let mut reader = std::io::BufReader::new(file);
                    let n = reader.read(&mut buf)?;
                    let header = &buf[..n];

                    let detected = mimetype_detector::detect(header);
                    if detected.is("application/octet-stream") {
                        // No specific match — fall back to null-byte heuristic
                        if !header.contains(&0) {
                            Some("text/plain".to_string())
                        } else {
                            Some("application/octet-stream".to_string())
                        }
                    } else {
                        Some(detected.mime().to_string())
                    }
                }
            };

            Ok(FileDetails {
                size,
                mime_type,
                is_dir,
                is_symlink,
                symlink_target,
                user: user_field,
                group: group_field,
                mode: mode_field,
                modified: meta.modified().map(|t| t.to_unix()).ok(),
                accessed: meta.accessed().map(|t| t.to_unix()).ok(),
                created: meta.created().map(|t| t.to_unix()).ok(),
            })
        })
        .await?
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&path)?;
            let total_size = file.metadata()?.len();
            file.seek(SeekFrom::Start(offset))?;
            // Don't cap at total_size — pseudo-files (procfs, sysfs) and
            // block devices report size 0 but have readable content.
            let to_read = length as usize;
            let mut data = vec![0u8; to_read];
            let mut total_read = 0;
            while total_read < to_read {
                let n = file.read(&mut data[total_read..])?;
                if n == 0 {
                    break;
                }
                total_read += n;
            }
            data.truncate(total_read);
            Ok(FileChunk {
                data,
                offset,
                total_size,
            })
        })
        .await?
    }

    async fn open_read_sync(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let path = to_native(path);
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::open(&path).map_err(Error::from))
                .await??;
        Ok(Box::new(file))
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let path = to_native(path);
        let cache = self.fs_cache.clone();
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            let is_symlink = meta.is_symlink();
            let symlink_target = if is_symlink {
                std::fs::read_link(&path)
                    .ok()
                    .map(|t| t.to_string_lossy().into_owned())
            } else {
                None
            };
            let mut is_dir = meta.is_dir();
            if is_symlink && let Ok(target_meta) = std::fs::metadata(&path) {
                is_dir = target_meta.is_dir();
            }
            let (mode_field, user_field, group_field) = unix_owner_bits(&meta, &cache);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            Ok(File {
                is_hidden: is_hidden(&name, &meta),
                name,
                size: (!is_dir).then_some(meta.len()),
                is_dir,
                is_symlink,
                symlink_target,
                user: user_field,
                group: group_field,
                mode: mode_field,
                modified: meta.modified().map(|t| t.to_unix()).ok(),
                accessed: meta.accessed().map(|t| t.to_unix()).ok(),
                created: meta.created().map(|t| t.to_unix()).ok(),
                key: None,
                source: None,
            })
        })
        .await?
    }

    async fn overwrite_sync(&self, path: &Path) -> Result<Box<dyn Write + Send>, Error> {
        let path = to_native(path);
        let file =
            tokio::task::spawn_blocking(move || std::fs::File::create(&path).map_err(Error::from))
                .await??;
        Ok(Box::new(file))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || std::fs::create_dir_all(&path).map_err(Error::from))
            .await?
    }

    async fn create_symlink(&self, link: &Path, target: &str) -> Result<(), Error> {
        let link = to_native(link);
        let target = target.to_string();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &link)?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = (link, target);
                Err(Error::not_supported())
            }
        })
        .await?
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            std::fs::remove_file(&path)?;
            Ok(())
        })
        .await?
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            std::fs::remove_dir(&path)?;
            Ok(())
        })
        .await?
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path)?;
            let (permissions, uid, gid) = unix_meta_ids(&meta);
            Ok(VfsMetadata {
                permissions,
                uid,
                gid,
                atime: meta.accessed().ok(),
                mtime: meta.modified().ok(),
            })
        })
        .await?
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        let path = to_native(path);
        let meta = meta.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                if let Some(permissions) = meta.permissions {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
                }

                let uid = meta.uid.map(nix::unistd::Uid::from_raw);
                let gid = meta.gid.map(nix::unistd::Gid::from_raw);
                if uid.is_some() || gid.is_some() {
                    nix::unistd::chown(&path, uid, gid)?;
                }

                if meta.atime.is_some() || meta.mtime.is_some() {
                    let current_meta = std::fs::metadata(&path)?;
                    let atime = meta.atime.map_or_else(
                        || filetime::FileTime::from_last_access_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    let mtime = meta.mtime.map_or_else(
                        || filetime::FileTime::from_last_modification_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    filetime::set_file_times(&path, atime, mtime)?;
                }
            }
            #[cfg(windows)]
            {
                // Local Windows builds don't surface POSIX mode/uid/gid bits
                // (`get_metadata` returns them as `None`), so we only honor
                // atime/mtime — everything else is a no-op.
                if meta.atime.is_some() || meta.mtime.is_some() {
                    let current_meta = std::fs::metadata(&path)?;
                    let atime = meta.atime.map_or_else(
                        || filetime::FileTime::from_last_access_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    let mtime = meta.mtime.map_or_else(
                        || filetime::FileTime::from_last_modification_time(&current_meta),
                        filetime::FileTime::from_system_time,
                    );
                    filetime::set_file_times(&path, atime, mtime)?;
                }
            }
            Ok(())
        })
        .await?
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = to_native(from);
        let to = to_native(to);
        tokio::task::spawn_blocking(move || std::fs::rename(&from, &to).map_err(Error::from))
            .await?
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)?;
            Ok(())
        })
        .await?
    }

    async fn available_space(&self, path: &Path) -> Result<VfsSpaceInfo, Error> {
        let path = to_native(path);
        tokio::task::spawn_blocking(move || platform_space_info(&path)).await?
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let from = to_native(from);
        let to = to_native(to);
        tokio::task::spawn_blocking(move || {
            // Try FICLONE (instant COW clone) first on Linux
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::io::AsRawFd;
                let src = std::fs::File::open(&from)?;
                let dst = std::fs::File::create(&to)?;
                let ret = unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE, src.as_raw_fd()) };
                if ret == 0 {
                    return Ok(());
                }
                // FICLONE failed (unsupported FS), clean up and fall through to fs::copy
                drop(dst);
                let _ = std::fs::remove_file(&to);
            }

            // Fall back to kernel-level copy
            std::fs::copy(&from, &to)?;
            Ok(())
        })
        .await?
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        let link = to_native(link);
        let target = to_native(target);
        tokio::task::spawn_blocking(move || std::fs::hard_link(&target, &link).map_err(Error::from))
            .await?
    }
}

// ---------------------------------------------------------------------------
// Platform-specific helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn unix_owner_bits(
    meta: &std::fs::Metadata,
    cache: &Arc<UidGidCache>,
) -> (Option<Mode>, Option<UserGroup>, Option<UserGroup>) {
    (
        Some(Mode(meta.mode())),
        cache.user_name(meta.uid()).ok(),
        cache.group_name(meta.gid()).ok(),
    )
}

#[cfg(windows)]
fn unix_owner_bits(
    _meta: &std::fs::Metadata,
    _cache: &Arc<UidGidCache>,
) -> (Option<Mode>, Option<UserGroup>, Option<UserGroup>) {
    (None, None, None)
}

/// Whether a directory entry should be treated as hidden.
///
/// * Unix: the leading-dot convention.
/// * Windows: the filesystem `HIDDEN`/`SYSTEM` attributes (the dot
///   convention is meaningless there; Explorer / Salamander hide both).
#[cfg(unix)]
fn is_hidden(name: &str, _meta: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

#[cfg(windows)]
fn is_hidden(_name: &str, meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM};
    meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(unix)]
fn unix_meta_ids(meta: &std::fs::Metadata) -> (Option<u32>, Option<u32>, Option<u32>) {
    (Some(meta.mode()), Some(meta.uid()), Some(meta.gid()))
}

#[cfg(windows)]
fn unix_meta_ids(_meta: &std::fs::Metadata) -> (Option<u32>, Option<u32>, Option<u32>) {
    (None, None, None)
}

#[cfg(unix)]
fn platform_fs_stats(path: &StdPath) -> Option<FsStats> {
    nix::sys::statvfs::statvfs(path).ok().map(FsStats::from)
}

#[cfg(windows)]
fn platform_fs_stats(path: &StdPath) -> Option<FsStats> {
    win_disk_space(path).map(|(total, free, available)| {
        FsStats::new(
            /* free_bytes */ free, /* available_bytes */ available,
            /* total_bytes */ total,
        )
    })
}

#[cfg(unix)]
fn platform_space_info(path: &StdPath) -> Result<VfsSpaceInfo, Error> {
    let stats = nix::sys::statvfs::statvfs(path)?;
    let frag = stats.fragment_size() as u64;
    Ok(VfsSpaceInfo {
        total_bytes: Some(stats.blocks() as u64 * frag),
        used_bytes: Some((stats.blocks() as u64).saturating_sub(stats.blocks_free() as u64) * frag),
        available_bytes: Some(stats.blocks_available() as u64 * frag),
    })
}

#[cfg(windows)]
fn platform_space_info(path: &StdPath) -> Result<VfsSpaceInfo, Error> {
    match win_disk_space(path) {
        Some((total, free, available)) => Ok(VfsSpaceInfo {
            total_bytes: Some(total),
            used_bytes: Some(total.saturating_sub(free)),
            available_bytes: Some(available),
        }),
        None => Ok(VfsSpaceInfo {
            total_bytes: None,
            used_bytes: None,
            available_bytes: None,
        }),
    }
}

/// Returns `(total_bytes, free_bytes, available_to_caller_bytes)` for the
/// volume containing `path`. Returns `None` if the Win32 call fails (path
/// doesn't exist, network share unavailable, etc.).
#[cfg(windows)]
fn win_disk_space(path: &StdPath) -> Option<(u64, u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    // GetDiskFreeSpaceExW accepts any path on the volume; widen + NUL-terminate.
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut free_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    // SAFETY: All three out-params are valid u64 pointers; `wide` is NUL-terminated.
    let ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_caller, &mut total, &mut total_free)
    };
    if ok == 0 {
        return None;
    }
    Some((total, total_free, free_caller))
}
