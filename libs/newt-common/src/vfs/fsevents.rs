//! Directory watching on FSEvents, for `LocalVfs::poll_changes` on macOS.
//!
//! notify's FSEvents backend can't ask for `kFSEventStreamCreateFlagWatchRoot`,
//! the flag that reports a directory above the watched one moving away (a
//! parent dragged to the Trash) — and watching each ancestor in its place
//! roots a stream at `/`, which FSEvents watches recursively.

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use fsevent_sys as fs;
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::Error;

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: fs::FSEventStreamRef, queue: *mut c_void);
}

/// What the stream's callback sees: the directory in both spellings
/// (FSEvents reports paths with symlinks resolved), and who to tell.
struct Watch {
    dirs: [PathBuf; 2],
    tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl Watch {
    fn concerns(&self, path: &Path, flags: fs::FSEventStreamEventFlags) -> bool {
        if flags
            & (fs::kFSEventStreamEventFlagRootChanged
                | fs::kFSEventStreamEventFlagUserDropped
                | fs::kFSEventStreamEventFlagKernelDropped)
            != 0
        {
            return true;
        }
        let rescan = fs::kFSEventStreamEventFlagMustScanSubDirs
            | fs::kFSEventStreamEventFlagMount
            | fs::kFSEventStreamEventFlagUnmount;
        self.dirs.iter().any(|dir| {
            if flags & rescan != 0 {
                dir.starts_with(path) || path.starts_with(dir)
            } else {
                path == dir || path.parent() == Some(dir)
            }
        })
    }
}

/// A started stream; dropping it stops it. FSEvents frees the `Watch` once
/// no callback can run (`release_watch`).
struct Stream(fs::FSEventStreamRef);

// SAFETY: FSEvents' stream functions may be called from any thread; the
// callbacks run on the dispatch queue.
unsafe impl Send for Stream {}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            fs::FSEventStreamStop(self.0);
            fs::FSEventStreamInvalidate(self.0);
            fs::FSEventStreamRelease(self.0);
        }
    }
}

/// One serial queue for every stream's callbacks, which only send on a
/// channel.
fn queue() -> *mut c_void {
    static QUEUE: OnceLock<usize> = OnceLock::new();
    *QUEUE.get_or_init(|| unsafe {
        dispatch_queue_create(c"newt.fsevents".as_ptr(), std::ptr::null()) as usize
    }) as *mut c_void
}

/// A watch on a directory, started: it reports what happens from here on.
pub struct DirWatch {
    _stream: Stream,
    rx: oneshot::Receiver<()>,
}

impl DirWatch {
    /// Watch for an entry of `dir` being added, removed, renamed or
    /// modified, or `dir` — or a directory above it — being moved, deleted
    /// or unmounted. `canonical` is `dir` with symlinks resolved.
    pub fn start(dir: &Path, canonical: PathBuf) -> Result<Self, Error> {
        let (tx, rx) = oneshot::channel();
        let watch = Box::new(Watch {
            dirs: [dir.to_path_buf(), canonical],
            tx: Mutex::new(Some(tx)),
        });
        let paths =
            CFArray::from_CFTypes(&[CFString::new(dir.to_str().expect("VFS paths are UTF-8"))]);
        let context = fs::FSEventStreamContext {
            version: 0,
            info: Box::into_raw(watch) as *mut c_void,
            retain: None,
            release: Some(release_watch),
            copy_description: None,
        };
        let stream = unsafe {
            let stream = Stream(fs::FSEventStreamCreate(
                std::ptr::null_mut(),
                callback,
                &context,
                paths.as_concrete_TypeRef() as fs::core_foundation::CFArrayRef,
                fs::kFSEventStreamEventIdSinceNow,
                0.0,
                fs::kFSEventStreamCreateFlagFileEvents
                    | fs::kFSEventStreamCreateFlagNoDefer
                    | fs::kFSEventStreamCreateFlagWatchRoot,
            ));
            FSEventStreamSetDispatchQueue(stream.0, queue());
            if fs::FSEventStreamStart(stream.0) == 0 {
                return Err(Error::custom(format!(
                    "Couldn't watch {} for changes",
                    dir.display()
                )));
            }
            stream
        };
        Ok(DirWatch {
            _stream: stream,
            rx,
        })
    }

    pub async fn changed(self) {
        let _ = self.rx.await;
    }
}

extern "C" fn release_watch(info: *const c_void) {
    drop(unsafe { Box::from_raw(info as *mut Watch) });
}

extern "C" fn callback(
    _stream: fs::FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const fs::FSEventStreamEventFlags,
    _ids: *const fs::FSEventStreamEventId,
) {
    // SAFETY: `info` is the `Watch` the stream was made with, alive until
    // `release_watch`; without `kFSEventStreamCreateFlagUseCFTypes`, `paths`
    // is `count` C strings, and `flags` has `count` entries.
    let (watch, paths, flags) = unsafe {
        (
            &*(info as *const Watch),
            std::slice::from_raw_parts(paths as *const *const c_char, count),
            std::slice::from_raw_parts(flags, count),
        )
    };
    let concerned = paths.iter().zip(flags).any(|(&path, &flags)| {
        let path = Path::new(OsStr::from_bytes(
            unsafe { CStr::from_ptr(path) }.to_bytes(),
        ));
        watch.concerns(path, flags)
    });
    if concerned && let Some(tx) = watch.tx.lock().take() {
        let _ = tx.send(());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    async fn wakes(dir: &Path, change: impl FnOnce()) {
        let watch = DirWatch::start(dir, std::fs::canonicalize(dir).unwrap()).unwrap();
        change();
        tokio::time::timeout(Duration::from_secs(10), watch.changed())
            .await
            .expect("no change reported");
    }

    #[tokio::test]
    async fn an_entry_added() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        wakes(&dir, || std::fs::write(dir.join("x"), b"").unwrap()).await;
    }

    #[tokio::test]
    async fn a_parent_moved_away() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p/a");
        std::fs::create_dir_all(&dir).unwrap();
        let (from, to) = (tmp.path().join("p"), tmp.path().join("q"));
        wakes(&dir, || std::fs::rename(&from, &to).unwrap()).await;
    }

    #[tokio::test]
    async fn a_parent_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p/a");
        std::fs::create_dir_all(&dir).unwrap();
        let parent = tmp.path().join("p");
        wakes(&dir, || std::fs::remove_dir_all(&parent).unwrap()).await;
    }

    #[test]
    fn entries_of_the_directory_only() {
        let (tx, _rx) = oneshot::channel();
        let watch = Watch {
            dirs: ["/tmp/a".into(), "/private/tmp/a".into()],
            tx: Mutex::new(Some(tx)),
        };
        let created = fs::kFSEventStreamEventFlagItemCreated;
        assert!(watch.concerns(Path::new("/private/tmp/a/x"), created));
        assert!(watch.concerns(Path::new("/tmp/a"), fs::kFSEventStreamEventFlagItemRemoved));
        assert!(!watch.concerns(Path::new("/private/tmp/a/b/x"), created));
        assert!(!watch.concerns(Path::new("/private/tmp/ab"), created));
        assert!(watch.concerns(Path::new("/tmp"), fs::kFSEventStreamEventFlagRootChanged));
        assert!(watch.concerns(Path::new("/private"), fs::kFSEventStreamEventFlagUnmount));
    }
}
