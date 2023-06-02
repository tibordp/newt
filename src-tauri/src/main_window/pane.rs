use parking_lot::RwLock;
use parking_lot::RwLockReadGuard;
use parking_lot::RwLockWriteGuard;

use crate::common::Error;
use crate::common::ToUnix;
use std::collections::HashMap;
use std::collections::HashSet;

#[cfg(target_family = "unix")]
use std::os::unix::prelude::MetadataExt;
#[cfg(target_family = "windows")]
use std::os::windows::prelude::MetadataExt;

use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use super::DisplayOptions;
use super::DisplayOptionsInner;

#[derive(Clone, serde::Serialize)]
pub struct File {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub is_symlink: bool,
    pub mode: u32,
    pub modified: Option<i128>,
    pub accessed: Option<i128>,
    pub created: Option<i128>,
}

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

pub struct FileList {
    path: PathBuf,
    files: Vec<File>,
}

impl FileList {
    pub fn new(path: PathBuf, files: Vec<File>) -> Self {
        Self { path, files }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn files(&self) -> &[File] {
        &self.files
    }
}

fn resolve(path: &Path) -> PathBuf {
    assert!(path.is_absolute());
    let mut ret = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                ret.pop();
            }
            component => ret.push(component.as_os_str()),
        }
    }
    ret
}

fn list_files(path: &Path) -> Result<FileList, Error> {
    fn reload(path: &Path) -> Result<Vec<File>, Error> {
        let mut ret = Vec::new();
        if let Some(parent) = path.parent() {
            let metadata = parent.symlink_metadata()?;

            #[cfg(target_family = "unix")]
            let mode = metadata.mode();
            #[cfg(target_family = "windows")]
            let mode = metadata.file_attributes() as _;

            ret.push(File {
                name: "..".to_string(),
                size: metadata.len(),
                is_dir: true,
                is_symlink: metadata.is_symlink(),
                is_hidden: false,
                mode,
                modified: metadata.modified().map(|t| t.to_unix()).ok(),
                accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                created: metadata.created().map(|t| t.to_unix()).ok(),
            });
        }

        for maybe_entry in std::fs::read_dir(path)? {
            let entry = maybe_entry?;
            let metadata = entry.metadata()?;
            let file_type = metadata.file_type();

            let name = entry.file_name().into_string().unwrap();
            let mut is_dir = file_type.is_dir();

            if file_type.is_symlink() {
                let target_metadata = std::fs::metadata(entry.path());
                // If we e.g. don't have permission to read the target, we show the link details
                if let Ok(target_metadata) = target_metadata {
                    is_dir = target_metadata.is_dir();
                }
            }

            #[cfg(target_family = "unix")]
            let mode = metadata.mode();
            #[cfg(target_family = "windows")]
            let mode = metadata.file_attributes() as _;

            ret.push(File {
                name: name.clone(),
                size: metadata.len(),
                is_dir,
                is_symlink: file_type.is_symlink(),
                is_hidden: name.starts_with('.'),
                mode,
                modified: metadata.modified().map(|t| t.to_unix()).ok(),
                accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                created: metadata.created().map(|t| t.to_unix()).ok(),
            });
        }

        Ok(ret)
    }

    assert!(path.is_absolute());

    let mut path = resolve(path);
    loop {
        match reload(&path) {
            Ok(files) => return Ok(FileList::new(path, files)),
            Err(Error::Io(e)) => match e.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
                    if !path.pop() {
                        return Err(e.into());
                    }
                }
                _ => return Err(e.into()),
            },
            Err(e) => return Err(e),
        }
    }
}

pub struct Pane {
    file_list: RwLock<FileList>,
    view_state: RwLock<PaneViewState>,
    display_options: DisplayOptions,
}

impl Pane {
    pub fn new(path: PathBuf, display_options: DisplayOptions) -> Self {
        Self {
            file_list: RwLock::new(FileList::new(path, Vec::new())),
            view_state: RwLock::new(PaneViewState::default()),
            display_options,
        }
    }

    pub fn path(&self) -> PathBuf {
        self.file_list.read().path().to_path_buf()
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

    pub fn refresh(&self) -> Result<(), Error> {
        let original_path = self.file_list.read().path().to_path_buf();

        let file_list = list_files(&original_path)?;

        let display_options = self.display_options.0.read().clone();
        let mut self_file_list = self.file_list.write();
        let mut view_state = self.view_state.write();

        view_state.update(display_options, &file_list);
        *self_file_list = file_list;
        view_state.focus_descendant(&original_path);

        Ok(())
    }

    pub fn navigate<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        let original_path = self.path();
        let target = original_path.join(path);

        let file_list = list_files(&target)?;

        let display_options = self.display_options.0.read().clone();
        let mut self_file_list = self.file_list.write();
        let mut view_state = self.view_state.write();

        view_state.set_filter(None);
        view_state.selected.clear();
        view_state.focused = None;

        view_state.update(display_options, &file_list);
        *self_file_list = file_list;
        view_state.focus_descendant(&target);

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
    pub sorting: Sorting,
    pub files: Vec<File>,
    pub focused: Option<String>,
    pub selected: HashSet<String>,
    pub filter: Option<String>,

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
