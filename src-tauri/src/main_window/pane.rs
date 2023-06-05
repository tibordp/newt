use parking_lot::Mutex;
use parking_lot::RwLock;
use parking_lot::RwLockReadGuard;
use parking_lot::RwLockWriteGuard;
use tokio_util::sync::CancellationToken;

use crate::common::Error;
use crate::common::UpdatePublisher;
use crate::filesystem::File;
use crate::filesystem::FileList;
use crate::filesystem::Filesystem;
use crate::filesystem::resolve;
use std::collections::HashMap;
use std::collections::HashSet;

use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::DisplayOptions;
use super::DisplayOptionsInner;
use super::MainWindowState;


#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortingKey {
    #[default]
    Name,
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
    navigation_state: RwLock<(usize, tokio::sync::watch::Sender<()>, Arc<FileList>)>,
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
            navigation_state: RwLock::new((0, tx, Arc::new(FileList::new(path, Vec::new())))),
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
            tokio::select! {
                _ = self.fs.poll_changes(self.path()) => {
                    eprintln!("Change detected");
                }
                _ = rx.changed() =>  {
                    continue;
                }
            };

            let cloned = self.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = cloned.refresh().await {
                    eprintln!("failed to refresh pane: {}", e);
                }
                cloned.publisher.publish().unwrap();
            });
        }
    }

    pub fn path(&self) -> PathBuf {
        self.navigation_state.read().2.path().to_path_buf()
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
        let navigation_state = self.navigation_state.read();
        let mut view_state: parking_lot::lock_api::RwLockWriteGuard<
            parking_lot::RawRwLock,
            PaneViewState,
        > = self.view_state.write();

        view_state.update(display_options, &*navigation_state.2);
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        self.navigate(".").await?;
        Ok(())
    }

    pub async fn navigate<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let (epoch, original_fl, target) = {
            let mut navigation_state = self.navigation_state.write();
            let version = navigation_state.0;
            let target = resolve(&navigation_state.2.path().join(path));

            navigation_state.0 += 1;
            let old_fl = std::mem::replace(&mut navigation_state.2, Arc::new(FileList::new(target.clone(), Vec::new())));

            let mut ws = self.view_state_mut();
            ws.pending_path = Some(target.clone());
            ws.busy = true;

            (version + 1, old_fl, target)
        };

        let _ = self.publisher.publish();

        let new_file_list = match self.cancellable(self.fs.list_files(target.clone())).await {
            Ok(ret) => Arc::new(ret),
            Err(e) => {
                // Restore the old navigation & view-state, unless another navigation got there first.
                let mut ws = self.view_state_mut();
                let mut navigation_state = self.navigation_state.write();
                if navigation_state.0 == epoch {
                    navigation_state.0 = epoch + 1;
                    navigation_state.2 = original_fl.clone();

                    ws.busy = false;
                    ws.pending_path = None
                }
                return match e {
                    Error::Cancelled => Ok(()),
                    e => Err(e),
                };
            }
        };

        let mut ws = self.view_state.write();
        let mut navigation_state = self.navigation_state.write();
        if navigation_state.0 != epoch {
            return Ok(());
        }
        navigation_state.0 = epoch + 1;
        navigation_state.2 = new_file_list.clone();

        if original_fl.path() != new_file_list.path() {
            let _ = navigation_state.1.send(());
        }

        let display_options = self.display_options.0.read().clone();

        ws.busy = false;
        ws.set_filter(None);
        ws.selected.clear();
        ws.focused = None;
        ws.pending_path = None;

        ws.update(display_options, &*new_file_list);

        if target == new_file_list.path() {
            ws.focus_descendant(&original_fl.path());
        } else {
            ws.focus_descendant(&target);
        }

        Ok(())
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
    pub busy: bool,

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
        if self.busy {
            return;
        }

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
