use std::path::Path;

use crate::filesystem::{mode_string, resolve};

// --- resolve() tests ---

#[test]
fn resolve_simple_path() {
    assert_eq!(resolve(Path::new("/foo/bar")), Path::new("/foo/bar"));
}

#[test]
fn resolve_root() {
    assert_eq!(resolve(Path::new("/")), Path::new("/"));
}

#[test]
fn resolve_removes_dot() {
    assert_eq!(resolve(Path::new("/foo/./bar")), Path::new("/foo/bar"));
}

#[test]
fn resolve_removes_multiple_dots() {
    assert_eq!(resolve(Path::new("/./././foo")), Path::new("/foo"));
}

#[test]
fn resolve_handles_dotdot() {
    assert_eq!(resolve(Path::new("/foo/bar/..")), Path::new("/foo"));
}

#[test]
fn resolve_dotdot_at_root_stays_at_root() {
    assert_eq!(resolve(Path::new("/../foo")), Path::new("/foo"));
}

#[test]
fn resolve_multiple_dotdot_past_root() {
    assert_eq!(
        resolve(Path::new("/foo/../../../../../../etc/passwd")),
        Path::new("/etc/passwd")
    );
}

#[test]
fn resolve_trailing_dotdot() {
    assert_eq!(resolve(Path::new("/foo/bar/baz/../..")), Path::new("/foo"));
}

#[test]
fn resolve_mixed_dot_and_dotdot() {
    assert_eq!(
        resolve(Path::new("/foo/./bar/../baz/./qux/..")),
        Path::new("/foo/baz")
    );
}

#[test]
fn resolve_only_dotdot_from_root() {
    assert_eq!(resolve(Path::new("/..")), Path::new("/"));
}

#[test]
fn resolve_preserves_trailing_component() {
    assert_eq!(resolve(Path::new("/a/b/../c/d")), Path::new("/a/c/d"));
}

// --- mode_string() tests ---

#[test]
fn mode_string_regular_file_644() {
    assert_eq!(mode_string(0o100644), "-rw-r--r--");
}

#[test]
fn mode_string_directory_755() {
    assert_eq!(mode_string(0o040755), "drwxr-xr-x");
}

#[test]
fn mode_string_symlink_777() {
    assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
}

#[test]
fn mode_string_executable_755() {
    assert_eq!(mode_string(0o100755), "-rwxr-xr-x");
}

#[test]
fn mode_string_no_permissions() {
    assert_eq!(mode_string(0o100000), "----------");
}

#[test]
fn mode_string_setuid() {
    // setuid on owner execute
    assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
}

#[test]
fn mode_string_setuid_no_exec() {
    // setuid without owner execute -> capital S
    assert_eq!(mode_string(0o104644), "-rwSr--r--");
}

#[test]
fn mode_string_setgid() {
    assert_eq!(mode_string(0o102755), "-rwxr-sr-x");
}

#[test]
fn mode_string_setgid_no_exec() {
    assert_eq!(mode_string(0o102744), "-rwxr-Sr--");
}

#[test]
fn mode_string_sticky() {
    assert_eq!(mode_string(0o101777), "-rwxrwxrwt");
}

#[test]
fn mode_string_sticky_no_exec() {
    assert_eq!(mode_string(0o101776), "-rwxrwxrwT");
}

#[test]
fn mode_string_all_special_bits() {
    // setuid + setgid + sticky + all permissions
    assert_eq!(mode_string(0o107777), "-rwsrwsrwt");
}

#[test]
fn mode_string_block_device() {
    assert_eq!(mode_string(0o060660), "brw-rw----");
}

#[test]
fn mode_string_char_device() {
    assert_eq!(mode_string(0o020666), "crw-rw-rw-");
}

#[test]
fn mode_string_pipe() {
    assert_eq!(mode_string(0o010644), "prw-r--r--");
}

#[test]
fn mode_string_socket() {
    assert_eq!(mode_string(0o140755), "srwxr-xr-x");
}
