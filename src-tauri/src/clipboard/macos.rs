//! `NSPasteboard`: one item per file with its `public.file-url`, the text
//! on the first. A cut has no pasteboard form on macOS; it lives in the
//! record of whoever cut.

use std::path::PathBuf;
use std::sync::Arc;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSString, NSURL};
use tauri::AppHandle;

use super::{CopiedFiles, Stamp, error};
use crate::common::Error;

pub struct Clipboard;

impl Clipboard {
    pub fn new(_app: &AppHandle, _changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        Clipboard
    }

    pub async fn text(&self) -> Result<Option<String>, Error> {
        pasteboard(|pb| {
            Ok(pb
                .stringForType(unsafe { NSPasteboardTypeString })
                .map(|s| s.to_string()))
        })
        .await
    }

    pub async fn set_text(&self, text: String) -> Result<Stamp, Error> {
        pasteboard(move |pb| {
            pb.clearContents();
            if !pb.setString_forType(&NSString::from_str(&text), unsafe {
                NSPasteboardTypeString
            }) {
                return Err(error("the pasteboard refused the text"));
            }
            Ok(stamp_of(pb))
        })
        .await
    }

    pub async fn set_files(
        &self,
        paths: Vec<PathBuf>,
        text: String,
        _cut: bool,
    ) -> Result<Stamp, Error> {
        pasteboard(move |pb| {
            let mut items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
            for (i, path) in paths.iter().enumerate() {
                let Some(path) = path.to_str() else {
                    continue;
                };
                let url = NSURL::fileURLWithPath(&NSString::from_str(path));
                let Some(uri) = url.absoluteString() else {
                    continue;
                };
                let item = NSPasteboardItem::new();
                item.setString_forType(&uri, unsafe { NSPasteboardTypeFileURL });
                if i == 0 {
                    item.setString_forType(&NSString::from_str(&text), unsafe {
                        NSPasteboardTypeString
                    });
                }
                items.push(ProtocolObject::from_retained(item));
            }
            pb.clearContents();
            if !pb.writeObjects(&NSArray::from_retained_slice(&items)) {
                return Err(error("the pasteboard refused the files"));
            }
            Ok(stamp_of(pb))
        })
        .await
    }

    pub async fn files(&self) -> Result<Option<CopiedFiles>, Error> {
        pasteboard(|pb| {
            let Some(items) = pb.pasteboardItems() else {
                return Ok(None);
            };
            let paths: Vec<PathBuf> = items
                .iter()
                .filter_map(|item| item.stringForType(unsafe { NSPasteboardTypeFileURL }))
                .filter_map(|uri| NSURL::URLWithString(&uri))
                // Finder may hand over file reference URLs (`/.file/id=…`).
                .filter_map(|url| url.filePathURL())
                .filter_map(|url| url.path())
                .map(|path| PathBuf::from(path.to_string()))
                .collect();
            Ok((!paths.is_empty()).then_some(CopiedFiles { paths, cut: false }))
        })
        .await
    }

    pub async fn clear(&self) -> Result<(), Error> {
        pasteboard(|pb| {
            pb.clearContents();
            Ok(())
        })
        .await
    }

    pub async fn stamp(&self) -> Stamp {
        pasteboard(|pb| Ok(stamp_of(pb)))
            .await
            .unwrap_or(Stamp(u64::MAX))
    }
}

fn stamp_of(pb: &NSPasteboard) -> Stamp {
    Stamp(pb.changeCount() as u64)
}

/// Run `f` on the general pasteboard, off the async runtime: reading can
/// wait on another app that provides its data lazily.
async fn pasteboard<T: Send + 'static>(
    f: impl FnOnce(&NSPasteboard) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tauri::async_runtime::spawn_blocking(move || {
        autoreleasepool(|_| f(&NSPasteboard::generalPasteboard()))
    })
    .await
    .map_err(error)?
}
