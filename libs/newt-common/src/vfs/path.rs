//! Platform-independent VFS paths.
//!
//! [`Path`]/[`PathBuf`] mirror the borrowed/owned split of
//! `std::path::{Path, PathBuf}`, but are deliberately *not* the std types:
//!
//! * Backed by `String`, not `OsString`.
//! * Behave identically on every OS — always `/`-separated, one notion of
//!   "absolute", no drive-letter / UNC / separator skew.
//! * Opaque: no `Deref<str>`, `AsRef<str>`, or `From<String>`. Code that
//!   needs the raw string for a wire/protocol boundary must go through
//!   [`Path::as_wire_str`] / [`PathBuf::from_wire_str`], which makes those
//!   boundaries greppable.
//!
//! A `Path` is the path *within* a VFS — it carries no VFS identity. The
//! `Vfs` trait surface takes `&Path` because dispatch has already selected
//! the VFS. [`VfsPath`] is the fully-qualified locator (`vfs_id` + path).
//!
//! # Canonical form
//!
//! The backing string is always one of:
//! * `"/"` — the VFS root.
//! * `"/a/b/c"` — no trailing slash, no empty segments, no `.`/`..`.
//!
//! Construction normalizes to this form; every method preserves it.

use std::borrow::Borrow;
use std::ops::Deref;
use std::path::{Path as StdPath, PathBuf as StdPathBuf};

use serde::{Deserialize, Serialize};

use super::VfsId;

// ---------------------------------------------------------------------------
// Path (borrowed, unsized — mirrors std::path::Path)
// ---------------------------------------------------------------------------

/// Borrowed VFS path. See module docs. Always in canonical form.
#[repr(transparent)]
pub struct Path {
    inner: str,
}

impl Path {
    /// Wrap a string slice that is *already* canonical. Internal — callers
    /// outside this module go through [`PathBuf::from_wire_str`].
    fn new_unchecked(s: &str) -> &Path {
        // SAFETY: `Path` is `repr(transparent)` over `str`, so `&str` and
        // `&Path` have identical layout. Same pattern std uses for `Path`.
        unsafe { &*(s as *const str as *const Path) }
    }

    /// The canonical backing string (`"/"` or `"/a/b"`). This is the
    /// wire/serialization form and the *only* sanctioned way to obtain
    /// the raw string — kept deliberately verbose so boundaries grep.
    pub fn as_wire_str(&self) -> &str {
        &self.inner
    }

    /// `true` when this is the VFS root.
    pub fn is_root(&self) -> bool {
        &self.inner == "/"
    }

    /// Number of path components. `0` at the root.
    pub fn depth(&self) -> usize {
        if self.is_root() {
            0
        } else {
            // Canonical non-root form is "/a/b" → one fewer than the
            // number of '/'.
            self.inner.bytes().filter(|&b| b == b'/').count()
        }
    }

    /// Last component, or `None` at the root.
    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            None
        } else {
            self.inner.rsplit('/').next().filter(|s| !s.is_empty())
        }
    }

    /// Parent path — `None` at the root. Parent of `/a` is `/`.
    pub fn parent(&self) -> Option<&Path> {
        if self.is_root() {
            return None;
        }
        let idx = self.inner.rfind('/').expect("non-root has a '/'");
        if idx == 0 {
            Some(Path::new_unchecked("/"))
        } else {
            Some(Path::new_unchecked(&self.inner[..idx]))
        }
    }

    /// Iterator over the path's components (no leading `/`, no empties).
    /// Empty at the root.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        let body = if self.is_root() { "" } else { &self.inner[1..] };
        body.split('/').filter(|s| !s.is_empty())
    }

    /// `true` when `prefix` is a path-prefix of `self` (component-wise; a
    /// path is its own prefix).
    pub fn starts_with(&self, prefix: &Path) -> bool {
        if prefix.is_root() {
            return true;
        }
        if self.inner == prefix.inner {
            return true;
        }
        // "/a/b" starts_with "/a"  ⇔  inner starts with "/a" + boundary '/'
        self.inner.starts_with(&prefix.inner)
            && self.inner.as_bytes().get(prefix.inner.len()) == Some(&b'/')
    }

    /// Strip `prefix`, returning the remaining components as a `/`-joined
    /// string *without* a leading slash (`""` when equal). `None` if
    /// `prefix` is not actually a prefix.
    pub fn strip_prefix(&self, prefix: &Path) -> Option<&str> {
        if !self.starts_with(prefix) {
            return None;
        }
        if self.inner == prefix.inner {
            return Some("");
        }
        let cut = if prefix.is_root() {
            1
        } else {
            prefix.inner.len() + 1
        };
        Some(&self.inner[cut..])
    }

    /// Append a relative subpath, returning a new owned path. `rel` may
    /// be a single component (`foo`) or a multi-component relative path
    /// (`a/b/c`) — empty and `.` segments are dropped, mirroring
    /// `std::path::Path::join` for relative inputs. Synthetic VFSes
    /// (search results) legitimately produce multi-segment keys, so this
    /// is intentionally NOT the strict single-component `push`.
    pub fn join(&self, rel: &str) -> PathBuf {
        let mut p = self.to_owned();
        for seg in rel.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            p.push(seg);
        }
        p
    }

    pub fn to_owned(&self) -> PathBuf {
        PathBuf {
            inner: self.inner.to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// PathBuf (owned — mirrors std::path::PathBuf)
// ---------------------------------------------------------------------------

/// Owned VFS path. See module docs. Always in canonical form.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathBuf {
    inner: String,
}

impl Default for PathBuf {
    fn default() -> Self {
        Self::root()
    }
}

impl PathBuf {
    /// The VFS root (`"/"`).
    pub fn root() -> Self {
        Self {
            inner: String::from("/"),
        }
    }

    /// Parse a wire/display string into a canonical path. Splits on `/`,
    /// drops empty and `.` components; `..` is kept literal (navigation —
    /// not path construction — is responsible for resolving it, and a
    /// correctly-built path never contains one). The leading slash is
    /// optional in the input.
    pub fn from_wire_str(s: &str) -> Self {
        let mut out = String::new();
        for seg in s.split('/') {
            if seg.is_empty() || seg == "." {
                continue;
            }
            out.push('/');
            out.push_str(seg);
        }
        if out.is_empty() {
            out.push('/');
        }
        Self { inner: out }
    }

    /// Build from already-split components.
    pub fn from_components<I, S>(components: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut inner = String::new();
        for c in components {
            let c = c.as_ref();
            if c.is_empty() {
                continue;
            }
            debug_assert!(!c.contains('/'), "path component must not contain '/'");
            inner.push('/');
            inner.push_str(c);
        }
        if inner.is_empty() {
            inner.push('/');
        }
        Self { inner }
    }

    /// Append a single component in place. Panics if `segment` is empty
    /// or contains `/`.
    pub fn push(&mut self, segment: &str) {
        assert!(!segment.is_empty(), "VFS path component must not be empty");
        assert!(
            !segment.contains('/'),
            "VFS path component must not contain '/': {segment:?}"
        );
        if self.inner == "/" {
            // "/" + "a" → "/a" (no extra separator needed at the root).
            self.inner.push_str(segment);
        } else {
            self.inner.push('/');
            self.inner.push_str(segment);
        }
    }

    /// Drop the last component. Returns `false` (and leaves the path
    /// unchanged at `/`) when already at the root.
    pub fn pop(&mut self) -> bool {
        if self.inner == "/" {
            return false;
        }
        let idx = self.inner.rfind('/').expect("non-root has a '/'");
        if idx == 0 {
            self.inner.truncate(1); // back to "/"
        } else {
            self.inner.truncate(idx);
        }
        true
    }

    /// Reborrow as a [`Path`].
    pub fn as_path(&self) -> &Path {
        Path::new_unchecked(&self.inner)
    }

    /// Round-trips with [`Path::as_wire_str`].
    pub fn from_wire_string(s: String) -> Self {
        // Re-normalize rather than trusting the input.
        Self::from_wire_str(&s)
    }
}

// ---------------------------------------------------------------------------
// Native (`std::path`) interop.
//
// Native forms must only ever exist in the process that physically owns the
// filesystem — the host locally, the agent in a remote session, each
// compiled for its own platform. Nothing produced here may cross the RPC
// boundary.
// ---------------------------------------------------------------------------

impl Path {
    /// Render into a host-native `std::path::PathBuf` safe to feed to
    /// `std::fs` / `opener` / any Win32 or POSIX consumer.
    ///
    /// * Unix: `/foo/bar` → `/foo/bar`.
    /// * Windows: `/?/C:/Users/Tibor` → `\\?\C:\Users\Tibor`;
    ///   `/?/UNC/server/share/foo` → `\\?\UNC\server\share\foo`.
    pub fn to_native(&self) -> StdPathBuf {
        #[cfg(windows)]
        {
            let comps: Vec<&str> = self.components().collect();
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
            StdPathBuf::from(self.as_wire_str())
        }
    }

    /// Native path suitable as a **spawned process's working directory**.
    ///
    /// [`Path::to_native`] returns the verbatim (`\\?\…`) form, which
    /// `std::fs` and the change watcher need but which `cmd.exe` chokes on:
    /// it reads `\\?\C:\…` as a UNC path, refuses to `cd` there, and
    /// silently starts in `%SystemRoot%` instead — so local terminals would
    /// open in the wrong directory. Strip the verbatim prefix so an ordinary
    /// local directory becomes a plain `C:\…` the shell accepts. Genuine
    /// network locations are intentionally left as conventional UNC
    /// (`\\server\share\…`) so the shell shows its own "UNC paths are not
    /// supported" notice rather than us masking it. Over-long paths
    /// (> `MAX_PATH`) keep the verbatim form, since stripping it wouldn't
    /// help `cmd` anyway and other shells can still use it.
    ///
    /// On non-Windows this is just [`Path::to_native`].
    pub fn launch_cwd(&self) -> StdPathBuf {
        #[cfg(windows)]
        {
            const MAX_PATH: usize = 260;
            let s = self.to_native().to_string_lossy().into_owned();
            if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
                // Verbatim UNC → conventional UNC; cmd will (rightly) warn.
                StdPathBuf::from(format!(r"\\{rest}"))
            } else if let Some(rest) = s.strip_prefix(r"\\?\") {
                if rest.len() <= MAX_PATH {
                    StdPathBuf::from(rest)
                } else {
                    StdPathBuf::from(s)
                }
            } else {
                StdPathBuf::from(s)
            }
        }
        #[cfg(not(windows))]
        {
            self.to_native()
        }
    }
}

impl PathBuf {
    /// Decode a host-native `std::path::Path` into VFS path components.
    ///
    /// * Unix: walks `Normal` components — `/home/user` → `["home", "user"]`.
    /// * Windows: emits the `"?"` sentinel then drive (`["?", "C:", …]`) or
    ///   UNC (`["?", "UNC", "server", "share", …]`) info, then `Normal`
    ///   components. Verbatim (`\\?\…`) and conventional forms collapse to
    ///   the same components.
    ///
    /// Used at the boundary between native-path APIs
    /// (`std::env::current_dir`, `dirs::*`, drag-and-drop) and `VfsPath`.
    pub fn from_native(path: &StdPath) -> PathBuf {
        PathBuf::from_components(native_segments(path))
    }

    /// [`PathBuf::from_native`] for *user-typed* input: the same decoding,
    /// behind vetting the native decode doesn't do. Only distinctively
    /// absolute Windows shapes are claimed — `X:` + separator/end (drive) or
    /// a leading `\\` (UNC/verbatim). Drive-relative (`C:foo`) and plain
    /// relative inputs return `None` instead of being silently absolutized,
    /// as do `..` segments (the native decode drops them, which is only
    /// sound for canonicalized paths) and the non-filesystem `\\?\`/`\\.\`
    /// namespaces beyond verbatim drive/UNC forms.
    ///
    /// Windows-hosted by construction: the caller gates on a Windows-styled
    /// `mount_meta`, which for a client-local mount implies this process
    /// *is* the Windows side — so `std::path` parses the syntax natively.
    /// Compiled to `None` elsewhere rather than let `std::path` misread the
    /// input as a single relative component.
    #[cfg(windows)]
    pub fn from_typed_display(input: &str) -> Option<PathBuf> {
        let (s, verbatim) = match input.strip_prefix(r"\\?\") {
            Some(rest) => (rest, true),
            None => (input, false),
        };
        let b = s.as_bytes();
        let drive = b.len() >= 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/');
        if !drive {
            // UNC: require server + share, and reject the `?`/`.` device
            // namespaces (`//?/…` is not parsed as verbatim by std::path).
            let rest = if verbatim {
                s.strip_prefix("UNC\\").or_else(|| s.strip_prefix("UNC/"))?
            } else {
                s.strip_prefix(r"\\").or_else(|| s.strip_prefix("//"))?
            };
            let mut parts = rest.split(['/', '\\']).filter(|p| !p.is_empty());
            let server = parts.next()?;
            parts.next()?;
            if server == "?" || server == "." {
                return None;
            }
        }
        if s.split(['/', '\\']).any(|seg| seg == "..") {
            return None;
        }
        Some(Self::from_native(StdPath::new(input)))
    }

    #[cfg(not(windows))]
    pub fn from_typed_display(_input: &str) -> Option<PathBuf> {
        None
    }
}

fn native_segments(path: &StdPath) -> Vec<String> {
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

// --- Deref / Borrow / conversions: PathBuf ⇒ Path, mirroring std ---

impl Deref for PathBuf {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.as_path()
    }
}

impl Borrow<Path> for PathBuf {
    fn borrow(&self) -> &Path {
        self.as_path()
    }
}

impl AsRef<Path> for PathBuf {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl AsRef<Path> for Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl ToOwned for Path {
    type Owned = PathBuf;
    fn to_owned(&self) -> PathBuf {
        Path::to_owned(self)
    }
}

impl std::fmt::Debug for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", &self.inner)
    }
}

impl std::fmt::Debug for PathBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.inner)
    }
}

impl std::fmt::Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.inner)
    }
}

impl std::fmt::Display for PathBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.inner)
    }
}

impl PartialEq<Path> for PathBuf {
    fn eq(&self, other: &Path) -> bool {
        self.inner == other.inner
    }
}

// --- Serde: transparent to the canonical string ---

impl Serialize for PathBuf {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.inner)
    }
}

impl<'de> Deserialize<'de> for PathBuf {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(PathBuf::from_wire_string(s))
    }
}

impl specta::Type for PathBuf {
    fn inline(types: &mut specta::TypeCollection, generics: specta::Generics) -> specta::DataType {
        // Opaque to TS — it's just a string on the wire.
        String::inline(types, generics)
    }
}

// ---------------------------------------------------------------------------
// VfsPath — the VFS-qualified locator
// ---------------------------------------------------------------------------

/// A fully-qualified location: which VFS, and where within it.
///
/// `specta::Type` is derived: `PathBuf`'s own `Type` impl renders as a
/// plain `string`, so the generated TS is `{ vfs_id: VfsId; path: string }`
/// under the named type `VfsPath` — identical to the pre-refactor shape.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    specta::Type,
)]
pub struct VfsPath {
    pub vfs_id: VfsId,
    pub path: PathBuf,
}

impl VfsPath {
    /// Root of the given VFS.
    pub fn root(vfs_id: VfsId) -> Self {
        Self {
            vfs_id,
            path: PathBuf::root(),
        }
    }

    pub fn new(vfs_id: VfsId, path: PathBuf) -> Self {
        Self { vfs_id, path }
    }

    /// Convenience: parse a wire string under the given VFS.
    pub fn from_wire_str(vfs_id: VfsId, s: &str) -> Self {
        Self {
            vfs_id,
            path: PathBuf::from_wire_str(s),
        }
    }

    pub fn is_root(&self) -> bool {
        self.path.is_root()
    }

    pub fn file_name(&self) -> Option<&str> {
        self.path.file_name()
    }

    /// Logical parent (same VFS, one component shorter). `None` at root.
    pub fn parent(&self) -> Option<VfsPath> {
        self.path.parent().map(|p| VfsPath {
            vfs_id: self.vfs_id,
            path: p.to_owned(),
        })
    }

    pub fn join(&self, segment: &str) -> VfsPath {
        VfsPath {
            vfs_id: self.vfs_id,
            path: self.path.join(segment),
        }
    }

    /// `true` when `prefix` is in the same VFS and a path-prefix of self.
    pub fn starts_with(&self, prefix: &VfsPath) -> bool {
        self.vfs_id == prefix.vfs_id && self.path.starts_with(&prefix.path)
    }

    /// Remaining components after stripping `prefix` (same semantics as
    /// [`Path::strip_prefix`]). `None` if not a prefix or different VFS.
    pub fn strip_prefix(&self, prefix: &VfsPath) -> Option<&str> {
        if self.vfs_id != prefix.vfs_id {
            return None;
        }
        self.path.strip_prefix(&prefix.path)
    }
}

impl std::fmt::Display for VfsPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.vfs_id == VfsId::ROOT {
            write!(f, "{}", self.path)
        } else {
            write!(f, "vfs://{}:{}", self.vfs_id, self.path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(s: &str) -> PathBuf {
        PathBuf::from_wire_str(s)
    }

    #[test]
    fn canonical_form() {
        assert_eq!(pb("/").as_wire_str(), "/");
        assert_eq!(pb("").as_wire_str(), "/");
        assert_eq!(pb("//a//b/").as_wire_str(), "/a/b");
        assert_eq!(pb("a/b").as_wire_str(), "/a/b");
        assert_eq!(pb("/a/./b").as_wire_str(), "/a/b");
    }

    #[test]
    fn root_predicates() {
        let r = PathBuf::root();
        assert!(r.is_root());
        assert_eq!(r.depth(), 0);
        assert_eq!(r.file_name(), None);
        assert!(r.parent().is_none());
        assert_eq!(r.components().count(), 0);
    }

    #[test]
    fn file_name_and_parent() {
        let p = pb("/users/tibor/src");
        assert_eq!(p.file_name(), Some("src"));
        assert_eq!(p.depth(), 3);
        let parent = p.parent().unwrap();
        assert_eq!(parent.as_wire_str(), "/users/tibor");
        assert_eq!(parent.parent().unwrap().as_wire_str(), "/users");
        assert_eq!(
            parent.parent().unwrap().parent().unwrap().as_wire_str(),
            "/"
        );
    }

    #[test]
    fn push_pop() {
        let mut p = PathBuf::root();
        p.push("a");
        assert_eq!(p.as_wire_str(), "/a");
        p.push("b");
        assert_eq!(p.as_wire_str(), "/a/b");
        assert!(p.pop());
        assert_eq!(p.as_wire_str(), "/a");
        assert!(p.pop());
        assert_eq!(p.as_wire_str(), "/");
        assert!(!p.pop());
        assert_eq!(p.as_wire_str(), "/");
    }

    #[test]
    #[should_panic]
    fn push_rejects_slash() {
        PathBuf::root().push("a/b");
    }

    #[test]
    #[should_panic]
    fn push_rejects_empty() {
        PathBuf::root().push("");
    }

    #[test]
    fn components_iter() {
        assert_eq!(
            pb("/a/b/c").components().collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert_eq!(PathBuf::root().components().count(), 0);
    }

    #[test]
    fn starts_with_and_strip() {
        let base = pb("/users/tibor");
        let child = pb("/users/tibor/src/lib.rs");
        assert!(child.starts_with(&base));
        assert!(base.starts_with(&base));
        assert!(base.starts_with(&PathBuf::root()));
        assert!(!base.starts_with(&child));
        // Not fooled by a shared string prefix that isn't a path prefix.
        assert!(!pb("/users/tiborX").starts_with(&base));

        assert_eq!(child.strip_prefix(&base), Some("src/lib.rs"));
        assert_eq!(base.strip_prefix(&base), Some(""));
        assert_eq!(
            child.strip_prefix(&PathBuf::root()),
            Some("users/tibor/src/lib.rs")
        );
        assert_eq!(base.strip_prefix(&child), None);
    }

    #[test]
    fn deref_to_path() {
        let p = pb("/a/b");
        // Method resolution through Deref<Target = Path>.
        fn takes_path(_: &Path) {}
        takes_path(&p);
        assert_eq!(p.file_name(), Some("b"));
    }

    #[test]
    fn serde_is_transparent_string() {
        let p = pb("/a/b");
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"/a/b\"");
        let back: PathBuf = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);

        let vp = VfsPath::from_wire_str(VfsId(3), "/x/y");
        let j = serde_json::to_string(&vp).unwrap();
        assert!(j.contains("\"/x/y\""), "got {j}");
        let back: VfsPath = serde_json::from_str(&j).unwrap();
        assert_eq!(back, vp);
    }

    #[test]
    fn vfs_path_display() {
        assert_eq!(VfsPath::root(VfsId::ROOT).to_string(), "/");
        assert_eq!(
            VfsPath::from_wire_str(VfsId::ROOT, "/a/b").to_string(),
            "/a/b"
        );
        assert_eq!(
            VfsPath::from_wire_str(VfsId(5), "/a/b").to_string(),
            "vfs://5:/a/b"
        );
    }

    #[test]
    fn vfs_path_strip_prefix_respects_vfs_id() {
        let a = VfsPath::from_wire_str(VfsId::ROOT, "/x");
        let b = VfsPath::from_wire_str(VfsId(2), "/x/y");
        assert!(!b.starts_with(&a));
        assert_eq!(b.strip_prefix(&a), None);
    }

    #[test]
    fn vfs_path_join() {
        let p = VfsPath::from_wire_str(VfsId::ROOT, "/home");
        let joined = p.join("user");
        assert_eq!(joined.path, PathBuf::from_wire_str("/home/user"));
        assert_eq!(joined.vfs_id, VfsId::ROOT);
    }

    #[test]
    fn vfs_path_parent() {
        let p = VfsPath::from_wire_str(VfsId::ROOT, "/home/user");
        let parent = p.parent().unwrap();
        assert_eq!(parent.path, PathBuf::from_wire_str("/home"));
    }
}

#[cfg(all(test, windows))]
mod typed_display_tests {
    use super::*;

    fn p(comps: &[&str]) -> PathBuf {
        PathBuf::from_components(comps.iter().copied())
    }

    #[test]
    fn typed_display_accepts_distinctive_absolute_shapes() {
        for (input, comps) in [
            (r"C:\Users\X", &["?", "C:", "Users", "X"][..]),
            ("c:/x", &["?", "C:", "x"][..]),
            ("C:", &["?", "C:"][..]),
            (r"C:\", &["?", "C:"][..]),
            (r"\\server\share", &["?", "UNC", "server", "share"][..]),
            (
                r"\\server\share\a",
                &["?", "UNC", "server", "share", "a"][..],
            ),
            (
                "//server/share/a",
                &["?", "UNC", "server", "share", "a"][..],
            ),
            (r"\\?\C:\x", &["?", "C:", "x"][..]),
            (
                r"\\?\UNC\server\share\a",
                &["?", "UNC", "server", "share", "a"][..],
            ),
        ] {
            assert_eq!(
                PathBuf::from_typed_display(input).as_ref(),
                Some(&p(comps)),
                "{input}"
            );
        }
    }

    #[test]
    fn typed_display_rejects_relative_and_exotic_shapes() {
        for input in [
            "foo",
            r"foo\bar",
            "C:foo",   // drive-relative — no per-drive cwd to resolve against
            "/unix/x", // unix absolute stays with the session shell
            r"\\server",
            r"\\.\COM1",
            "//?/C:/x",
            r"C:\a\..\b", // native decode would drop the `..`, not resolve it
        ] {
            assert_eq!(PathBuf::from_typed_display(input), None, "{input}");
        }
    }
}
