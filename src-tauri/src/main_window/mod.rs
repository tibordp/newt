#[cfg(windows)]
pub mod drives;
#[cfg(windows)]
pub mod elevate;
#[cfg(target_os = "macos")]
pub mod menu;
pub mod pane;
pub mod session;
#[cfg(windows)]
pub mod shell_menu;
pub mod terminal;
#[cfg(windows)]
pub mod win_proc;
#[cfg(windows)]
pub mod wsl_launch;

use newt_common::file_reader::FileReader;
use newt_common::filesystem::{Filesystem, ShellService, UserGroup};
use newt_common::operation::{OperationId, OperationProgress, OperationsClient};
use newt_common::terminal::TerminalClient;
use newt_common::terminal::TerminalHandle;
use newt_common::vfs::{MountedVfsInfo, VfsId, VfsPath, all_descriptors, lookup_descriptor};
use parking_lot::{Mutex, RwLock, RwLockWriteGuard};
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use std::cmp::PartialOrd;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use std::future::Future;
use std::sync::Arc;
use tauri::Manager;
use tauri::State;
use tauri::WebviewWindow;
use tauri::Wry;

use crate::GlobalContext;
use crate::common::Error;
use crate::common::UpdatePublisher;
use crate::main_window::session::VfsInfo;

use self::pane::Pane;
use self::session::Session;
use self::terminal::Terminal;

pub use self::session::{
    AgentResolver, ConnectionState, ConnectionStatus, ConnectionTarget, DirectCopyPlan, SpawnSpec,
    TauriAgentResolver, docker_direct_copy_plan, docker_transport_cmd, kube_transport_cmd,
    podman_direct_copy_plan, podman_transport_cmd, ssh_transport_cmd,
};

#[derive(Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct DisplayOptionsInner {
    pub show_hidden: bool,
    pub active_pane: PaneHandle,
    pub active_terminal: Option<TerminalHandle>,
    pub panes_focused: bool,
    pub terminal_panel_visible: bool,
}

#[derive(Default, Clone)]
pub struct DisplayOptions(pub Arc<RwLock<DisplayOptionsInner>>);

impl Default for DisplayOptionsInner {
    fn default() -> Self {
        Self {
            show_hidden: false,
            active_pane: PaneHandle(0),
            active_terminal: None,
            panes_focused: true,
            terminal_panel_visible: false,
        }
    }
}

impl serde::Serialize for DisplayOptions {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(
    Default,
    Debug,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Clone,
    Copy,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
)]
pub struct PaneHandle(usize);

impl PaneHandle {
    pub fn left() -> Self {
        PaneHandle(0)
    }

    pub fn right() -> Self {
        PaneHandle(1)
    }
}

#[derive(Clone)]
pub struct Panes(Arc<RwLock<Vec<Arc<Pane>>>>);

impl Default for Panes {
    fn default() -> Self {
        Self::new()
    }
}

impl Panes {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Vec::new())))
    }

    pub fn add(&self, pane: Pane) {
        let mut lock = self.0.write();
        lock.push(Arc::new(pane));
    }

    pub fn get(&self, handle: PaneHandle) -> Option<Arc<Pane>> {
        self.0.read().get(handle.0).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Pane>> {
        self.0.read().clone()
    }

    pub fn clear(&self) {
        self.0.write().clear();
    }
}

impl serde::Serialize for Panes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let locked = self.0.read();
        let mut seq = serializer.serialize_seq(Some(locked.len()))?;
        for e in locked.iter() {
            seq.serialize_element(&**e)?;
        }
        seq.end()
    }
}

#[derive(Clone)]
pub struct Terminals(Arc<RwLock<HashMap<TerminalHandle, Arc<Terminal>>>>);

impl Default for Terminals {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminals {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(HashMap::new())))
    }

    pub fn len(&self) -> usize {
        self.0.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.read().is_empty()
    }

    pub fn get(&self, handle: TerminalHandle) -> Option<Arc<Terminal>> {
        self.0.read().get(&handle).cloned()
    }

    pub fn insert(&self, handle: TerminalHandle, terminal: Terminal) -> Arc<Terminal> {
        let mut lock = self.0.write();
        let term = Arc::new(terminal);
        lock.insert(handle, term.clone());
        term
    }

    pub fn remove(&self, handle: TerminalHandle) -> Option<Arc<Terminal>> {
        self.0.write().remove(&handle)
    }

    pub fn first_handle(&self) -> Option<TerminalHandle> {
        self.0.read().keys().copied().min_by_key(|h| h.0)
    }

    pub fn handles_sorted(&self) -> Vec<TerminalHandle> {
        let mut handles: Vec<_> = self.0.read().keys().copied().collect();
        handles.sort_by_key(|h| h.0);
        handles
    }

    pub fn clear(&self) {
        self.0.write().clear();
    }
}

impl serde::Serialize for Terminals {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let lock = self.0.read();

        let mut seq = serializer.serialize_map(Some(lock.len()))?;
        for e in lock.iter() {
            seq.serialize_entry(&e.0, &**e.1)?;
        }
        seq.end()
    }
}

#[derive(Clone, serde::Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Scanning,
    Running,
    Completed,
    Failed,
    Cancelled,
    WaitingForInput,
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct OperationIssueInfo {
    pub issue_id: u64,
    pub message: String,
    pub detail: Option<String>,
    pub actions: Vec<newt_common::operation::IssueAction>,
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct OperationState {
    pub id: OperationId,
    pub kind: String,
    pub description: String,
    pub total_bytes: Option<u64>,
    pub total_items: Option<u64>,
    pub bytes_done: u64,
    pub items_done: u64,
    pub current_item: String,
    pub status: OperationStatus,
    pub error: Option<String>,
    pub issue: Option<OperationIssueInfo>,
    pub backgrounded: bool,
    /// Running totals from the scanning/planning phase.
    pub scanning_items: Option<u64>,
    pub scanning_bytes: Option<u64>,
}

type OperationCallback = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
pub struct Operations {
    pub state: Arc<RwLock<HashMap<OperationId, OperationState>>>,
    callbacks: Arc<Mutex<HashMap<OperationId, OperationCallback>>>,
}

impl Default for Operations {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(HashMap::new())),
            callbacks: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Operations {
    pub fn foreground_operation_id(&self) -> Option<OperationId> {
        self.state
            .read()
            .values()
            .filter(|op| !op.backgrounded)
            .min_by_key(|op| op.id)
            .map(|op| op.id)
    }

    pub fn register_completion_callback(&self, id: OperationId, cb: Box<dyn FnOnce() + Send>) {
        self.callbacks.lock().insert(id, cb);
    }

    /// If a completion callback is registered for `id`, remove and return it.
    fn take_callback(&self, id: OperationId) -> Option<Box<dyn FnOnce() + Send>> {
        self.callbacks.lock().remove(&id)
    }

    pub fn clear(&self) {
        self.state.write().clear();
        self.callbacks.lock().clear();
    }
}

impl serde::Serialize for Operations {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let lock = self.state.read();
        let mut map = serializer.serialize_map(Some(lock.len()))?;
        for (k, v) in lock.iter() {
            map.serialize_entry(&k.to_string(), v)?;
        }
        map.end()
    }
}

// ---------------------------------------------------------------------------
// VfsProgressState — host-side mirror of the VFS progress channel
// ---------------------------------------------------------------------------

/// VfsId-keyed map of progress entries pushed by VFSes (e.g. SearchVfs's
/// walker). Lives on `MainWindowState`; the frontend reads it for the
/// pane status bar. Mutated by `LocalProgressSink` from any context;
/// readers serialize through the inner `RwLock`.
#[derive(Clone, Default)]
pub struct VfsProgressState(Arc<RwLock<HashMap<VfsId, newt_common::vfs::VfsProgress>>>);

impl VfsProgressState {
    pub fn clear_for(&self, vfs_id: VfsId) {
        self.0.write().remove(&vfs_id);
    }
}

impl serde::Serialize for VfsProgressState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let lock = self.0.read();
        let mut map = serializer.serialize_map(Some(lock.len()))?;
        for (k, v) in lock.iter() {
            map.serialize_entry(&k.to_string(), v)?;
        }
        map.end()
    }
}

/// Rolling transcript of counter-less progress stage lines — in practice,
/// agent-mount connection/bootstrap logs. Rendered live by the Connect
/// dialog while a pane mount is in flight (mount progress is scoped to the
/// *new* VfsId, which no pane shows yet, so without this the log would be
/// invisible). Cleared when a new connect/mount begins.
#[derive(Clone, Default)]
pub struct MountLogState(Arc<RwLock<Vec<String>>>);

impl MountLogState {
    pub fn clear(&self) {
        self.0.write().clear();
    }

    fn push(&self, line: &str) {
        let mut lock = self.0.write();
        if lock.last().map(String::as_str) == Some(line) {
            return;
        }
        lock.push(line.to_string());
        let overflow = lock.len().saturating_sub(200);
        if overflow > 0 {
            lock.drain(..overflow);
        }
    }
}

impl serde::Serialize for MountLogState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

/// Concrete `VfsProgressSink` used on the host: writes into
/// `VfsProgressState` and triggers a publish so the frontend sees the
/// update. Used both for local-mode VFSes (which call it directly via a
/// `ScopedReporter` constructed by the manager) and for remote-mode
/// VFSes (whose reports arrive over `API_VFS_PROGRESS` and are
/// forwarded into the same sink by `HostDispatcher::notify`).
pub struct LocalProgressSink {
    state: VfsProgressState,
    mount_log: MountLogState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
}

impl LocalProgressSink {
    pub fn new(
        state: VfsProgressState,
        mount_log: MountLogState,
        publisher: Arc<UpdatePublisher<MainWindowState>>,
    ) -> Self {
        Self {
            state,
            mount_log,
            publisher,
        }
    }
}

impl newt_common::vfs::VfsProgressSink for LocalProgressSink {
    fn report(&self, vfs_id: VfsId, progress: Option<newt_common::vfs::VfsProgress>) {
        {
            let mut map = self.state.0.write();
            match progress {
                Some(p) => {
                    // Counter-less stages are log lines (agent-mount
                    // connect/bootstrap); counted ones are live progress
                    // (search walkers etc.) and would spam a transcript.
                    if p.processed.is_none() && p.total.is_none() {
                        self.mount_log.push(&p.stage);
                    }
                    map.insert(vfs_id, p);
                }
                None => {
                    map.remove(&vfs_id);
                }
            }
        }
        // Publish is best-effort; if it fails (no subscribers) the
        // next state mutation will resync anyway.
        let _ = self.publisher.publish();
    }
}

/// Which delete-confirmation dialog to show, and which outcomes it offers.
#[derive(Clone, Copy, PartialEq, serde::Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum DeleteConfirmMode {
    /// Move to Trash (primary) / Delete Permanently / Cancel.
    Trash,
    /// Delete (destructive primary) / Cancel.
    Permanent,
    /// Trash preference is on but the VFS has no trash: explain that the
    /// items will be deleted permanently. Delete Permanently / Cancel.
    TrashUnavailable,
}

/// Pack-dialog defaults sourced from `ArchivePreferences`.
#[derive(Clone, serde::Serialize, specta::Type)]
pub struct ArchiveDialogDefaults {
    pub format: newt_common::operation::ArchiveFormat,
    pub preserve_symlinks: bool,
    pub zip_level: i32,
    pub gzip_level: i32,
    pub xz_level: i32,
    pub zstd_level: i32,
}

/// Extended-properties section of the Properties dialog. The sheet is
/// fetched after the dialog opens (open-then-fill): the modal starts in
/// `Loading` and a spawned task patches it to `Loaded`/`Failed` once the
/// per-file sheets have been fetched and folded. `Hidden` when the VFS
/// has no extended properties.
#[derive(Clone, serde::Serialize, specta::Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PropertySheetState {
    Hidden,
    Loading,
    Loaded {
        sheet: newt_common::vfs::PropertySheet,
    },
    Failed {
        error: String,
    },
}

// One short-lived instance exists at a time; the size spread between
// variants doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, serde::Serialize, specta::Type)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ModalDataKind {
    CreateDirectory {
        path: VfsPath,
    },
    CreateFile {
        path: VfsPath,
        open_editor: bool,
    },
    Properties {
        paths: Vec<VfsPath>,
        name: String,
        size: Option<u64>,
        /// Bytes allocated on disk (`File::allocated_size`); summed across
        /// a multi-selection the same way `size` is.
        allocated_size: Option<u64>,
        /// Single-selection only, like the timestamps.
        hard_links: Option<u64>,
        inode: Option<u64>,
        device_id: Option<u64>,
        is_dir: bool,
        is_symlink: bool,
        symlink_target: Option<String>,
        /// Whether the VFS supports metadata changes (chmod/chown)
        can_set_metadata: bool,
        /// Bits that are ON in all selected files
        mode_set: u32,
        /// Bits that are OFF in all selected files
        mode_clear: u32,
        /// Whether any file has a mode at all
        has_mode: bool,
        owner: Option<UserGroup>,
        group: Option<UserGroup>,
        /// Owner UID (resolved from name if needed)
        owner_id: Option<u32>,
        /// Group GID (resolved from name if needed)
        group_id: Option<u32>,
        modified: Option<i64>,
        accessed: Option<i64>,
        created: Option<i64>,
        sheet: PropertySheetState,
        /// Volume stats + classification. `Some` only for a volume root
        /// (DirectoryProperties at a root, or the RootProperties dialog).
        fs_stats: Option<newt_common::filesystem::FsStats>,
    },
    Navigate {
        path: VfsPath,
        display_path: String,
    },
    Rename {
        base_path: VfsPath,
        name: String,
    },
    CopyMove {
        kind: String,
        sources: Vec<VfsPath>,
        destination: VfsPath,
        display_destination: String,
        summary: String,
        /// Single-source transfers offer a rename field prefilled with the
        /// source's leaf name; `None` (multi-selection) hides it.
        default_name: Option<String>,
        /// Sticky last-used preserve toggles, seeded from runtime state.
        defaults: crate::runtime_state::CopyMoveDefaults,
    },
    CreateArchive {
        sources: Vec<VfsPath>,
        /// Directory the archive lands in (the other pane); the dialog
        /// composes the final file path from this and the name field.
        destination: VfsPath,
        display_destination: String,
        summary: String,
        /// Suggested archive name, without extension.
        default_name: String,
        defaults: ArchiveDialogDefaults,
    },
    ConnectRemote {
        /// Pre-populated transport for the dialog. Empty `Ssh { host: "" }`
        /// when opened cold from the palette.
        initial: crate::connections::ConnectionKind,
        /// Session-dependent default scope: local sessions default to a new
        /// session window, remote sessions to a pane mount (the common
        /// reason to connect from inside a remote session is peeking into
        /// one of its containers).
        default_open_in: crate::connections::OpenIn,
    },
    MountSftp {
        host: String,
    },
    MountS3,
    /// Recursive-search dialog. Opened from a pane to mount a `SearchVfs`
    /// rooted at `path`. The pane navigates to the mount root on submit.
    Search {
        /// Search root, captured from the source pane at dialog-open time.
        /// When refining an existing search, this is that search's origin
        /// (the original root), not the search VFS itself.
        path: VfsPath,
        /// Pre-rendered display label for the root (so the dialog can
        /// show "Search in /home/foo" without re-resolving).
        display_path: String,
        /// Params of the search being refined (cmd+f inside a search) —
        /// the dialog opens pre-filled with these. `None` for a fresh
        /// search.
        prefill: Option<newt_common::vfs::search::SearchParams>,
        /// Sticky last-used toggles for a fresh search, seeded from runtime
        /// state. Ignored when `prefill` is present (refine restores instead).
        defaults: crate::runtime_state::SearchDefaults,
    },
    // specta's snake_case tokenizer splits `K8s` → `k_8s`; pin both ends to
    // the wire format serde emits.
    #[serde(rename = "mount_k8s")]
    #[specta(rename = "mount_k8s")]
    MountK8s {
        k8s_context: String,
    },
    /// Quick sort menu (keyboard-launched, anchored to the pane header).
    SortMenu {
        sorting: crate::main_window::pane::Sorting,
        /// The `appearance.folders_first` preference, so the menu can show
        /// and toggle it inline.
        folders_first: bool,
    },
    QuickConnect {
        connections: Vec<crate::connections::ConnectionProfile>,
        /// Ad-hoc (unsaved) targets, most-recent first, already filtered to
        /// exclude any that match a saved profile.
        recent_connections: Vec<crate::runtime_state::RecentConnection>,
    },
    SelectVfs {
        targets: Vec<VfsTarget>,
    },
    /// "Disconnect X:?" for the unmap-network-drive command. Constructed
    /// on Windows only; plain data, so no cfg (keeps bindings.ts
    /// host-independent without a gate).
    ConfirmUnmapDrive {
        /// Drive letter (`X:`).
        drive: String,
        /// The share it maps to (`\\server\share`), when recorded.
        target: Option<String>,
    },
    // Specta-visible, so compiled under the bindings feature too (keeps
    // bindings.ts host-independent). Constructed only on Windows; the
    // allow covers the off-Windows bindings build.
    #[cfg(any(windows, feature = "specta-bindings"))]
    #[cfg_attr(not(windows), allow(dead_code))]
    SelectWslDistro {
        distros: Vec<crate::discovery::wsl::WslDistro>,
    },
    HistoryNavigator {
        entries: Vec<pane::HistoryEntryView>,
        current_index: usize,
        /// Direction of the keypress that opened the overlay: -1 for back,
        /// +1 for forward. The overlay uses this to set the initial preview
        /// (one step in that direction, skipping dead entries).
        initial_direction: i32,
        /// When true, the overlay is opened as a persistent dialog: it stays
        /// open until explicitly dismissed (Esc / outside-click), Alt-up does
        /// not commit, blur does not abort, and per-entry delete buttons are
        /// shown. When false, the overlay behaves alt-tab style (the default
        /// alt-held mode).
        persistent: bool,
    },
    CommandPalette {
        category_filter: Option<String>,
    },
    HotPaths,
    Settings,
    ConfirmDelete {
        message: String,
        paths: Vec<VfsPath>,
        mode: DeleteConfirmMode,
    },
    UserCommandInput {
        command_index: usize,
        command_title: String,
        prompts: Vec<UserCommandPrompt>,
        confirms: Vec<String>,
    },
    Debug,
    ConnectionLog,
    About {
        version: String,
        git_revision: Option<String>,
        target_triple: String,
    },
}

#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct UserCommandPrompt {
    pub label: String,
    pub default: String,
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct ModalContext {
    pub pane_handle: Option<PaneHandle>,
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct ModalData {
    #[serde(flatten)]
    pub kind: ModalDataKind,
    pub context: ModalContext,
}

#[derive(Clone)]
pub struct ModalState(pub Arc<RwLock<Option<ModalData>>>);

impl Default for ModalState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(None)))
    }
}

impl serde::Serialize for ModalState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct DndFile {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct DndData {
    pub source_pane: PaneHandle,
    pub files: Vec<DndFile>,
    /// True once the drag has escalated to a native OS drag session.
    #[serde(skip)]
    pub outbound: bool,
    /// Monotonic id so async clears can't clobber a newer session.
    #[serde(skip)]
    pub generation: u64,
}

static DND_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl DndData {
    pub fn new(source_pane: PaneHandle, files: Vec<DndFile>) -> Self {
        Self {
            source_pane,
            files,
            outbound: false,
            generation: DND_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
pub struct DndState(pub Arc<RwLock<Option<DndData>>>);

impl Default for DndState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(None)))
    }
}

impl serde::Serialize for DndState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct VfsTarget {
    pub vfs_id: Option<VfsId>,
    pub type_name: String,
    pub display_name: String,
    /// Human-readable label for a mounted instance (e.g. hostname for
    /// SFTP, or the drive for a split-root entry).
    pub label: Option<String>,
    /// Dialog to open when user selects this unmounted VFS type.
    /// If None and vfs_id is None, the type supports auto-mount.
    pub mount_dialog: Option<String>,
    /// Specific root to land on. `Some` only for split-root VFSes
    /// (one target per drive); selecting it navigates straight there
    /// instead of the VFS's default `initial_path`.
    pub root: Option<newt_common::vfs::path::PathBuf>,
    /// Volume classification for a split-root entry (drive kind, label,
    /// UNC/subst target), recorded at mount time on the owning side.
    pub volume: Option<newt_common::vfs::VolumeInfo>,
    /// Free bytes on the target's volume. Always `None` at open — the
    /// selector opens instantly and a background fetch fills these in
    /// (a dead network drive must not stall the dropdown).
    pub available_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// Askpass — SSH password / host-key prompts via SSH_ASKPASS
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize, specta::Type)]
pub struct AskpassPrompt {
    pub prompt: String,
    pub is_secret: bool,
}

#[derive(Clone)]
pub struct AskpassState(pub Arc<RwLock<Option<AskpassPrompt>>>);

/// Host-side `AskpassProvider`: shows the prompt in the UI by writing to
/// `MainWindowState.askpass`, and waits on a oneshot fed by the
/// `askpass_respond` Tauri command.
pub struct TauriAskpassProvider {
    state: AskpassState,
    response_slot: Arc<parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
    publisher: Arc<crate::common::UpdatePublisher<MainWindowState>>,
}

impl TauriAskpassProvider {
    pub fn new(
        state: AskpassState,
        response_slot: Arc<
            parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<Option<String>>>>,
        >,
        publisher: Arc<crate::common::UpdatePublisher<MainWindowState>>,
    ) -> Self {
        Self {
            state,
            response_slot,
            publisher,
        }
    }
}

#[async_trait::async_trait]
impl newt_common::askpass::AskpassProvider for TauriAskpassProvider {
    async fn prompt(
        &self,
        req: newt_common::askpass::AskpassRequest,
    ) -> newt_common::askpass::AskpassResponse {
        let is_secret = newt_common::askpass::is_secret_prompt(&req);
        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.state.0.write() = Some(AskpassPrompt {
            prompt: req.prompt,
            is_secret,
        });
        *self.response_slot.lock() = Some(tx);
        let _ = self.publisher.publish();

        // Askpass prompts can fire from background work triggered in
        // any window (e.g. F3 viewer reading an encrypted ZIP entry),
        // but the dialog is rendered in the main window. Pull the main
        // window forward so the user can actually see and answer it.
        let window = self.publisher.window();
        let _ = window.unminimize();
        let _ = window.set_focus();

        let result = rx.await.unwrap_or(None);

        *self.state.0.write() = None;
        *self.response_slot.lock() = None;
        let _ = self.publisher.publish();

        newt_common::askpass::AskpassResponse(result)
    }
}

impl Default for AskpassState {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(None)))
    }
}

impl serde::Serialize for AskpassState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

#[derive(Clone)]
pub struct MainWindowState {
    pub connection_status: ConnectionState,
    pub askpass: AskpassState,
    pub panes: Panes,
    pub terminals: Terminals,
    pub modal: ModalState,
    pub dnd: DndState,
    pub display_options: DisplayOptions,
    pub operations: Operations,
    pub window_title: String,
    pub vfs_progress: VfsProgressState,
    pub mount_log: MountLogState,
    pub mount_summary: MountSummaryState,
}

/// Session-level facts about the mounted VFS set that the frontend needs
/// outside the selector modal. Refreshed on mount/unmount and at session
/// init; reset on disconnect.
#[derive(Clone, Default, serde::Serialize, specta::Type)]
pub struct MountSummary {
    /// Whether any mounted VFS is split-root (Windows drive letters) —
    /// gates the Shift+<drive> shortcut, independent of the host OS.
    pub has_split_root_vfs: bool,
}

#[derive(Clone, Default)]
pub struct MountSummaryState(pub Arc<RwLock<MountSummary>>);

impl serde::Serialize for MountSummaryState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.0.read().serialize(serializer)
    }
}

impl serde::Serialize for MainWindowState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        use serde::ser::SerializeStruct;
        let foreground_id = self.operations.foreground_operation_id();
        let mut s = serializer.serialize_struct("MainWindowState", 13)?;
        s.serialize_field("connection_status", &self.connection_status)?;
        s.serialize_field("askpass", &self.askpass)?;
        s.serialize_field("panes", &self.panes)?;
        s.serialize_field("terminals", &self.terminals)?;
        s.serialize_field("modal", &self.modal)?;
        s.serialize_field("dnd", &self.dnd)?;
        s.serialize_field("display_options", &self.display_options)?;
        s.serialize_field("operations", &self.operations)?;
        s.serialize_field("window_title", &self.window_title)?;
        s.serialize_field("foreground_operation_id", &foreground_id)?;
        s.serialize_field("vfs_progress", &self.vfs_progress)?;
        s.serialize_field("mount_log", &self.mount_log)?;
        s.serialize_field("mount_summary", &self.mount_summary)?;
        s.end()
    }
}

impl MainWindowState {
    fn new() -> Self {
        let display_options = DisplayOptions::default();

        Self {
            connection_status: ConnectionState::default(),
            askpass: AskpassState::default(),
            panes: Panes::new(),
            terminals: Terminals::new(),
            modal: ModalState::default(),
            dnd: DndState::default(),
            display_options,
            operations: Operations::default(),
            window_title: "Newt".to_string(),
            vfs_progress: VfsProgressState::default(),
            mount_log: MountLogState::default(),
            mount_summary: MountSummaryState::default(),
        }
    }

    pub fn other_pane(&self, handle: PaneHandle) -> Arc<Pane> {
        self.panes.get(PaneHandle(1 - handle.0)).unwrap()
    }

    pub async fn refresh(&self, force: bool) -> Result<(), Error> {
        for pane in self.panes.all() {
            pane.refresh(None, force).await?;
        }
        Ok(())
    }

    pub fn close_modal(&self) {
        *self.modal.0.write() = None;
    }

    pub fn activate_pane(&self, handle: PaneHandle) {
        let mut opts = self.display_options.0.write();
        opts.active_pane = handle;
        opts.panes_focused = true;
    }

    pub async fn as_other_pane(&self, handle: PaneHandle) -> Result<(), Error> {
        let other_pane = self.other_pane(handle);
        let pane = self.panes.get(handle).unwrap();

        pane.navigate_to(other_pane.path()).await?;

        Ok(())
    }

    pub fn toggle_hidden(&self) {
        {
            let mut display_options = self.display_options.0.write();
            display_options.show_hidden = !display_options.show_hidden;
        }

        for pane in self.panes.all() {
            pane.update_view_state();
        }
    }
}

/// Apply an `OperationProgress` update to the operations state map.
/// Used by both local progress forwarding and remote RPC notifications.
pub(crate) fn apply_operation_progress(
    operations: &Operations,
    progress: OperationProgress,
    keep_finished: bool,
) {
    let mut ops = operations.state.write();
    match progress {
        OperationProgress::Scanning {
            id,
            items_found,
            bytes_found,
        } => {
            if let Some(op) = ops.get_mut(&id) {
                op.scanning_items = Some(items_found);
                op.scanning_bytes = Some(bytes_found);
            }
        }
        OperationProgress::Prepared {
            id,
            total_bytes,
            total_items,
        } => {
            if let Some(op) = ops.get_mut(&id) {
                op.total_bytes = Some(total_bytes);
                op.total_items = Some(total_items);
                op.status = OperationStatus::Running;
            }
        }
        OperationProgress::Progress {
            id,
            bytes_done,
            items_done,
            current_item,
        } => {
            if let Some(op) = ops.get_mut(&id) {
                op.bytes_done = bytes_done;
                op.items_done = items_done;
                op.current_item = current_item;
                op.status = OperationStatus::Running;
                op.issue = None;
            }
        }
        OperationProgress::Completed { id } => {
            if keep_finished {
                if let Some(op) = ops.get_mut(&id) {
                    op.status = OperationStatus::Completed;
                    op.backgrounded = true;
                }
            } else {
                ops.remove(&id);
            }
            if let Some(cb) = operations.take_callback(id) {
                cb();
            }
        }
        OperationProgress::Failed { id, error } => {
            if let Some(op) = ops.get_mut(&id) {
                op.status = OperationStatus::Failed;
                op.error = Some(error);
            }
            operations.take_callback(id); // discard
        }
        OperationProgress::Cancelled { id } => {
            if keep_finished {
                if let Some(op) = ops.get_mut(&id) {
                    op.status = OperationStatus::Cancelled;
                    op.backgrounded = true;
                }
            } else {
                ops.remove(&id);
            }
            operations.take_callback(id); // discard
        }
        OperationProgress::Issue { id, issue } => {
            if let Some(op) = ops.get_mut(&id) {
                op.status = OperationStatus::WaitingForInput;
                op.issue = Some(OperationIssueInfo {
                    issue_id: issue.issue_id,
                    message: issue.message,
                    detail: issue.detail,
                    actions: issue.actions,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

pub enum MainWindowEvent {
    /// A pane navigated — check for stale archive mounts.
    PaneNavigated,
}

#[allow(dead_code)]
struct MainWindowContextInner {
    window: WebviewWindow,
    main_window_state: MainWindowState,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
    preferences: crate::preferences::PreferencesHandle,
    connection_target: ConnectionTarget,
    window_title: String,
    /// Per-pane initial paths from the CLI (`--cwd-left`, `--cwd-right`).
    /// `None` per slot means "use the connection's default" (cwd locally,
    /// `~` on remote). Only honoured during the initial connect; subsequent
    /// reconnects ignore them.
    initial_pane_paths: [Option<std::path::PathBuf>; 2],
    session: Arc<arc_swap::ArcSwap<Option<Session>>>,
    askpass_response: Arc<parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
    clipboard: RwLock<arboard::Clipboard>,
}

#[derive(Clone)]
pub struct MainWindowContext {
    inner: Arc<MainWindowContextInner>,
}

impl<'de> tauri::ipc::CommandArg<'de, Wry> for MainWindowContext {
    fn from_command(
        command: tauri::ipc::CommandItem<'de, Wry>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        let window = command.message.webview();
        let app_handle = window.app_handle();
        let s: State<GlobalContext> = app_handle.state();

        s.main_window(&window)
            .ok_or_else(|| tauri::ipc::InvokeError::from("window not yet initialized"))
    }
}

// `MainWindowContext` is server-side state (resolved via `CommandArg` from
// `GlobalContext.main_windows`), not a value the frontend supplies. Tell
// specta to skip it so it doesn't appear in the generated TS signatures.
impl specta::function::FunctionArg for MainWindowContext {
    fn to_datatype(_: &mut specta::TypeCollection) -> Option<specta::datatype::DataType> {
        None
    }
}

impl MainWindowContext {
    pub fn new(
        window: WebviewWindow,
        connection_target: ConnectionTarget,
        window_title: String,
        preferences: crate::preferences::PreferencesHandle,
        initial_pane_paths: [Option<std::path::PathBuf>; 2],
    ) -> Self {
        let mut global_state = MainWindowState::new();
        global_state.window_title = window_title.clone();
        global_state.display_options.0.write().show_hidden =
            preferences.load().appearance.show_hidden;
        let publisher = Arc::new(UpdatePublisher::new(
            window.clone(),
            "main_window",
            global_state.clone(),
        ));

        Self {
            inner: Arc::new(MainWindowContextInner {
                window,
                main_window_state: global_state,
                publisher,
                preferences,
                connection_target,
                window_title,
                initial_pane_paths,
                session: Arc::new(arc_swap::ArcSwap::from_pointee(None)),
                askpass_response: Arc::new(parking_lot::Mutex::new(None)),
                clipboard: RwLock::new(
                    arboard::Clipboard::new().expect("failed to initialize clipboard"),
                ),
            }),
        }
    }

    /// Per-pane initial paths from the CLI; consumed by session::connect.
    pub fn initial_pane_paths(&self) -> &[Option<std::path::PathBuf>; 2] {
        &self.inner.initial_pane_paths
    }

    pub async fn connect(&self, agent_resolver: Arc<dyn AgentResolver>) -> Result<(), Error> {
        let state = &self.inner.main_window_state;
        let publisher = &self.inner.publisher;
        let session_slot = &self.inner.session;
        let askpass_state = &state.askpass;
        let askpass_response_slot = &self.inner.askpass_response;

        let askpass_provider: Arc<dyn newt_common::askpass::AskpassProvider> =
            Arc::new(TauriAskpassProvider::new(
                askpass_state.clone(),
                askpass_response_slot.clone(),
                publisher.clone(),
            ));

        session::connect(
            &self.inner.connection_target,
            agent_resolver,
            state,
            publisher,
            self.inner.preferences.clone(),
            session_slot,
            |msg| {
                state.connection_status.set_connecting(msg);
                let _ = publisher.publish();
            },
            askpass_provider,
            self.clone(),
        )
        .await
    }

    pub fn askpass_respond(&self, response: Option<String>) {
        if let Some(tx) = self.inner.askpass_response.lock().take() {
            let _ = tx.send(response);
        }
    }

    pub fn with_session<T>(&self, f: impl FnOnce(&Session) -> T) -> Result<T, Error> {
        let guard = self.inner.session.load();
        let opt: &Option<Session> = &guard;
        opt.as_ref()
            .ok_or_else(|| Error::Custom("not connected".into()))
            .map(f)
    }

    pub fn connection_target(&self) -> &ConnectionTarget {
        &self.inner.connection_target
    }

    pub fn window_title(&self) -> &str {
        &self.inner.window_title
    }

    pub fn is_connected(&self) -> bool {
        let guard = self.inner.session.load();
        let opt: &Option<Session> = &guard;
        opt.is_some()
    }

    pub fn set_connection_failed(&self, error: String) {
        self.inner
            .main_window_state
            .connection_status
            .set_failed(error);
        let _ = self.inner.publisher.publish();
    }

    pub fn fs(&self) -> Result<Arc<dyn Filesystem>, Error> {
        self.with_session(|s| s.fs.clone())
    }

    pub fn shell_service(&self) -> Result<Arc<dyn ShellService>, Error> {
        self.with_session(|s| s.shell_service.clone())
    }

    pub fn terminal_client(&self) -> Result<Arc<dyn TerminalClient>, Error> {
        self.with_session(|s| s.terminal_client.clone())
    }

    pub fn file_reader(&self) -> Result<Arc<dyn FileReader>, Error> {
        self.with_session(|s| s.file_reader.clone())
    }

    pub fn vfs_info(&self) -> Result<Arc<dyn VfsInfo>, Error> {
        self.with_session(|s| s.vfs_info.clone())
    }

    /// Reset the mount-log transcript at the start of a connect/mount, so
    /// the Connect dialog shows only the attempt in flight.
    pub fn clear_mount_log(&self) {
        self.inner.main_window_state.mount_log.clear();
        let _ = self.inner.publisher.publish();
    }

    pub fn discovery_provider(
        &self,
    ) -> Result<Arc<dyn newt_common::discovery::DiscoveryProvider>, Error> {
        self.with_session(|s| s.discovery_provider.clone())
    }

    pub fn hot_paths_provider(
        &self,
    ) -> Result<Arc<dyn newt_common::hot_paths::HotPathsProvider>, Error> {
        self.with_session(|s| s.hot_paths_provider.clone())
    }

    pub fn file_server_base_url(&self) -> Result<String, Error> {
        self.with_session(|s| {
            format!(
                "http://localhost:{}/{}",
                s.file_server_port, s.file_server_token
            )
        })
    }

    pub fn window(&self) -> WebviewWindow {
        self.inner.window.clone()
    }

    /// The label of the main window that owns this context.
    pub fn main_window_label(&self) -> &str {
        self.inner.window.label()
    }

    pub fn preferences(&self) -> &crate::preferences::PreferencesHandle {
        &self.inner.preferences
    }

    pub fn with_update<T>(
        &self,
        f: impl FnOnce(&MainWindowState) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let ret = f(&self.inner.main_window_state);
        self.inner.publisher.publish()?;
        ret
    }

    pub async fn with_update_async<T, F, Fut>(&self, f: F) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
        F: FnOnce(MainWindowState) -> Fut,
    {
        let ret = f(self.inner.main_window_state.clone()).await;
        self.inner.publisher.publish()?;
        ret
    }

    pub fn with_pane_update<T>(
        &self,
        pane_handle: PaneHandle,
        f: impl FnOnce(&MainWindowState, &Pane) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.with_update(|s| {
            let pane = s.panes.get(pane_handle).unwrap();
            f(s, &pane)
        })
    }

    pub async fn with_pane_update_async<T, F, Fut>(
        &self,
        pane_handle: PaneHandle,
        f: F,
    ) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
        F: FnOnce(MainWindowState, Arc<Pane>) -> Fut,
    {
        self.with_update_async(|s| {
            let pane = s.panes.get(pane_handle).unwrap();
            async move { f(s, pane).await }
        })
        .await
    }

    pub fn panes(&self) -> &Panes {
        &self.inner.main_window_state.panes
    }

    pub fn active_pane_handle(&self) -> PaneHandle {
        self.inner
            .main_window_state
            .display_options
            .0
            .read()
            .active_pane
    }

    pub fn active_pane(&self) -> Option<Arc<Pane>> {
        self.inner.main_window_state.panes.get(
            self.inner
                .main_window_state
                .display_options
                .0
                .read()
                .active_pane,
        )
    }

    pub fn active_terminal(&self) -> Option<Arc<Terminal>> {
        self.inner
            .main_window_state
            .display_options
            .0
            .read()
            .active_terminal
            .and_then(|handle| self.inner.main_window_state.terminals.get(handle))
    }

    pub fn terminals(&self) -> &Terminals {
        &self.inner.main_window_state.terminals
    }

    pub async fn create_terminal(
        &self,
        path: Option<&newt_common::vfs::path::Path>,
    ) -> Result<Arc<Terminal>, Error> {
        let handle = self
            .terminal_client()?
            .create(newt_common::terminal::TerminalOptions {
                working_dir: path.map(|p| p.to_owned()),
                ..Default::default()
            })
            .await?;
        let terminal = Terminal::from_handle(self, handle);

        let terminal = self.with_update(|s| {
            let terminal = s.terminals.insert(handle, terminal);
            let mut opts = s.display_options.0.write();
            opts.active_terminal = Some(handle);
            opts.panes_focused = false;
            opts.terminal_panel_visible = true;
            Ok(terminal)
        })?;
        terminal.spawn_reader(self.clone(), self.inner.window.clone());
        Ok(terminal)
    }

    pub fn operations_client(&self) -> Result<Arc<dyn OperationsClient>, Error> {
        self.with_session(|s| s.operations_client.clone())
    }

    pub fn next_operation_id(&self) -> Result<OperationId, Error> {
        self.with_session(|s| s.next_operation_id.fetch_add(1, Ordering::Relaxed))
    }

    pub fn operations(&self) -> &Operations {
        &self.inner.main_window_state.operations
    }

    pub fn publish_full(&self) -> Result<(), Error> {
        self.inner.publisher.publish_full()
    }

    pub fn publish(&self) -> Result<(), Error> {
        self.inner.publisher.publish()
    }

    pub fn compute_vfs_targets(&self) -> Result<Vec<VfsTarget>, Error> {
        /// Maps VFS type_name → dialog name for types that need user input to mount.
        fn mount_dialog_for(type_name: &str) -> Option<&'static str> {
            match type_name {
                "s3" => Some("mount_s3"),
                "sftp" => Some("mount_sftp"),
                "k8s" => Some("mount_k8s"),
                _ => None,
            }
        }

        let mut targets = Vec::new();
        let mut mounted_types = HashSet::new();
        self.with_session(|s| {
            let mounted = s.mounted_vfs.read();
            for (vfs_id, info) in mounted.iter() {
                // Ephemeral mounts (archives, searches) are reachable via
                // navigation history; surfacing them as switch targets
                // here would clutter the selector with dead-after-the-
                // fact entries. They auto-unmount when no pane references
                // them, so they don't accumulate either way.
                if info.descriptor.is_ephemeral() {
                    continue;
                }
                mounted_types.insert(info.descriptor.type_name());
                let display_name = s.vfs_info.display_name(*vfs_id).unwrap_or_default();
                if info.descriptor.has_unified_root(&info.mount_meta) {
                    targets.push(VfsTarget {
                        vfs_id: Some(*vfs_id),
                        type_name: info.descriptor.type_name().to_string(),
                        display_name,
                        label: info.descriptor.mount_label(&info.mount_meta),
                        mount_dialog: None,
                        root: None,
                        volume: None,
                        available_bytes: None,
                    });
                } else {
                    // Split-root FS (Windows drives): one entry per root,
                    // labelled with the drive (`C:\`).
                    for root in info.descriptor.roots(&info.mount_meta) {
                        targets.push(VfsTarget {
                            vfs_id: Some(*vfs_id),
                            type_name: info.descriptor.type_name().to_string(),
                            display_name: display_name.clone(),
                            label: Some(info.descriptor.format_path(&root.path, &info.mount_meta)),
                            mount_dialog: None,
                            root: Some(root.path),
                            volume: root.volume,
                            available_bytes: None,
                        });
                    }
                }
            }
        })?;

        for desc in all_descriptors() {
            if mounted_types.contains(desc.type_name()) || desc.is_ephemeral() {
                continue;
            }
            let mount_dialog = mount_dialog_for(desc.type_name()).map(|s| s.to_string());
            if desc.auto_mount_request().is_some() || mount_dialog.is_some() {
                targets.push(VfsTarget {
                    vfs_id: None,
                    type_name: desc.type_name().to_string(),
                    display_name: desc.display_name().to_string(),
                    label: None,
                    mount_dialog,
                    root: None,
                    volume: None,
                    available_bytes: None,
                });
            }
        }

        // Not a VFS descriptor: opens the connect dialog scoped to a pane
        // mount. Offered unconditionally — even inside a remote session the
        // user may want to peek at another host.
        targets.push(VfsTarget {
            vfs_id: None,
            type_name: "remote".to_string(),
            display_name: "Remote".to_string(),
            label: None,
            mount_dialog: Some("mount_remote".to_string()),
            root: None,
            volume: None,
            available_bytes: None,
        });

        targets.sort_by(|a, b| match (a.vfs_id, b.vfs_id) {
            // Same VFS → keep split-root drives in a stable order.
            (Some(id_a), Some(id_b)) => id_a.cmp(&id_b).then_with(|| a.root.cmp(&b.root)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.type_name.cmp(&b.type_name),
        });

        Ok(targets)
    }

    pub async fn mount_vfs(
        &self,
        request: newt_common::vfs::MountRequest,
    ) -> Result<newt_common::vfs::MountResponse, Error> {
        // Archive dedup: if there's already a mount with the same origin,
        // reuse it. With history-anchored auto-unmount, an archive the
        // user navigates back into is almost always still mounted; this
        // makes a re-entry a registry lookup rather than a re-mount, and
        // also coalesces things like clicking the same archive twice. Race
        // window between concurrent mounts of the same origin is left
        // alone — worst case is what we have today (two mounts), and
        // single-user UX rarely produces concurrent calls.
        //
        // Staleness is handled separately by `Vfs::revalidate`, called by
        // the navigation layer when a pane re-enters this VFS.
        if let newt_common::vfs::MountRequest::Archive { origin } = &request
            && let Some(existing) = self.with_session(|s| {
                s.mounted_vfs.read().iter().find_map(|(_, info)| {
                    // Match on type too — searches also carry an origin
                    // (their root directory), and a dedup must never hand
                    // back a non-archive mount.
                    if info.descriptor.type_name() == "archive"
                        && info.origin.as_ref() == Some(origin)
                    {
                        Some(newt_common::vfs::MountResponse {
                            vfs_id: info.vfs_id,
                            type_name: info.descriptor.type_name().into(),
                            mount_meta: info.mount_meta.clone(),
                            origin: info.origin.clone(),
                        })
                    } else {
                        None
                    }
                })
            })?
        {
            return Ok(existing);
        }

        let vfs_manager = self.with_session(|s| s.vfs_manager.clone())?;
        let response = vfs_manager.mount(request).await?;
        let descriptor = lookup_descriptor(&response.type_name)
            .ok_or_else(|| Error::Custom(format!("unknown VFS type: {}", response.type_name)))?;
        self.with_session(|s| {
            s.mounted_vfs.write().insert(
                response.vfs_id,
                MountedVfsInfo {
                    vfs_id: response.vfs_id,
                    descriptor,
                    mount_meta: response.mount_meta.clone(),
                    origin: response.origin.clone(),
                },
            );
        })?;
        self.refresh_mount_summary();
        Ok(response)
    }

    pub async fn unmount_vfs(&self, vfs_id: VfsId) -> Result<(), Error> {
        let vfs_manager = self.with_session(|s| s.vfs_manager.clone())?;
        vfs_manager.unmount(vfs_id).await?;
        self.with_session(|s| {
            s.mounted_vfs.write().remove(&vfs_id);
        })?;
        // Defensive: clear any progress entry the VFS may have left
        // behind. A well-behaved VFS sends a final `None`, but we
        // don't trust that across the RPC boundary.
        self.inner.main_window_state.vfs_progress.clear_for(vfs_id);
        self.refresh_mount_summary();
        Ok(())
    }

    /// Logical remount of `vfs_id` (see `VfsManager::remount`): refresh
    /// its `mount_meta` and, if it changed, update our cached copy and
    /// the pushed mount summary. Returns whether anything changed —
    /// callers decide what downstream state (an open VFS selector) to
    /// rebuild.
    pub async fn remount_vfs(
        &self,
        vfs_id: VfsId,
        mount_meta: Option<Vec<u8>>,
    ) -> Result<bool, Error> {
        let vfs_manager = self.with_session(|s| s.vfs_manager.clone())?;
        let new_meta = vfs_manager.remount(vfs_id, mount_meta).await?;
        let changed = self.with_session(|s| {
            let mut mounted = s.mounted_vfs.write();
            match mounted.get_mut(&vfs_id) {
                Some(info) if info.mount_meta != new_meta => {
                    info.mount_meta = new_meta;
                    true
                }
                _ => false,
            }
        })?;
        if changed {
            self.refresh_mount_summary();
        }
        Ok(changed)
    }

    /// Recompute the pushed [`MountSummary`] from the session's mounted
    /// set. The value rides out with the next state publish (every mount/
    /// unmount is followed by one — a navigation or a modal update).
    pub fn refresh_mount_summary(&self) {
        let has_split_root_vfs = self
            .with_session(|s| {
                s.mounted_vfs
                    .read()
                    .values()
                    .any(|i| !i.descriptor.has_unified_root(&i.mount_meta))
            })
            .unwrap_or(false);
        self.inner
            .main_window_state
            .mount_summary
            .0
            .write()
            .has_split_root_vfs = has_split_root_vfs;
    }

    /// Fully-qualified path to land on when selecting/mounting `vfs_id`
    /// (asks the descriptor — see `VfsDescriptor::initial_path`). Falls
    /// back to the VFS root if the VFS isn't mounted.
    pub fn vfs_initial_path(&self, vfs_id: VfsId) -> VfsPath {
        self.with_session(|s| {
            s.mounted_vfs
                .read()
                .get(&vfs_id)
                .map(|info| VfsPath::new(vfs_id, info.descriptor.initial_path(&info.mount_meta)))
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| VfsPath::root(vfs_id))
    }

    pub fn resolve_display_path(&self, input: &str) -> Option<VfsPath> {
        self.with_session(|s| {
            let mut matches: Vec<_> = s
                .mounted_vfs
                .read()
                .iter()
                .filter_map(|(vfs_id, info)| {
                    let m = info
                        .descriptor
                        .try_parse_display_path(input, &info.mount_meta)?;
                    Some((m.priority, VfsPath::new(*vfs_id, m.path)))
                })
                .collect();
            matches.sort_by_key(|(priority, _)| *priority);
            matches.into_iter().next().map(|(_, path)| path)
        })
        .ok()
        .flatten()
    }

    pub fn format_vfs_path(&self, vfs_path: &VfsPath) -> String {
        self.with_session(|s| {
            s.mounted_vfs.read().get(&vfs_path.vfs_id).map(|info| {
                info.descriptor
                    .format_path(&vfs_path.path, &info.mount_meta)
            })
        })
        .ok()
        .flatten()
        .unwrap_or_else(|| vfs_path.to_string())
    }

    pub async fn refresh(&self, force: bool) -> Result<(), Error> {
        self.with_update_async(|gs| async move {
            gs.refresh(force).await?;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(super) async fn cleanup_stale_ephemeral_mounts(&self) -> Result<(), Error> {
        // Collect VFS IDs currently in use by any pane — both the pane's
        // current path and any path reachable via back/forward history.
        // Anchoring on history (rather than just the current path) means
        // navigating back into an archive (or a search) doesn't fail
        // with "unmounted" when the user had stepped outside it; the
        // mount stays alive as long as it's reachable via the history
        // of either pane.
        let pane_vfs_ids: std::collections::HashSet<VfsId> = self
            .panes()
            .all()
            .iter()
            .flat_map(|p| {
                let mut ids = p.history_vfs_ids();
                ids.push(p.path().vfs_id);
                ids
            })
            .collect();

        // An ephemeral VFS is "stale" iff no pane references it directly
        // and no other in-use VFS has it as its (transitive) origin —
        // the second condition keeps a parent archive alive while a
        // nested child archive is still open, and a search's source
        // mount (e.g. an archive it ran over) alive while the search
        // is reachable.
        let stale_ids: Vec<VfsId> = self.with_session(|s| {
            let mounted = s.mounted_vfs.read();

            let mut in_use = pane_vfs_ids.clone();
            let mut queue: Vec<VfsId> = pane_vfs_ids.into_iter().collect();
            while let Some(vfs_id) = queue.pop() {
                if let Some(info) = mounted.get(&vfs_id)
                    && let Some(ref origin) = info.origin
                    && in_use.insert(origin.vfs_id)
                {
                    queue.push(origin.vfs_id);
                }
            }

            mounted
                .iter()
                .filter(|(id, info)| info.descriptor.is_ephemeral() && !in_use.contains(id))
                .map(|(id, _)| *id)
                .collect()
        })?;

        for vfs_id in stale_ids {
            log::info!("unmounting stale ephemeral VFS {:?}", vfs_id);
            self.unmount_vfs(vfs_id).await?;
        }

        Ok(())
    }

    pub fn clipboard(&self) -> RwLockWriteGuard<'_, arboard::Clipboard> {
        self.inner.clipboard.write()
    }

    /// Tear down the current session and wipe connection-scoped UI state so this
    /// window can be reconnected from scratch. Safe to call if there's no
    /// session (e.g. the previous one already disconnected).
    pub async fn disconnect_for_reconnect(&self) {
        // Best-effort: kill open PTYs while we still have a live terminal
        // client. If the session is already gone this is a no-op.
        if let Ok(tc) = self.terminal_client() {
            for handle in self.inner.main_window_state.terminals.handles_sorted() {
                let _ = tc.kill(handle).await;
            }
        }

        let state = &self.inner.main_window_state;
        state.panes.clear();
        state.terminals.clear();
        state.operations.clear();
        {
            let mut opts = state.display_options.0.write();
            opts.active_terminal = None;
            opts.terminal_panel_visible = false;
            opts.panes_focused = true;
            opts.active_pane = PaneHandle(0);
        }
        *state.connection_status.0.write() = ConnectionStatus::Connecting {
            message: "Reconnecting...".into(),
            log: Vec::new(),
        };

        // Drop the session — aborts file server / event loop handles and, for
        // remote/elevated, causes the agent subprocess to exit.
        self.inner.session.store(Arc::new(None));

        let _ = self.publish();
    }
}

/// Create a new main window in the current process.
///
/// Creates the `WebviewWindow`, constructs a `MainWindowContext`, registers it
/// in `GlobalContext.main_windows`, attaches the per-window macOS Edit menu,
/// and subscribes to theme preference changes.
///
/// Does **not** connect the session. For the initial window at startup, the
/// caller (`main.rs::setup`) may synchronously `block_on(ctx.connect())` before
/// the webview loads. For windows created from IPC commands, the frontend's
/// `init` command drives the async connect.
pub fn spawn_main_window(
    app_handle: &tauri::AppHandle,
    connection_target: ConnectionTarget,
    window_title: String,
    initial_pane_paths: [Option<std::path::PathBuf>; 2],
) -> Result<(WebviewWindow, MainWindowContext), Error> {
    let global_ctx: State<GlobalContext> = app_handle.state();

    // First window uses the stable "main" label; subsequent windows get UUIDs.
    let label = {
        let locked = global_ctx.main_windows.lock();
        if locked.contains_key("main") {
            uuid::Uuid::new_v4().to_string()
        } else {
            "main".to_string()
        }
    };

    let prefs_handle = global_ctx.preferences().handle();
    let theme = prefs_handle
        .load()
        .appearance
        .theme
        .to_tauri_theme()
        .or_else(crate::detect_theme);

    let window =
        tauri::WebviewWindowBuilder::new(app_handle, &label, tauri::WebviewUrl::App("/".into()))
            .title(&window_title)
            .resizable(true)
            .inner_size(1100.0, 800.0)
            .theme(theme)
            .build()?;
    crate::disable_webview_autofill(&window);

    let ctx = MainWindowContext::new(
        window.clone(),
        connection_target,
        window_title,
        prefs_handle.clone(),
        initial_pane_paths,
    );
    global_ctx
        .main_windows
        .lock()
        .insert(label.clone(), ctx.clone());

    #[cfg(target_os = "macos")]
    menu::setup(app_handle, &label)?;

    // Live title-bar theme updates when the user changes preferences.
    crate::spawn_theme_sync(&window, prefs_handle.clone());

    Ok((window, ctx))
}
