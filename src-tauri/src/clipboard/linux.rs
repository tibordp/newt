//! GTK's clipboard, the same on X11 and Wayland: files in each format a
//! Linux file manager reads (Nautilus, Nemo and Thunar read GNOME's, Caja
//! MATE's twin of it, Dolphin the URI list and KDE's cut marker), text for
//! everything else, and `owner-change` for word of other apps taking it.
//! GTK lives on the main thread, so every call is made there.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gtk::gdk;
use gtk::prelude::*;
use tauri::AppHandle;
use tokio::sync::oneshot;

use super::{CopiedFiles, Stamp, error, formats};
use crate::common::Error;

const GNOME_FILES: &str = "x-special/gnome-copied-files";
const MATE_FILES: &str = "x-special/mate-copied-files";
const URI_LIST: &str = "text/uri-list";
const KDE_CUT: &str = "application/x-kde-cutselection";
const TEXT_TARGETS: [&str; 5] = [
    "UTF8_STRING",
    "text/plain;charset=utf-8",
    "text/plain",
    "STRING",
    "TEXT",
];

pub struct Clipboard {
    app: AppHandle,
    /// The stamp: bumped by each of Newt's writes, and again when what it
    /// wrote is replaced (see `Served`).
    writes: Arc<AtomicU64>,
    changed: Arc<dyn Fn() + Send + Sync>,
}

/// Owned by the callback that serves what Newt put on the clipboard. GTK
/// drops that callback once anything replaces the content — another app,
/// another session's write, or this process's own webview copying text,
/// which leaves the process the owner — so its drop is the one sure sign
/// the content is gone.
struct Served {
    writes: Arc<AtomicU64>,
    changed: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.writes.fetch_add(1, Ordering::SeqCst);
        (self.changed)();
    }
}

/// What a target is served from.
enum Serve {
    /// The GNOME/MATE copied-files body.
    Copied,
    List,
    /// KDE's cut marker.
    Cut,
    Text,
}

fn clipboard() -> gtk::Clipboard {
    gtk::Clipboard::get(&gdk::SELECTION_CLIPBOARD)
}

impl Clipboard {
    pub fn new(app: &AppHandle, changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        let notify = changed.clone();
        let connected = app.run_on_main_thread(move || {
            clipboard().connect_local("owner-change", false, move |_| {
                notify();
                None
            });
        });
        if let Err(e) = connected {
            log::warn!("clipboard: no change listener: {e}");
        }
        Self {
            app: app.clone(),
            writes: Arc::default(),
            changed,
        }
    }

    pub async fn text(&self) -> Result<Option<String>, Error> {
        self.on_main(|tx| {
            clipboard().request_text(move |_, text| {
                let _ = tx.send(text.map(str::to_string));
            });
        })
        .await
    }

    pub async fn set_text(&self, text: String) -> Result<Stamp, Error> {
        let targets = TEXT_TARGETS
            .iter()
            .map(|&target| (target, Serve::Text))
            .collect();
        self.put(targets, String::new(), String::new(), text).await
    }

    pub async fn set_files(
        &self,
        paths: Vec<PathBuf>,
        text: String,
        cut: bool,
    ) -> Result<Stamp, Error> {
        let uris = formats::file_uris(&paths);
        let mut targets = vec![
            (GNOME_FILES, Serve::Copied),
            (MATE_FILES, Serve::Copied),
            (URI_LIST, Serve::List),
        ];
        targets.extend(TEXT_TARGETS.iter().map(|&target| (target, Serve::Text)));
        if cut {
            targets.push((KDE_CUT, Serve::Cut));
        }
        let copied = formats::copied_files(cut, &uris);
        let list = formats::uri_list(&uris);
        self.put(targets, copied, list, text).await
    }

    pub async fn files(&self) -> Result<Option<CopiedFiles>, Error> {
        for target in [GNOME_FILES, MATE_FILES] {
            if let Some(body) = self.contents(target).await?
                && let Some((uris, cut)) = formats::parse_copied_files(&body)
            {
                return Ok(local(&uris, cut));
            }
        }
        let Some(body) = self.contents(URI_LIST).await? else {
            return Ok(None);
        };
        let cut = self
            .contents(KDE_CUT)
            .await?
            .is_some_and(|marker| marker.first() == Some(&b'1'));
        Ok(local(&formats::parse_uri_list(&body), cut))
    }

    pub async fn clear(&self) -> Result<(), Error> {
        self.on_main(|tx| {
            clipboard().clear();
            let _ = tx.send(());
        })
        .await
    }

    pub async fn stamp(&self) -> Stamp {
        Stamp(self.writes.load(Ordering::SeqCst))
    }

    /// Offer `targets`, each served from `copied`, `list` or `text` as its
    /// `Serve` says, until something replaces them.
    async fn put(
        &self,
        targets: Vec<(&'static str, Serve)>,
        copied: String,
        list: String,
        text: String,
    ) -> Result<Stamp, Error> {
        let served = Served {
            writes: self.writes.clone(),
            changed: self.changed.clone(),
        };
        let writes = self.writes.clone();
        self.on_main(move |tx| {
            let entries: Vec<gtk::TargetEntry> = targets
                .iter()
                .enumerate()
                .map(|(i, (target, _))| {
                    gtk::TargetEntry::new(target, gtk::TargetFlags::empty(), i as u32)
                })
                .collect();
            let set = clipboard().set_with_data(&entries, move |_, data, info| {
                let _served = &served;
                let target = data.target();
                match targets.get(info as usize).map(|(_, serve)| serve) {
                    Some(Serve::Copied) => data.set(&target, 8, copied.as_bytes()),
                    Some(Serve::List) => data.set(&target, 8, list.as_bytes()),
                    Some(Serve::Cut) => data.set(&target, 8, b"1"),
                    Some(Serve::Text) => {
                        data.set_text(&text);
                    }
                    None => {}
                }
            });
            // Counted after GTK has dropped what served the content before.
            let _ = tx.send(set.then(|| Stamp(writes.fetch_add(1, Ordering::SeqCst) + 1)));
        })
        .await?
        .ok_or_else(|| error("GTK refused the content"))
    }

    /// The clipboard's contents as `target`, if it offers them.
    async fn contents(&self, target: &'static str) -> Result<Option<Vec<u8>>, Error> {
        self.on_main(move |tx| {
            clipboard().request_contents(&gdk::Atom::intern(target), move |_, data| {
                let _ = tx.send((data.length() > 0).then(|| data.data()));
            });
        })
        .await
    }

    /// Run `f` on the main thread, and wait for what it sends back.
    async fn on_main<T: Send + 'static>(
        &self,
        f: impl FnOnce(oneshot::Sender<T>) + Send + 'static,
    ) -> Result<T, Error> {
        let (tx, rx) = oneshot::channel();
        self.app.run_on_main_thread(move || f(tx)).map_err(error)?;
        rx.await.map_err(|_| error("GTK didn't answer"))
    }
}

/// The local files among `uris`; `None` when there are none (only
/// `sftp://` and the like, which Newt can't reach as paths).
fn local(uris: &[String], cut: bool) -> Option<CopiedFiles> {
    let paths = formats::local_paths(uris);
    (!paths.is_empty()).then_some(CopiedFiles { paths, cut })
}
