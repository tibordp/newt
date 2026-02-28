use std::path::PathBuf;

use crate::rpc::Communicator;
use crate::vfs::{VfsId, VfsPath};
use crate::Error;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum HotPathCategory {
    UserBookmark,
    StandardFolder,
    Bookmark,
    Mount,
    RecentFolder,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HotPathEntry {
    pub path: VfsPath,
    pub name: Option<String>,
    pub category: HotPathCategory,
}

#[async_trait::async_trait]
pub trait HotPathsProvider: Send + Sync {
    async fn system_hot_paths(&self) -> Result<Vec<HotPathEntry>, Error>;
}

// ---------------------------------------------------------------------------
// Local implementation
// ---------------------------------------------------------------------------

pub struct Local;

impl Local {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl HotPathsProvider for Local {
    async fn system_hot_paths(&self) -> Result<Vec<HotPathEntry>, Error> {
        tokio::task::spawn_blocking(collect_system_paths)
            .await
            .map_err(Error::Tokio)
    }
}

fn make_entry(path: PathBuf, name: Option<String>, category: HotPathCategory) -> HotPathEntry {
    HotPathEntry {
        path: VfsPath {
            vfs_id: VfsId::ROOT,
            path,
        },
        name,
        category,
    }
}

fn collect_system_paths() -> Vec<HotPathEntry> {
    let mut entries = Vec::new();
    collect_standard_folders(&mut entries);

    #[cfg(target_os = "linux")]
    {
        collect_gtk_bookmarks(&mut entries);
        collect_linux_mounts(&mut entries);
        collect_recent_xbel(&mut entries);
    }

    #[cfg(target_os = "macos")]
    {
        collect_macos_volumes(&mut entries);
        collect_macos_recent_folders(&mut entries);
    }

    entries
}

// ---------------------------------------------------------------------------
// Cross-platform: standard folders via `dirs`
// ---------------------------------------------------------------------------

fn collect_standard_folders(out: &mut Vec<HotPathEntry>) {
    let pairs: Vec<(&str, Option<PathBuf>)> = vec![
        ("Home", dirs::home_dir()),
        ("Desktop", dirs::desktop_dir()),
        ("Downloads", dirs::download_dir()),
        ("Documents", dirs::document_dir()),
        ("Pictures", dirs::picture_dir()),
        ("Music", dirs::audio_dir()),
        ("Videos", dirs::video_dir()),
    ];

    for (name, dir) in pairs {
        if let Some(path) = dir {
            if path.exists() {
                out.push(make_entry(
                    path,
                    Some(name.to_string()),
                    HotPathCategory::StandardFolder,
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Linux: GTK bookmarks
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn collect_gtk_bookmarks(out: &mut Vec<HotPathEntry>) {
    use std::fs;

    // GTK 3/4 bookmarks location (GTK4 still uses the gtk-3.0 path)
    let gtk3_path = dirs::config_dir().map(|c| c.join("gtk-3.0").join("bookmarks"));
    // Legacy fallback
    let legacy_path = dirs::home_dir().map(|h| h.join(".gtk-bookmarks"));

    let content = gtk3_path
        .and_then(|p| fs::read_to_string(&p).ok())
        .or_else(|| legacy_path.and_then(|p| fs::read_to_string(&p).ok()));

    let content = match content {
        Some(c) => c,
        None => return,
    };

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Format: URI [optional label]
        let (uri_str, label) = match line.find(' ') {
            Some(pos) => (&line[..pos], Some(line[pos + 1..].to_string())),
            None => (line, None),
        };
        if let Some(path) = file_uri_to_path(uri_str) {
            if path.exists() {
                out.push(make_entry(path, label, HotPathCategory::Bookmark));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Linux: mount points from /proc/self/mountinfo
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn collect_linux_mounts(out: &mut Vec<HotPathEntry>) {
    use std::fs;

    let content = match fs::read_to_string("/proc/self/mountinfo") {
        Ok(c) => c,
        Err(_) => return,
    };

    let pseudo_fs_types = [
        "sysfs",
        "proc",
        "tmpfs",
        "devtmpfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "pstore",
        "securityfs",
        "debugfs",
        "configfs",
        "fusectl",
        "hugetlbfs",
        "mqueue",
        "binfmt_misc",
        "autofs",
        "tracefs",
        "overlay",
        "nsfs",
        "efivarfs",
        "bpf",
        "ramfs",
        "rpc_pipefs",
        "nfsd",
    ];

    for line in content.lines() {
        // mountinfo format (space-separated):
        // id parent_id major:minor root mount_point options ... - fs_type source super_options
        let fields: Vec<&str> = line.split_whitespace().collect();
        let separator_pos = fields.iter().position(|&f| f == "-");
        let separator_pos = match separator_pos {
            Some(p) => p,
            None => continue,
        };

        if fields.len() < separator_pos + 3 {
            continue;
        }

        let mount_point = unescape_mountinfo(fields[4]);
        let fs_type = fields[separator_pos + 1];

        if pseudo_fs_types.contains(&fs_type) {
            continue;
        }

        let mount_path = PathBuf::from(&mount_point);

        // Only include mounts under /media, /run/media, or /mnt
        let dominated = mount_point.starts_with("/media/")
            || mount_point.starts_with("/run/media/")
            || mount_point.starts_with("/mnt/");

        if !dominated {
            continue;
        }

        if mount_path.exists() {
            let name = mount_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            out.push(make_entry(mount_path, name, HotPathCategory::Mount));
        }
    }
}

/// Unescape octal escapes in mountinfo fields (e.g. `\040` → space).
#[cfg(target_os = "linux")]
fn unescape_mountinfo(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let oct: String = chars.by_ref().take(3).collect();
            if let Ok(byte) = u8::from_str_radix(&oct, 8) {
                result.push(byte as char);
            } else {
                result.push('\\');
                result.push_str(&oct);
            }
        } else {
            result.push(c);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Linux: recently-used.xbel
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn collect_recent_xbel(out: &mut Vec<HotPathEntry>) {
    use quick_xml::events::Event;
    use quick_xml::Reader;
    use std::collections::HashMap;
    use std::fs;

    let xbel_path = dirs::data_dir().map(|d| d.join("recently-used.xbel"));
    let content = match xbel_path.and_then(|p| fs::read_to_string(&p).ok()) {
        Some(c) => c,
        None => return,
    };

    // Extract href + modified from <bookmark> elements and collect parent dirs.
    // We track the most recent modification timestamp per parent directory.
    let mut dir_timestamps: HashMap<PathBuf, String> = HashMap::new();
    let mut reader = Reader::from_str(&content);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e))
                if e.name().as_ref() == b"bookmark" =>
            {
                let mut href = None;
                let mut modified = None;
                for attr in e.attributes().flatten() {
                    match attr.key.as_ref() {
                        b"href" => {
                            href = String::from_utf8(attr.value.to_vec()).ok();
                        }
                        b"modified" => {
                            modified = String::from_utf8(attr.value.to_vec()).ok();
                        }
                        _ => {}
                    }
                }
                if let Some(href) = href {
                    if let Some(path) = file_uri_to_path(&href) {
                        if let Some(parent) = path.parent() {
                            let parent = parent.to_path_buf();
                            let ts = modified.unwrap_or_default();
                            dir_timestamps
                                .entry(parent)
                                .and_modify(|existing| {
                                    if ts > *existing {
                                        *existing = ts.clone();
                                    }
                                })
                                .or_insert(ts);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    // Sort by most recent timestamp, take top 20
    let mut dirs: Vec<(PathBuf, String)> = dir_timestamps.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    dirs.truncate(20);

    // Filter out standard folders (they're already shown under StandardFolder)
    let standard_dirs: Vec<PathBuf> = [
        dirs::home_dir(),
        dirs::desktop_dir(),
        dirs::download_dir(),
        dirs::document_dir(),
        dirs::picture_dir(),
        dirs::audio_dir(),
        dirs::video_dir(),
    ]
    .into_iter()
    .flatten()
    .collect();

    for (path, _) in dirs {
        if !path.exists() || standard_dirs.contains(&path) {
            continue;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string());
        out.push(make_entry(path, name, HotPathCategory::RecentFolder));
    }
}

// ---------------------------------------------------------------------------
// macOS: volumes
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn collect_macos_volumes(out: &mut Vec<HotPathEntry>) {
    use std::fs;

    let volumes = match fs::read_dir("/Volumes") {
        Ok(d) => d,
        Err(_) => return,
    };

    for entry in volumes.flatten() {
        let path = entry.path();
        let name = path.file_name().map(|n| n.to_string_lossy().to_string());
        out.push(make_entry(path, name, HotPathCategory::Mount));
    }
}

// ---------------------------------------------------------------------------
// macOS: recent folders from Finder prefs
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn collect_macos_recent_folders(out: &mut Vec<HotPathEntry>) {
    let plist_path = dirs::home_dir().map(|h| h.join("Library/Preferences/com.apple.finder.plist"));
    let plist_path = match plist_path {
        Some(p) if p.exists() => p,
        _ => return,
    };

    let plist_val: plist::Value = match plist::from_file(&plist_path) {
        Ok(v) => v,
        Err(_) => return,
    };

    let dict = match plist_val.as_dictionary() {
        Some(d) => d,
        None => return,
    };

    // GoToFieldHistory: array of path strings
    if let Some(plist::Value::Array(arr)) = dict.get("GoToFieldHistory") {
        for val in arr {
            if let Some(s) = val.as_string() {
                let path = PathBuf::from(s);
                if path.exists() {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string());
                    out.push(make_entry(path, name, HotPathCategory::RecentFolder));
                }
            }
        }
    }

    // RecentMoveAndCopyDestinations: array of file:// URL strings
    if let Some(plist::Value::Array(arr)) = dict.get("RecentMoveAndCopyDestinations") {
        for val in arr {
            if let Some(s) = val.as_string() {
                if let Some(path) = file_uri_to_path(s) {
                    if path.exists() {
                        let name = path.file_name().map(|n| n.to_string_lossy().to_string());
                        out.push(make_entry(path, name, HotPathCategory::RecentFolder));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Remote implementation
// ---------------------------------------------------------------------------

pub struct Remote {
    communicator: Communicator,
}

impl Remote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl HotPathsProvider for Remote {
    async fn system_hot_paths(&self) -> Result<Vec<HotPathEntry>, Error> {
        let ret: Result<Vec<HotPathEntry>, Error> = self
            .communicator
            .invoke(crate::api::API_SYSTEM_HOT_PATHS, &())
            .await?;
        Ok(ret?)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let parsed = url::Url::parse(uri).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    parsed.to_file_path().ok()
}
