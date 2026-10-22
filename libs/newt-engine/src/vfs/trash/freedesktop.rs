//! The freedesktop.org trash: `$XDG_DATA_HOME/Trash` for the filesystem it
//! lives on, a trash at the top of every other filesystem, and a
//! `.trashinfo` record beside each item saying where it came from.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_encode};
use tokio::io::AsyncWriteExt;

use super::refused;
use newt_common::{Error, ErrorKind};

/// `S_ISVTX`, whose type differs between libcs.
const STICKY: u32 = 0o1000;

/// RFC 2396 escaping, which the spec asks of `Path=`, with `/` kept.
const PATH_ESCAPES: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'/')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

#[cfg(not(target_os = "macos"))]
pub(super) async fn trash(path: &Path) -> Result<(), Error> {
    trash_into(path, &home_trash()?, nix::unistd::geteuid().as_raw()).await
}

/// `$XDG_DATA_HOME/Trash`, by default `~/.local/share/Trash`.
#[cfg(not(target_os = "macos"))]
fn home_trash() -> Result<PathBuf, Error> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
        .map(|data_home| data_home.join("Trash"))
        .ok_or_else(|| refused("neither XDG_DATA_HOME nor HOME is set"))
}

async fn trash_into(path: &Path, home_trash: &Path, uid: u32) -> Result<(), Error> {
    // The item itself is never followed: a link goes to the trash as a link.
    // Its parent is resolved, so the walk up to the top of its filesystem
    // passes real directories.
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(refused("a filesystem root can't be moved to the trash"));
    };
    let item = tokio::fs::canonicalize(parent).await?.join(name);
    let device = tokio::fs::symlink_metadata(&item).await?.dev();

    if device == device_of_nearest(home_trash).await? {
        return move_into(&item, home_trash, None).await;
    }
    let top = top_dir(&item, device).await?;
    let trash = volume_trash(&top, uid).await?;
    move_into(&item, &trash, Some(&top)).await
}

/// The device of `path`, or of its nearest existing ancestor: the home
/// trash is created on first use.
async fn device_of_nearest(mut path: &Path) -> Result<u64, Error> {
    loop {
        match tokio::fs::metadata(path).await {
            Ok(meta) => return Ok(meta.dev()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                path = path.parent().ok_or(e)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// The mount point of `device`, the filesystem `item` is on.
async fn top_dir(item: &Path, device: u64) -> Result<PathBuf, Error> {
    let mut top = item
        .parent()
        .expect("item is a resolved parent joined with a name");
    if tokio::fs::metadata(top).await?.dev() != device {
        return Err(refused("a mount point can't be moved to the trash"));
    }
    while let Some(up) = top.parent() {
        if tokio::fs::metadata(up).await?.dev() != device {
            break;
        }
        top = up;
    }
    Ok(top.to_owned())
}

/// `$top/.Trash/$uid` where an administrator set up a shared `.Trash` (a
/// sticky directory, not a link), else `$top/.Trash-$uid`. Either has to be
/// the user's own directory; a filesystem where neither can be has no trash.
async fn volume_trash(top: &Path, uid: u32) -> Result<PathBuf, Error> {
    let shared = top.join(".Trash");
    if let Ok(meta) = tokio::fs::symlink_metadata(&shared).await
        && meta.is_dir()
        && meta.mode() & STICKY != 0
    {
        let own = shared.join(uid.to_string());
        if private_dir(&own, uid).await {
            return Ok(own);
        }
    }
    let own = top.join(format!(".Trash-{uid}"));
    if private_dir(&own, uid).await {
        return Ok(own);
    }
    Err(refused(format!(
        "there is no trash on the filesystem at {}",
        top.display()
    )))
}

/// Whether `dir` is, or now is, a directory of `uid`'s and not a link.
async fn private_dir(dir: &Path, uid: u32) -> bool {
    let _ = tokio::fs::DirBuilder::new().mode(0o700).create(dir).await;
    tokio::fs::symlink_metadata(dir)
        .await
        .is_ok_and(|meta| meta.is_dir() && meta.uid() == uid)
}

/// Reserve a name in `trash` by creating its `.trashinfo`, then rename
/// `item` in under it. The record holds the path relative to `top` in a
/// filesystem's own trash, the absolute path in the home trash.
async fn move_into(item: &Path, trash: &Path, top: Option<&Path>) -> Result<(), Error> {
    let files = trash.join("files");
    let info = trash.join("info");
    for dir in [&files, &info] {
        tokio::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .await?;
    }
    let original = match top {
        Some(top) => item.strip_prefix(top).expect("top is an ancestor of item"),
        None => item,
    };
    let record = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(original.as_os_str().as_bytes(), PATH_ESCAPES),
        deletion_date()
    );
    let name = item
        .file_name()
        .expect("item is a resolved parent joined with a name");

    let mut n = 1;
    loop {
        let slot = numbered(name, n);
        n += 1;
        let mut record_name = slot.clone();
        record_name.push(".trashinfo");
        let record_path = info.join(record_name);
        let mut file = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&record_path)
            .await
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        };
        let moved = match file.write_all(record.as_bytes()).await {
            Ok(()) => rename_into(item, &files.join(&slot)).await,
            Err(e) => Err(e.into()),
        };
        match moved {
            Ok(true) => return Ok(()),
            // A slot taken by an item whose record went missing.
            Ok(false) => {
                let _ = tokio::fs::remove_file(&record_path).await;
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&record_path).await;
                return Err(e);
            }
        }
    }
}

/// Rename `item` to `target` unless something is there already (`false`).
async fn rename_into(item: &Path, target: &Path) -> Result<bool, Error> {
    let (from, to) = (item.to_owned(), target.to_owned());
    let result = tokio::task::spawn_blocking(move || {
        match super::super::local::rename_no_replace(&from, &to) {
            // A filesystem without an exclusive rename (or the cross-device
            // case, which reads the same): check, then rename plainly.
            Err(e) if e.kind == ErrorKind::NotSupported => match std::fs::symlink_metadata(&to) {
                Ok(_) => Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists).into()),
                Err(_) => std::fs::rename(&from, &to).map_err(Error::from),
            },
            result => result,
        }
    })
    .await?;
    match result {
        Ok(()) => Ok(true),
        Err(e) if e.kind == ErrorKind::AlreadyExists => Ok(false),
        Err(e) if e.kind == ErrorKind::NotSupported => Err(refused(format!(
            "it can't be moved into the trash at {}",
            target.parent().unwrap_or(target).display()
        ))),
        Err(e) => Err(e),
    }
}

/// `name`, then `stem.2.ext`, `stem.3.ext`, …
fn numbered(name: &std::ffi::OsStr, n: u32) -> OsString {
    if n == 1 {
        return name.to_owned();
    }
    let path = Path::new(name);
    let mut slot = OsString::from(path.file_stem().unwrap_or(name));
    slot.push(format!(".{n}"));
    if let Some(ext) = path.extension() {
        slot.push(".");
        slot.push(ext);
    }
    slot
}

/// Local time, `YYYY-MM-DDThh:mm:ss`, as the spec has it.
fn deletion_date() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid() -> u32 {
        nix::unistd::geteuid().as_raw()
    }

    async fn record(trash: &Path, slot: &str) -> String {
        tokio::fs::read_to_string(trash.join("info").join(format!("{slot}.trashinfo")))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_item_on_the_home_filesystem_goes_to_the_home_trash() {
        let dir = tempfile::tempdir().unwrap();
        let home_trash = dir.path().join("data/Trash");
        let item = dir.path().join("a file.txt");
        std::fs::write(&item, b"x").unwrap();

        trash_into(&item, &home_trash, uid()).await.unwrap();

        assert!(!item.exists());
        assert_eq!(
            std::fs::read(home_trash.join("files/a file.txt")).unwrap(),
            b"x"
        );
        let record = record(&home_trash, "a file.txt").await;
        let canonical = std::fs::canonicalize(dir.path())
            .unwrap()
            .join("a%20file.txt");
        assert!(record.starts_with("[Trash Info]\nPath=/"), "{record}");
        assert!(
            record.contains(&format!("Path={}\n", canonical.display())),
            "{record}"
        );
        assert!(record.contains("\nDeletionDate=2"), "{record}");
    }

    #[tokio::test]
    async fn names_already_in_the_trash_get_numbered() {
        let dir = tempfile::tempdir().unwrap();
        let home_trash = dir.path().join("Trash");
        for content in [b"1", b"2", b"3"] {
            let item = dir.path().join("photo.jpg");
            std::fs::write(&item, content).unwrap();
            trash_into(&item, &home_trash, uid()).await.unwrap();
        }
        let files = home_trash.join("files");
        assert_eq!(std::fs::read(files.join("photo.jpg")).unwrap(), b"1");
        assert_eq!(std::fs::read(files.join("photo.2.jpg")).unwrap(), b"2");
        assert_eq!(std::fs::read(files.join("photo.3.jpg")).unwrap(), b"3");
        assert!(home_trash.join("info/photo.3.jpg.trashinfo").exists());
    }

    #[tokio::test]
    async fn a_slot_taken_without_a_record_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let home_trash = dir.path().join("Trash");
        std::fs::create_dir_all(home_trash.join("files/notes")).unwrap();
        let item = dir.path().join("notes");
        std::fs::write(&item, b"new").unwrap();

        trash_into(&item, &home_trash, uid()).await.unwrap();

        assert_eq!(
            std::fs::read(home_trash.join("files/notes.2")).unwrap(),
            b"new"
        );
        assert!(!home_trash.join("info/notes.trashinfo").exists());
        assert!(home_trash.join("info/notes.2.trashinfo").exists());
    }

    #[tokio::test]
    async fn a_link_goes_to_the_trash_as_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let home_trash = dir.path().join("Trash");
        let target = dir.path().join("target");
        std::fs::write(&target, b"kept").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        trash_into(&link, &home_trash, uid()).await.unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"kept");
        let trashed = home_trash.join("files/link");
        assert!(std::fs::symlink_metadata(&trashed).unwrap().is_symlink());
    }

    #[tokio::test]
    async fn a_volume_trash_records_the_path_from_its_top() {
        let dir = tempfile::tempdir().unwrap();
        let top = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(top.join("sub")).unwrap();
        let item = top.join("sub/f");
        std::fs::write(&item, b"x").unwrap();

        let trash = volume_trash(&top, uid()).await.unwrap();
        assert_eq!(trash, top.join(format!(".Trash-{}", uid())));
        move_into(&item, &trash, Some(&top)).await.unwrap();

        assert!(record(&trash, "f").await.contains("\nPath=sub/f\n"));
        let mode = std::fs::metadata(&trash).unwrap().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[tokio::test]
    async fn a_shared_trash_is_used_only_when_sticky() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        let shared = top.join(".Trash");
        std::fs::create_dir(&shared).unwrap();

        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let trash = volume_trash(top, uid()).await.unwrap();
        assert_eq!(trash, top.join(format!(".Trash-{}", uid())));

        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let trash = volume_trash(top, uid()).await.unwrap();
        assert_eq!(trash, shared.join(uid().to_string()));
    }

    #[tokio::test]
    async fn a_trash_dir_that_is_a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        let elsewhere = top.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, top.join(format!(".Trash-{}", uid()))).unwrap();

        let err = volume_trash(top, uid()).await.unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotSupported);
    }

    #[tokio::test]
    async fn a_filesystem_root_is_refused() {
        let err = trash_into(Path::new("/"), Path::new("/nonexistent/Trash"), uid())
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::NotSupported);
    }

    #[test]
    fn numbering_keeps_the_extension() {
        let numbered = |name: &str, n| numbered(std::ffi::OsStr::new(name), n);
        assert_eq!(numbered("a.tar.gz", 2), "a.tar.2.gz");
        assert_eq!(numbered(".bashrc", 2), ".bashrc.2");
        assert_eq!(numbered("Makefile", 3), "Makefile.3");
    }

    #[test]
    fn paths_are_escaped_as_uris() {
        let escaped = percent_encode(b"/a b/\xff%/x(1).txt", PATH_ESCAPES).to_string();
        assert_eq!(escaped, "/a%20b/%FF%25/x(1).txt");
    }
}
