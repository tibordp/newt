use log::debug;
use log::info;
use log::warn;
use newt_common::filesystem::FsStats;
use newt_common::filesystem::resolve;
use newt_common::filesystem::File;
use newt_common::filesystem::FileList;
use newt_common::filesystem::Filesystem;
use parking_lot::Mutex;
use parking_lot::RwLock;
use parking_lot::RwLockReadGuard;
use parking_lot::RwLockWriteGuard;
use tokio_util::sync::CancellationToken;

use crate::common::Error;
use crate::common::UpdatePublisher;
use std::collections::HashMap;
use std::collections::HashSet;

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

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
}

impl Pane {
    pub fn new(
        fs: Arc<dyn Filesystem>,
        path: PathBuf,
        display_options: DisplayOptions,
        publisher: Arc<UpdatePublisher<MainWindowState>>,
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
        }
    }

    pub async fn watch_changes(self: Arc<Self>) {
        let mut rx = self.nav_changes_rx.clone();
        loop {
            let path = self.path();
            tokio::select! {
                ret = self.fs.poll_changes(path.clone()) => {
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
                match cloned.refresh(Some(path)).await {
                    Ok(()) => cloned.publisher.publish().unwrap(),
                    Err(e) => warn!("failed to refresh pane: {}", e),
                }
            });
        }
    }

    pub fn path(&self) -> PathBuf {
        self.file_list.read().path().to_path_buf()
    }

    async fn cancellable<T, Fut>(&self, f: Fut) -> Result<T, Error>
    where
        Fut: Future<Output = Result<T, Error>>,
    {
        let token = CancellationToken::new();
        if let Some(previous) = self.cancellation_token.lock().replace(token.clone()) {
            previous.cancel();
        }

        tokio::select! {
            _ = token.cancelled() => {
                Err(Error::Cancelled)
            }
            ret = f => {
                ret
            }
        }
    }

    async fn navigate_impl(
        &self,
        target: PathBuf,
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
                self.view_state_mut().pending_path = Some(target.clone());
            }

            old_file_list
        };

        if !silent {
            let _ = self.publisher.publish();
        }

        let fut = {
            let target = target.clone();
            async move { Ok(self.fs.list_files(target).await?) }
        };

        let new_file_list = match self.cancellable(fut).await {
            Ok(ret) => Arc::new(ret),
            Err(e) => {
                debug!("navigation failed: {}", e);

                // Restore the old navigation state
                *self.file_list.write() = old_file_list;
                self.view_state_mut().pending_path = None;

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
        if has_path_changed {
            let _ = changes_sender.send(());
            ws.set_filter(None);
            ws.selected.clear();
            ws.focused = None;
        }

        ws.update(display_options, &new_file_list);

        if has_path_changed {
            if target == new_file_list.path() {
                ws.focus_descendant(old_file_list.path());
            } else {
                ws.focus_descendant(&target);
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
    }

    pub async fn refresh(&self, expected_path: Option<PathBuf>) -> Result<(), Error> {
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
        let target = resolve(&self.path().join(path));

        // Cancel any pending navigation
        self.cancel();

        let mut changes_sender = self.navigation_mutex.lock().await;
        self.navigate_impl(target, false, &mut changes_sender).await
    }

    /// If the selection is empty, returns the focused file.
    /// Otherwise, returns the selected files. The focused file is NOT included
    /// in the selection in this case.
    pub fn get_effective_selection(&self) -> Vec<PathBuf> {
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
    pub path: PathBuf,
    pub pending_path: Option<PathBuf>,
    pub sorting: Sorting,
    pub files: Vec<File>,
    pub focused: Option<String>,
    pub selected: HashSet<String>,
    pub filter: Option<String>,
    pub fs_stats: Option<FsStats>,

    #[serde(skip)]
    file_lookup: HashMap<String, usize>,
    #[serde(skip)]
    filter_regex: Option<regex::Regex>,
}

impl PaneViewState {
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
            self.focused = self.files.get(0).map(|f| f.name.clone());
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

        self.path = file_list.path().to_path_buf();
        self.fs_stats = file_list.fs_stats().cloned();
        self.files = file_list
            .files()
            .iter()
            .filter(|f| !f.is_hidden || display_options.show_hidden || f.name == "..")
            .cloned()
            .collect();

        self.sort();
        self.update_focus();
    }

    pub fn focus(&mut self, filename: String) {
        self.update_filter(None);
        self.focused = Some(filename);
        self.update_focus();
    }

    pub fn focus_descendant(&mut self, path: &Path) {
        if let Some(filename) = path
            .strip_prefix(&self.path)
            .ok()
            .and_then(|prefix| prefix.iter().next())
        {
            self.focus(filename.to_string_lossy().to_string());
        }
    }

    pub fn set_sorting(&mut self, sorting: Sorting) {
        self.sorting = sorting;
        self.sort();
    }

    pub fn toggle_selected(&mut self, filename: String, focus_next: bool) {
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

        let Some(&end_index) = self
            .file_lookup
            .get(&filename) else {
                return;
            };

        for i in start_index.min(end_index)..=start_index.max(end_index) {
            self.selected.insert(self.files[i].name.clone());
        }
        self.selected.remove("..");

        self.focused = Some(filename);
    }

    pub fn select_all(&mut self) {
        self.update_filter(None);
        self.filter_regex = None;
        self.selected = self.file_lookup.keys().cloned().collect();
        self.selected.remove("..");
    }

    pub fn deselect_all(&mut self) {
        self.update_filter(None);
        self.filter_regex = None;
        self.selected.clear();
    }

    pub fn set_filter(&mut self, filter: Option<String>) {
        let Some(filter) = filter else {
            self.update_filter(None);
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
                return;
            }
        }

        // Then search in the up direction
        for f in self.files.iter().take(start_index).rev() {
            if new_filter.is_match(&f.name) {
                self.focused = Some(f.name.clone());
                self.filter = Some(filter);
                self.filter_regex = Some(new_filter);
                return;
            }
        }

        if self.filter.is_none() {
            self.update_filter(Some(Default::default()));
        }
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
            if self.files[i as usize]
                .name
                .starts_with(self.filter.as_deref().unwrap_or(""))
            {
                new_index = i;
                offset -= direction;
            }
        }

        self.focused = Some(self.files[new_index as usize].name.clone());
    }
}
