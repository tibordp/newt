use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSControlStateValueOn, NSModalResponseOK, NSOpenPanel, NSOpenSavePanelDelegate,
    NSPopUpButton, NSStackView, NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{
    NSArray, NSFileManager, NSObject, NSObjectProtocol, NSRect, NSString, NSURL, ns_string,
};
use objc2_uniform_type_identifiers::{UTType, UTTypeApplicationBundle};

use super::OpenWithApp;

fn content_type(name: &str) -> Option<Retained<UTType>> {
    let extension = Path::new(name).extension()?.to_str()?;
    UTType::typeWithFilenameExtension(&NSString::from_str(extension))
}

fn path_of(url: &NSURL) -> Option<String> {
    url.path().map(|p| p.to_string())
}

fn app_name(path: &str) -> String {
    let name = NSFileManager::defaultManager()
        .displayNameAtPath(&NSString::from_str(path))
        .to_string();
    name.strip_suffix(".app")
        .map(str::to_string)
        .unwrap_or(name)
}

pub fn apps_for(name: &str) -> Vec<OpenWithApp> {
    let Some(content_type) = content_type(name) else {
        return Vec::new();
    };
    let workspace = NSWorkspace::sharedWorkspace();
    let default = workspace
        .URLForApplicationToOpenContentType(&content_type)
        .and_then(|url| path_of(&url));
    workspace
        .URLsForApplicationsToOpenContentType(&content_type)
        .iter()
        .filter_map(|url| path_of(&url))
        .map(|path| OpenWithApp {
            name: app_name(&path),
            is_default: Some(&path) == default.as_ref(),
            id: path,
        })
        .collect()
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, String> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("{} is not a valid path", path.display()))?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

fn open_in(file: &NSURL, app: &NSURL) {
    NSWorkspace::sharedWorkspace().openURLs_withApplicationAtURL_configuration_completionHandler(
        &NSArray::from_slice(&[file]),
        app,
        &NSWorkspaceOpenConfiguration::configuration(),
        None,
    );
}

pub fn open_with(path: &Path, app: &str) -> Result<(), String> {
    open_in(
        &*file_url(path)?,
        &NSURL::fileURLWithPath(&NSString::from_str(app)),
    );
    Ok(())
}

pub fn choose(window: &tauri::WebviewWindow, path: PathBuf) -> Result<(), String> {
    let ns_window = window.ns_window().map_err(|e| e.to_string())? as usize;
    window
        .run_on_main_thread(move || {
            let mtm = MainThreadMarker::new().expect("run_on_main_thread runs on the main thread");
            // SAFETY: the window this command came from outlives the
            // sheet attached to it.
            let window = unsafe { &*(ns_window as *const NSWindow) };
            if let Err(e) = present_chooser(mtm, window, &path) {
                log::warn!("open with: {e}");
            }
        })
        .map_err(|e| e.to_string())
}

/// Finder's "Open With ▸ Other…": the open panel on Applications, with
/// only the applications recommended for the file enabled unless "All
/// Applications" is picked, and "Always Open With" to make the choice the
/// default for files of its type.
fn present_chooser(mtm: MainThreadMarker, window: &NSWindow, path: &Path) -> Result<(), String> {
    let file = file_url(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let recommended: HashSet<String> = apps_for(&name).into_iter().map(|app| app.id).collect();

    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(ns_string!("/Applications"))));
    // SAFETY: a static the framework defines.
    panel.setAllowedContentTypes(&NSArray::from_slice(&[unsafe { UTTypeApplicationBundle }]));
    panel.setPrompt(Some(ns_string!("Open")));
    panel.setMessage(Some(&NSString::from_str(&format!(
        "Choose an application to open the document “{name}”."
    ))));

    let delegate = ChooserDelegate::new(mtm, recommended);
    // SAFETY: the completion handler below keeps the delegate alive for as
    // long as the panel is up.
    unsafe { panel.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };

    let label = NSTextField::labelWithString(ns_string!("Enable:"), mtm);
    let filter =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    filter.addItemWithTitle(ns_string!("Recommended Applications"));
    filter.addItemWithTitle(ns_string!("All Applications"));
    // Nothing recommended would leave nothing to choose.
    if delegate.ivars().recommended.is_empty() {
        filter.selectItemAtIndex(1);
        delegate.ivars().recommended_only.set(false);
    }
    // SAFETY: the delegate implements `filterChanged:`.
    unsafe {
        filter.setTarget(Some(&delegate));
        filter.setAction(Some(sel!(filterChanged:)));
    }
    let always = unsafe {
        NSButton::checkboxWithTitle_target_action(ns_string!("Always Open With"), None, None, mtm)
    };
    let enable_row = NSStackView::stackViewWithViews(
        &NSArray::from_slice(&[label.as_ref() as &NSView, filter.as_ref()]),
        mtm,
    );
    let accessory = NSStackView::stackViewWithViews(
        &NSArray::from_slice(&[enable_row.as_ref() as &NSView, always.as_ref()]),
        mtm,
    );
    accessory.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    accessory.setEdgeInsets(objc2_foundation::NSEdgeInsets {
        top: 8.0,
        left: 8.0,
        bottom: 8.0,
        right: 8.0,
    });
    panel.setAccessoryView(Some(&accessory));
    panel.setAccessoryViewDisclosed(true);

    let handler_panel = panel.clone();
    let handler = RcBlock::new(move |response| {
        let _keep_alive = &delegate;
        if response != NSModalResponseOK {
            return;
        }
        let Some(app) = handler_panel.URL() else {
            return;
        };
        if always.state() == NSControlStateValueOn {
            NSWorkspace::sharedWorkspace()
                .setDefaultApplicationAtURL_toOpenContentTypeOfFileAtURL_completionHandler(
                    &app, &file, None,
                );
        }
        open_in(&file, &app);
    });
    panel.beginSheetModalForWindow_completionHandler(window, &handler);
    Ok(())
}

struct ChooserIvars {
    /// Paths of the applications recommended for the file.
    recommended: HashSet<String>,
    recommended_only: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ChooserIvars]
    struct ChooserDelegate;

    unsafe impl NSObjectProtocol for ChooserDelegate {}

    unsafe impl NSOpenSavePanelDelegate for ChooserDelegate {
        #[unsafe(method(panel:shouldEnableURL:))]
        fn should_enable(&self, _sender: &AnyObject, url: &NSURL) -> Bool {
            let Some(path) = path_of(url) else {
                return Bool::NO;
            };
            // Folders stay enabled so the panel can be navigated.
            Bool::new(
                !path.to_lowercase().ends_with(".app")
                    || !self.ivars().recommended_only.get()
                    || self.ivars().recommended.contains(&path),
            )
        }
    }

    impl ChooserDelegate {
        #[unsafe(method(filterChanged:))]
        fn filter_changed(&self, sender: &NSPopUpButton) {
            self.ivars()
                .recommended_only
                .set(sender.indexOfSelectedItem() == 0);
            if let Some(panel) = sender.window() {
                // SAFETY: the popup lives in the open panel's accessory view.
                let _: () = unsafe { msg_send![&*panel, validateVisibleColumns] };
            }
        }
    }
);

impl ChooserDelegate {
    fn new(mtm: MainThreadMarker, recommended: HashSet<String>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ChooserIvars {
            recommended,
            recommended_only: Cell::new(true),
        });
        unsafe { msg_send![super(this), init] }
    }
}
