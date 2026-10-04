//! Reading and writing the user's configuration files (`settings.toml`,
//! `connections.toml`, `state.json`, `sessions.json`).

use std::io::Write;
use std::path::Path;

/// The file's contents for an edit that will write it back: empty when it
/// doesn't exist yet, an error when it can't be read — never an empty
/// string standing in for contents that would then be overwritten.
pub fn read_for_edit(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("Can't read {}: {e}", path.display())),
    }
}

/// Replace the file at `path` with `contents` in one step: written and
/// synced to a temporary file beside it, then renamed over it, so a crash
/// or a full disk leaves the old file whole. A symlink is written through
/// to its target and stays a link; the target keeps its permissions.
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let target = match std::fs::canonicalize(path) {
        Ok(target) => target,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_owned(),
        Err(e) => return Err(e),
    };
    let dir = target.parent().unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(contents.as_ref())?;
    if let Ok(meta) = std::fs::metadata(&target) {
        file.as_file().set_permissions(meta.permissions())?;
    }
    file.as_file().sync_all()?;
    file.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_reads_as_empty_and_an_unreadable_one_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_for_edit(&dir.path().join("absent.toml")),
            Ok(String::new())
        );
        let latin1 = dir.path().join("latin1.toml");
        std::fs::write(&latin1, b"name = \"caf\xe9\"\n").unwrap();
        assert!(read_for_edit(&latin1).is_err());
    }

    #[test]
    fn writes_replace_the_file_and_leave_nothing_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        write_atomic(&path, "a = 1\n").unwrap();
        write_atomic(&path, "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_written_at_its_target_with_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        let target = dotfiles.join("settings.toml");
        std::fs::write(&target, "a = 1\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("settings.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_atomic(&link, "a = 2\n").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "a = 2\n");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
