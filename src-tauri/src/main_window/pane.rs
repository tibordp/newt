use crate::common::Error;
use crate::common::ToUnix;
use std::collections::HashMap;
use std::collections::HashSet;

#[cfg(target_family = "unix")]
use std::os::unix::prelude::MetadataExt;
#[cfg(target_family = "windows")]
use std::os::windows::prelude::MetadataExt;

use std::path::PathBuf;

use super::DisplayOptions;

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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortingKey {
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

/// View model for a pane.
#[derive(Clone, serde::Serialize)]
pub struct PaneViewState {
    pub path: PathBuf,
    pub sorting: Sorting,
    pub files: Vec<File>,
    pub focused: Option<String>,
    pub selected: HashSet<String>,

    pub active: bool,
    pub filter: Option<String>,

    #[serde(skip)]
    raw_files: Vec<File>,
    #[serde(skip)]
    display_options: DisplayOptions,
    #[serde(skip)]
    file_lookup: HashMap<String, usize>,
    #[serde(skip)]
    filter_regex: Option<regex::Regex>,
}

impl PaneViewState {
    pub fn create(path: PathBuf, display_options: DisplayOptions) -> Result<Self, Error> {
        let mut ret = Self {
            path,
            sorting: Sorting {
                key: SortingKey::Name,
                asc: true,
            },
            files: Vec::new(),
            focused: None,
            selected: HashSet::new(),
            active: false,
            filter: None,
            filter_regex: None,
            file_lookup: HashMap::new(),
            display_options,
            raw_files: Vec::new(),
        };
        ret.refresh()?;

        Ok(ret)
    }

    fn reload(&mut self) -> Result<(), Error> {
        let mut ret = std::fs::read_dir(&self.path)?
            .map(|entry| {
                let entry = entry?;
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

                Ok(File {
                    name: name.clone(),
                    size: metadata.len(),
                    is_dir,
                    is_symlink: file_type.is_symlink(),
                    is_hidden: name.starts_with('.'),
                    mode,
                    modified: metadata.modified().map(|t| t.to_unix()).ok(),
                    accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                    created: metadata.created().map(|t| t.to_unix()).ok(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        if let Some(parent) = self.path.parent() {
            let metadata = parent.metadata()?;

            #[cfg(target_family = "unix")]
            let mode = metadata.mode();
            #[cfg(target_family = "windows")]
            let mode = metadata.file_attributes() as _;

            ret.push(File {
                name: "..".to_string(),
                size: metadata.len(),
                is_dir: true,
                is_symlink: false,
                is_hidden: false,
                mode,
                modified: metadata.modified().map(|t| t.to_unix()).ok(),
                accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                created: metadata.created().map(|t| t.to_unix()).ok(),
            });
        }

        self.raw_files = ret;
        Ok(())
    }

    pub fn filter(&mut self) {
        let display_options = self.display_options.0.read();
        self.files = self
            .raw_files
            .iter()
            .filter(|f| !f.is_hidden || display_options.show_hidden || f.name == "..")
            .cloned()
            .collect();
    }

    pub fn sort(&mut self) {
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

    pub fn update_focus(&mut self) {
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

    pub fn refresh(&mut self) -> Result<(), Error> {
        // the path is expected to be canonical by now

        match self.reload() {
            Err(Error::Io(e)) => match e.kind() {
                // Directory we were in might have been deleted. In this case we go up until we find a
                // directory that exists.
                std::io::ErrorKind::NotFound => {
                    if self.path.pop() {
                        return self.refresh();
                    } else {
                        return Err(e.into());
                    }
                }
                std::io::ErrorKind::NotADirectory => {
                    self.focused = self
                        .path
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string());

                    if self.path.pop() {
                        return self.refresh();
                    } else {
                        return Err(e.into());
                    }
                }
                _ => return Err(e.into()),
            },
            Err(e) => return Err(e),
            _ => {}
        }

        self.filter();
        self.sort();
        self.update_focus();

        Ok(())
    }

    /// If the selection is empty, returns the focused file.
    /// Otherwise, returns the selected files. The focused file is NOT included
    /// in the selection in this case.
    pub fn get_effective_selection(&self) -> Vec<PathBuf> {
        if self.selected.is_empty() {
            self.focused.iter().map(|s| self.path.join(s)).collect()
        } else {
            self.selected.iter().map(|s| self.path.join(s)).collect()
        }
    }

    pub fn navigate(&mut self, path: &str) -> Result<(), Error> {
        let mut new_state = self.clone();

        let previous = if path == ".." {
            let p = new_state
                .path
                .file_name()
                .map(|f| f.to_string_lossy().to_string());
            // We are at the root
            if !new_state.path.pop() {
                return Ok(());
            }
            p
        } else {
            new_state.path.push(path);
            None
        };

        new_state.path = new_state.path.canonicalize()?;
        new_state.focused = previous;
        new_state.refresh()?;
        new_state.filter = None;
        // Focus the folder we just came from
        new_state.selected.clear();
        new_state.update_focus();

        *self = new_state;
        Ok(())
    }

    pub fn focus(&mut self, filename: String) {
        self.update_filter(None);
        self.focused = Some(filename);
        self.update_focus();
    }

    pub fn set_sorting(&mut self, sorting: Sorting) {
        self.sorting = sorting;
        self.sort();
    }

    pub fn toggle_selected(&mut self, filename: String, focus_next: bool) {
        if !self.selected.remove(&filename) && self.file_lookup.contains_key(&filename) {
            self.selected.insert(filename.clone());
        }

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

        self.focused = Some(filename);
    }

    pub fn select_all(&mut self) {
        self.update_filter(None);
        self.filter_regex = None;
        self.selected = self.file_lookup.keys().cloned().collect();
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

    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }
}
