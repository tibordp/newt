use crate::common::{Error, ToUnix};
use std::{
    collections::{HashMap, HashSet},
    os::unix::prelude::MetadataExt,
    path::PathBuf,
};

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
    file_lookup: HashMap<String, usize>,
}

impl PaneViewState {
    pub fn create(path: PathBuf) -> Result<Self, Error> {
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

            file_lookup: HashMap::new(),
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

                Ok(File {
                    name: name.clone(),
                    size: metadata.len(),
                    is_dir: file_type.is_dir(),
                    is_hidden: name.starts_with('.'),
                    mode: metadata.mode(),
                    modified: metadata.modified().map(|t| t.to_unix()).ok(),
                    accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                    created: metadata.created().map(|t| t.to_unix()).ok(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        if let Some(parent) = self.path.parent() {
            let metadata = parent.metadata()?;
            ret.push(File {
                name: "..".to_string(),
                size: metadata.len(),
                is_dir: true,
                is_hidden: false,
                mode: metadata.mode(),
                modified: metadata.modified().map(|t| t.to_unix()).ok(),
                accessed: metadata.accessed().map(|t| t.to_unix()).ok(),
                created: metadata.created().map(|t| t.to_unix()).ok(),
            });
        }

        self.files = ret;
        Ok(())
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

    // Public API

    pub fn refresh(&mut self) -> Result<(), Error> {
        match self.reload() {
            // Directory we were in might have been deleted. In this case we go up until we find a
            // directory that exists.
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                if self.path.pop() {
                    return self.refresh();
                } else {
                    return Err(Error::Io(e));
                }
            }
            _ => {}
        }
        self.sort();
        self.update_focus();

        Ok(())
    }

    pub fn navigate(&mut self, path: String) -> Result<(), Error> {
        let mut new_state = self.clone();

        let previous = if path == ".." {
            let p = new_state.path.file_name().map(|f| f.to_string_lossy().to_string());
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
        new_state.refresh()?;
        new_state.filter = None;
        // Focus the folder we just came from
        new_state.focused = previous
            .or_else(|| new_state.files.get(0).map(|f| f.name.clone()));
        new_state.selected.clear();
        new_state.update_focus();

        *self = new_state;
        Ok(())
    }

    pub fn focus(&mut self, filename: String) {
        self.filter = None;
        self.focused = Some(filename);
        self.update_focus();
    }

    pub fn set_sorting(&mut self, sorting: Sorting) {
        self.sorting = sorting;
        self.sort();
    }

    pub fn toggle_selected(&mut self) {
        if let Some(focused) = self.focused.as_ref() {
            self.filter = None;
            if !self.selected.remove(focused) && self.file_lookup.contains_key(focused) {
                self.selected.insert(focused.clone());
            }
            self.relative_jump(1);
        }
    }

    pub fn select_all(&mut self) {
        self.filter = None;
        self.selected = self.file_lookup.keys().cloned().collect();
    }

    pub fn deselect_all(&mut self) {
        self.filter = None;
        self.selected.clear();
    }

    pub fn set_filter(&mut self, filter: Option<String>) {
        let Some(filter) = filter else {
            self.filter = None;
            return;
        };

        let start_index = *self
            .focused
            .as_deref()
            .map(|f| self.file_lookup.get(f).unwrap())
            .unwrap_or(&0);

        // Search in the down direction first
        for f in self.files.iter().skip(start_index) {
            if f.name.starts_with(&filter) {
                self.focused = Some(f.name.clone());
                self.filter = Some(filter);
                return;
            }
        }

        // Then search in the up direction
        for f in self.files.iter().take(start_index).rev() {
            if f.name.starts_with(&filter) {
                self.focused = Some(f.name.clone());
                self.filter = Some(filter);
                return;
            }
        }

        if self.filter.is_none() {
            self.filter = Some(Default::default());
        }
    }

    pub fn relative_jump(&mut self, mut offset: i32) {
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

#[derive(Clone, serde::Serialize)]
pub struct File {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub is_hidden: bool,
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
