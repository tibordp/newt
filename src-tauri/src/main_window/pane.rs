use log::debug;
use log::info;
use log::warn;
use newt_common::filesystem::resolve_vfs;
use newt_common::filesystem::File;
use newt_common::filesystem::FileList;
use newt_common::filesystem::Filesystem;
use newt_common::filesystem::FsStats;
use newt_common::filesystem::ListFilesOptions;
use newt_common::vfs::{Breadcrumb, VfsDescriptor, VfsId, VfsPath};
use parking_lot::Mutex;
use parking_lot::RwLock;
use parking_lot::RwLockReadGuard;
use parking_lot::RwLockWriteGuard;
use tokio_util::sync::CancellationToken;

use crate::common::Error;
use crate::common::UpdatePublisher;
use std::collections::HashMap;
use std::collections::HashSet;

use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::DisplayOptions;
use super::DisplayOptionsInner;
use super::MainWindowState;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortingKey {
    #[default]
    Name,
    Extension,
    Size,
    User,
    Mode,
    Group,
    Modified,
    Accessed,
    Created,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Sorting {
    pub key: SortingKey,
    pub asc: bool,
}

impl Default for Sorting {
    fn default() -> Self {
        Self {
            key: SortingKey::default(),
            asc: true,
        }
    }
}

#[derive(Default, Clone, serde::Serialize)]
pub struct PaneStats {
    pub file_count: usize,
    pub dir_count: usize,
    pub bytes: u64,
    pub selected_file_count: usize,
    pub selected_dir_count: usize,
    pub selected_bytes: u64,
}

pub type DescriptorLookup = Arc<dyn Fn(VfsId) -> Option<&'static dyn VfsDescriptor> + Send + Sync>;

pub struct Pane {
    fs: Arc<dyn Filesystem>,
    nav_changes_rx: tokio::sync::watch::Receiver<()>,
    navigation_mutex: tokio::sync::Mutex<tokio::sync::watch::Sender<()>>,
    refresh_queue: AtomicUsize,
    file_list: RwLock<Arc<FileList>>,
    view_state: RwLock<PaneViewState>,
    display_options: DisplayOptions,
    publisher: Arc<UpdatePublisher<MainWindowState>>,
    cancellation_token: Mutex<Option<CancellationToken>>,
    descriptor_lookup: DescriptorLookup,
}

impl Pane {
    pub fn new(
        fs: Arc<dyn Filesystem>,
        path: VfsPath,
        display_options: DisplayOptions,
        publisher: Arc<UpdatePublisher<MainWindowState>>,
        descriptor_lookup: DescriptorLookup,
    ) -> Self {
        let (tx, rx) = tokio::sync::watch::channel(());

        Self {
            fs,
            navigation_mutex: tokio::sync::Mutex::new(tx),
            file_list: RwLock::new(Arc::new(FileList::new(path, vec![], None))),
            refresh_queue: AtomicUsize::new(0),
            view_state: RwLock::new(PaneViewState::default()),
            nav_changes_rx: rx,
            display_options,
            publisher,
            cancellation_token: Mutex::new(None),
            descriptor_lookup,
        }
    }

    pub async fn watch_changes(self: Arc<Self>) {
        let mut rx = self.nav_changes_rx.clone();
        loop {
            let vfs_path = self.path();
            tokio::select! {
                ret = self.fs.poll_changes(vfs_path.clone()) => {
                    match ret {
                        Ok(()) => {
                            info!("changes detected")
                        }
                        Err(e) => {
                            warn!("failed to watch, the folder was probably removed: {}", e);
                            // We wait until the next navigation before restarting the watch
                            let _ = rx.changed().await;
                            continue;
                        }
                    }
                }
                _ = rx.changed() =>  {
                    continue;
                }
            };

            let cloned = self.clone();
            tauri::async_runtime::spawn(async move {
                match cloned.refresh(Some(vfs_path)).await {
                    Ok(()) => cloned.publisher.publish().unwrap(),
                    Err(e) => warn!("failed to refresh pane: {}", e),
                }
            });
        }
    }

    pub fn path(&self) -> VfsPath {
        self.file_list.read().path().clone()
    }

    async fn navigate_impl(
        &self,
        target: VfsPath,
        silent: bool,
        changes_sender: &mut tokio::sync::watch::Sender<()>,
    ) -> Result<(), Error> {
        let old_file_list = {
            // Temporarily push the new navigation state. This is mostly so people can backspace out of
            // a directory that is taking a long time to load (and not just Esc) - but eventually this
            // is also to support gradual loading of directories. Right now - after this block, the
            // view state is out of sync with the navigation state (still shows the old files)
            let mut file_list = self.file_list.write();
            let old_file_list = std::mem::replace(
                &mut *file_list,
                Arc::new(FileList::new(target.clone(), Vec::new(), None)),
            );

            if !silent {
                let mut ws = self.view_state_mut();
                ws.pending_path = Some(target.clone());
                self.update_display(&mut ws);
            }

            old_file_list
        };

        if !silent {
            let _ = self.publisher.publish();
        }

        // Set up cancellation
        let token = CancellationToken::new();
        if let Some(previous) = self.cancellation_token.lock().replace(token.clone()) {
            previous.cancel();
        }

        // Create batch channel and start streaming.
        // Skip batches when refreshing the same path — keep the current view
        // intact and swap in the final result atomically.
        let same_path = *old_file_list.path() == target;
        let (batch_tx, mut batch_rx) = mpsc::unbounded_channel::<FileList>();
        let streaming_fut = self.fs.list_files(
            target.clone(),
            ListFilesOptions { strict: !silent },
            (!silent && !same_path).then_some(batch_tx),
        );
        tokio::pin!(streaming_fut);

        let mut accumulated = Vec::new();
        let mut batch_path: Option<VfsPath> = None;
        let mut batch_fs_stats: Option<FsStats> = None;
        let mut first_batch = true;
        let mut last_publish = Instant::now();
        let mut dirty = false;
        let throttle = Duration::from_millis(100);

        let result = loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    break Err(Error::Cancelled);
                }
                result = &mut streaming_fut => {
                    break result.map_err(Error::from);
                }
                Some(file_list) = batch_rx.recv() => {
                    let incoming_path = file_list.path().clone();
                    if batch_path.as_ref() != Some(&incoming_path) {
                        accumulated.clear();
                        batch_path = Some(incoming_path);
                    }
                    batch_fs_stats = file_list.fs_stats().cloned();
                    accumulated.extend(file_list.files().iter().cloned());
                    if !silent {
                        // On first batch: clear pending_path, set loading=true,
                        // and reset filter/selection/focus for the new directory
                        if first_batch {
                            let mut ws = self.view_state_mut();
                            ws.pending_path = batch_path.clone();
                            ws.loading = true;
                            ws.set_filter(None);
                            ws.selected.clear();
                            ws.focused = None;
                            self.update_display(&mut ws);
                            first_batch = false;
                        }
                        // Throttled intermediate publish
                        if first_batch || last_publish.elapsed() >= throttle {
                            let display_options = self.display_options.0.read().clone();
                            let interim = FileList::new(
                                batch_path.clone().unwrap_or_else(|| target.clone()),
                                accumulated.clone(),
                                batch_fs_stats.clone(),
                            );
                            self.view_state_mut().update(display_options, &interim);
                            let _ = self.publisher.publish();
                            last_publish = Instant::now();
                            dirty = true;
                        }
                    }
                }
            }
        };

        let new_file_list = match result {
            Ok(ret) => Arc::new(ret),
            Err(e) => {
                debug!("navigation failed: {}", e);

                if !dirty {
                    // Restore the old navigation state, but only if we haven't already published a partial update
                    // The user may have already navigated somewhere else, so we shouldn't override that
                    *self.file_list.write() = old_file_list;
                }

                let mut ws = self.view_state_mut();
                ws.pending_path = None;
                ws.loading = false;
                ws.partial = dirty;
                self.update_display(&mut ws);

                return match e {
                    Error::Cancelled => Ok(()),
                    e => Err(e),
                };
            }
        };

        let mut ws = self.view_state_mut();

        let mut file_list = self.file_list.write();
        *file_list = new_file_list.clone();

        let has_path_changed = old_file_list.path() != new_file_list.path();
        let display_options = self.display_options.0.read().clone();

        ws.pending_path = None;
        ws.loading = false;
        ws.partial = false;
        if has_path_changed {
            let _ = changes_sender.send(());
            // Only clear if we didn't already do it on first batch
            if first_batch {
                ws.set_filter(None);
                ws.selected.clear();
                ws.focused = None;
            }
        }

        ws.update(display_options, &new_file_list);
        self.update_display(&mut ws);

        if has_path_changed {
            if target == *new_file_list.path() {
                ws.focus_descendant(&old_file_list.path().path);
            } else {
                ws.focus_descendant(&target.path);
            }
        }

        Ok(())
    }

    pub fn cancel(&self) {
        if let Some(token) = self.cancellation_token.lock().take() {
            token.cancel();
        }
    }

    pub fn view_state(&self) -> RwLockReadGuard<'_, PaneViewState> {
        self.view_state.read()
    }

    pub fn view_state_mut(&self) -> RwLockWriteGuard<'_, PaneViewState> {
        self.view_state.write()
    }

    pub fn update_view_state(&self) {
        let display_options = self.display_options.0.read().clone();
        let file_list = self.file_list.read();
        let mut view_state: parking_lot::lock_api::RwLockWriteGuard<
            parking_lot::RawRwLock,
            PaneViewState,
        > = self.view_state.write();

        view_state.update(display_options, &file_list);
        self.update_display(&mut view_state);
    }

    fn update_display(&self, ws: &mut PaneViewState) {
        if let Some(desc) = (self.descriptor_lookup)(ws.path.vfs_id) {
            ws.display_path = desc.format_path(&ws.path.path);
            ws.vfs_display_name = desc.display_name().to_string();
        } else {
            ws.display_path = ws.path.to_string();
            ws.vfs_display_name = String::new();
        }
        let shown_path = ws.pending_path.as_ref().unwrap_or(&ws.path);
        if let Some(shown_desc) = (self.descriptor_lookup)(shown_path.vfs_id) {
            ws.breadcrumbs = shown_desc.breadcrumbs(&shown_path.path);
        }
    }

    pub async fn refresh(&self, expected_path: Option<VfsPath>) -> Result<(), Error> {
        let Some(expected_path) = expected_path else {
            return self.navigate(".").await;
        };

        self.refresh_queue.fetch_add(1, Ordering::SeqCst);

        let mut changes_sender = self.navigation_mutex.lock().await;
        if self.refresh_queue.fetch_sub(1, Ordering::SeqCst) == 1 && self.path() == expected_path {
            self.navigate_impl(expected_path, true, &mut changes_sender)
                .await?;
        }

        Ok(())
    }

    pub async fn navigate<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let current = self.path();
        let target = resolve_vfs(&current.join(path));

        // Cancel any pending navigation
        self.cancel();

        let mut changes_sender = self.navigation_mutex.lock().await;
        self.navigate_impl(target, false, &mut changes_sender).await
    }

    pub async fn navigate_to(&self, target: VfsPath) -> Result<(), Error> {
        // Cancel any pending navigation
        self.cancel();

        let mut changes_sender = self.navigation_mutex.lock().await;
        self.navigate_impl(target, false, &mut changes_sender).await
    }

    /// If the selection is empty, returns the focused file.
    /// Otherwise, returns the selected files. The focused file is NOT included
    /// in the selection in this case.
    pub fn get_effective_selection(&self) -> Vec<VfsPath> {
        let view_state = self.view_state.read();

        if view_state.selected.is_empty() {
            view_state
                .focused
                .iter()
                .map(|s| view_state.path.join(s))
                .collect()
        } else {
            view_state
                .selected
                .iter()
                .map(|s: &String| view_state.path.join(s))
                .collect()
        }
    }

    pub fn get_focused_file(&self) -> Option<VfsPath> {
        let view_state = self.view_state.read();

        view_state.focused.as_ref().map(|s| view_state.path.join(s))
    }

    /// Returns true if the focused item is known to be a directory.
    /// Returns false for non-directories, unknown items, or if nothing is focused.
    pub fn is_focused_dir(&self) -> bool {
        let view_state = self.view_state.read();
        let focused = match view_state.focused.as_ref() {
            Some(f) => f,
            None => return false,
        };
        view_state
            .files
            .iter()
            .find(|f| f.name == *focused)
            .is_some_and(|f| f.is_dir)
    }
}

impl serde::Serialize for Pane {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        self.view_state.read().serialize(serializer)
    }
}

fn compare_extension(a: &File, b: &File) -> std::cmp::Ordering {
    if a.is_dir && b.is_dir {
        return std::cmp::Ordering::Equal;
    }

    let a = a.name.rfind('.').map(|i| &a.name[i + 1..]).unwrap_or("");
    let b = b.name.rfind('.').map(|i| &b.name[i + 1..]).unwrap_or("");

    a.cmp(b)
}

/// View model for a pane.
#[derive(Default, Clone, serde::Serialize)]
pub struct PaneViewState {
    pub path: VfsPath,
    pub pending_path: Option<VfsPath>,
    pub loading: bool,
    pub partial: bool,
    pub sorting: Sorting,
    pub files: Vec<File>,
    pub focused: Option<String>,
    pub selected: HashSet<String>,
    pub filter: Option<String>,
    pub fs_stats: Option<FsStats>,
    pub stats: PaneStats,
    pub focused_index: Option<usize>,
    pub display_path: String,
    pub vfs_display_name: String,
    pub breadcrumbs: Vec<Breadcrumb>,

    #[serde(skip)]
    file_lookup: HashMap<String, usize>,
    #[serde(skip)]
    filter_regex: Option<regex::Regex>,
}

impl PaneViewState {
    fn recompute_stats(&mut self) {
        let mut stats = PaneStats::default();
        for f in &self.files {
            if f.is_dir {
                stats.dir_count += 1;
            } else {
                stats.file_count += 1;
                stats.bytes += f.size.unwrap_or(0);
            }
            if self.selected.contains(&f.name) {
                if f.is_dir {
                    stats.selected_dir_count += 1;
                } else {
                    stats.selected_file_count += 1;
                    stats.selected_bytes += f.size.unwrap_or(0);
                }
            }
        }
        self.stats = stats;
        self.focused_index = self
            .focused
            .as_ref()
            .and_then(|name| self.file_lookup.get(name).copied());
    }

    fn sort(&mut self) {
        self.files.sort_by(|a, b| {
            // Directories first
            if a.name == ".." {
                return std::cmp::Ordering::Less;
            } else if b.name == ".." {
                return std::cmp::Ordering::Greater;
            }

            if a.is_dir && !b.is_dir {
                return std::cmp::Ordering::Less;
            } else if !a.is_dir && b.is_dir {
                return std::cmp::Ordering::Greater;
            }

            let (a, b) = if self.sorting.asc { (a, b) } else { (b, a) };
            match self.sorting.key {
                SortingKey::Name => a.name.cmp(&b.name),
                SortingKey::Extension => compare_extension(a, b),
                SortingKey::Size => a.size.cmp(&b.size),
                SortingKey::User => a
                    .user
                    .partial_cmp(&b.user)
                    .unwrap_or(std::cmp::Ordering::Less),
                SortingKey::Group => a
                    .group
                    .partial_cmp(&b.group)
                    .unwrap_or(std::cmp::Ordering::Less),
                SortingKey::Mode => a.mode.cmp(&b.mode),
                SortingKey::Modified => a.modified.unwrap_or(0).cmp(&b.modified.unwrap_or(0)),
                SortingKey::Accessed => a.accessed.unwrap_or(0).cmp(&b.accessed.unwrap_or(0)),
                SortingKey::Created => a.created.unwrap_or(0).cmp(&b.created.unwrap_or(0)),
            }
        });

        self.file_lookup = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.name.clone(), index))
            .collect();
    }

    fn update_focus(&mut self) {
        self.selected
            .retain(|name| self.file_lookup.contains_key(name));

        if self.focused.is_none()
            || !self
                .file_lookup
                .contains_key(self.focused.as_ref().unwrap())
        {
            self.focused = self.files.first().map(|f| f.name.clone());
        }
    }

    fn update_filter(&mut self, filter: Option<String>) {
        self.filter = filter;
        self.filter_regex = self
            .filter
            .as_ref()
            .map(|f| regex::Regex::new(&format!("(?i)^{}", regex::escape(f))).unwrap());
    }

    // Public API
    pub fn update(&mut self, display_options: DisplayOptionsInner, file_list: &FileList) {
        // the path is expected to be canonical by now

        self.path = file_list.path().clone();
        self.fs_stats = file_list.fs_stats().cloned();
        self.files = file_list
            .files()
            .iter()
            .filter(|f| !f.is_hidden || display_options.show_hidden || f.name == "..")
            .cloned()
            .collect();

        self.sort();
        self.update_focus();
        self.recompute_stats();
    }

    pub fn focus(&mut self, filename: String) {
        self.update_filter(None);
        self.focused = Some(filename);
        self.update_focus();
        self.recompute_stats();
    }

    pub fn focus_descendant(&mut self, path: &Path) {
        if let Some(filename) = path
            .strip_prefix(&self.path.path)
            .ok()
            .and_then(|prefix| prefix.iter().next())
        {
            self.focus(filename.to_string_lossy().to_string());
        }
    }

    pub fn set_sorting(&mut self, sorting: Sorting) {
        self.sorting = sorting;
        self.sort();
        self.recompute_stats();
    }

    pub fn toggle_selected(&mut self, filename: Option<String>, focus_next: bool) {
        let Some(filename) = filename.as_ref().or(self.focused.as_ref()).cloned() else {
            return;
        };

        if !self.selected.remove(&filename) && self.file_lookup.contains_key(&filename) {
            self.selected.insert(filename.clone());
        }
        self.selected.remove("..");

        self.update_filter(None);
        if focus_next {
            self.relative_jump(1, false);
        } else {
            self.focused = Some(filename);
            self.update_focus();
        }
        self.recompute_stats();
    }

    pub fn select_range(&mut self, filename: String) {
        self.update_filter(None);
        let Some(&start_index) = self
            .focused
            .as_deref()
            .map(|f| self.file_lookup.get(f).unwrap())
        else {
            return;
        };

        let Some(&end_index) = self.file_lookup.get(&filename) else {
            return;
        };

        for i in start_index.min(end_index)..=start_index.max(end_index) {
            self.selected.insert(self.files[i].name.clone());
        }
        self.selected.remove("..");

        self.focused = Some(filename);
        self.recompute_stats();
    }

    pub fn select_all(&mut self) {
        self.update_filter(None);
        self.filter_regex = None;
        self.selected = self.file_lookup.keys().cloned().collect();
        self.selected.remove("..");
        self.recompute_stats();
    }

    pub fn deselect_all(&mut self) {
        self.update_filter(None);
        self.filter_regex = None;
        self.selected.clear();
        self.recompute_stats();
    }

    pub fn set_selection(&mut self, selected: HashSet<String>, focused: Option<String>) {
        self.update_filter(None);
        self.selected = selected;
        self.selected.remove("..");
        if let Some(ref f) = focused {
            if self.file_lookup.contains_key(f) {
                self.focused = Some(f.clone());
            }
        }
        self.recompute_stats();
    }

    pub fn set_filter(&mut self, filter: Option<String>) {
        let Some(filter) = filter else {
            self.update_filter(None);
            self.recompute_stats();
            return;
        };

        let start_index = *self
            .focused
            .as_deref()
            .map(|f| self.file_lookup.get(f).unwrap())
            .unwrap_or(&0);

        let new_filter = regex::Regex::new(&format!("(?i)^{}", regex::escape(&filter))).unwrap();

        // Search in the down direction first
        for f in self.files.iter().skip(start_index) {
            if new_filter.is_match(&f.name) {
                self.focused = Some(f.name.clone());
                self.filter = Some(filter);
                self.filter_regex = Some(new_filter);
                self.recompute_stats();
                return;
            }
        }

        // Then search in the up direction
        for f in self.files.iter().take(start_index).rev() {
            if new_filter.is_match(&f.name) {
                self.focused = Some(f.name.clone());
                self.filter = Some(filter);
                self.filter_regex = Some(new_filter);
                self.recompute_stats();
                return;
            }
        }

        if self.filter.is_none() {
            self.update_filter(Some(Default::default()));
        }
        self.recompute_stats();
    }

    pub fn relative_jump(&mut self, mut offset: i32, with_selection: bool) {
        let direction = offset.signum();

        if self.files.is_empty() {
            return;
        }

        let mut new_index = self
            .focused
            .as_ref()
            .map(|focused| *self.file_lookup.get(focused).unwrap() as i32)
            .unwrap_or(0);

        let mut i = new_index;
        if with_selection {
            self.selected
                .insert(self.files[new_index as usize].name.clone());
        }
        self.selected.remove("..");

        loop {
            i += direction;
            if i < 0 || i >= (self.files.len() as i32) || offset == 0 {
                break;
            }
            if self
                .filter_regex
                .as_ref()
                .map_or(true, |re| re.is_match(&self.files[i as usize].name))
            {
                new_index = i;
                offset -= direction;
            }
        }

        self.focused = Some(self.files[new_index as usize].name.clone());
        self.recompute_stats();
    }
}
