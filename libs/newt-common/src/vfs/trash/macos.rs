//! `NSFileManager`'s trash: what Finder's Move to Trash does, Put Back
//! included, without asking Finder to do it.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;

use objc2_foundation::{
    NSFeatureUnsupportedError, NSFileManager, NSFileNoSuchFileError, NSFileWriteNoPermissionError,
    NSURL,
};

use super::refused;
use crate::{Error, ErrorKind};

pub(super) fn trash(path: &Path) -> Result<(), Error> {
    let is_dir = std::fs::symlink_metadata(path)?.is_dir();
    let raw = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::custom("path contains a NUL byte"))?;
    let raw = NonNull::new(raw.as_ptr().cast_mut()).expect("a CString is never null");
    // SAFETY: `raw` is a NUL-terminated path that outlives the call.
    let url = unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(raw, is_dir, None)
    };
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|err| {
            let message = err.localizedDescription().to_string();
            if err.domain().to_string() != "NSCocoaErrorDomain" {
                return Error::custom(message);
            }
            let code = err.code();
            if code == NSFeatureUnsupportedError {
                // A volume without a trash: a network share, a read-only
                // FAT stick, …
                refused(message)
            } else if code == NSFileNoSuchFileError {
                Error {
                    kind: ErrorKind::NotFound,
                    message,
                }
            } else if code == NSFileWriteNoPermissionError {
                Error {
                    kind: ErrorKind::PermissionDenied,
                    message,
                }
            } else {
                Error::custom(message)
            }
        })
}
