// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{os::unix::fs::MetadataExt, time::SystemTime};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct File {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub mode: u32,
    pub modified: Option<u128>,
    pub accessed: Option<u128>,
    pub created: Option<u128>,
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortingKey {
    Name,
    Size,
    Modified,
    Accessed,
    Created,
}

#[derive(serde::Deserialize)]
pub struct Sorting {
    pub key: SortingKey,
    pub asc: bool,
}

// we must manually implement serde::Serialize
impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

#[tauri::command]
fn directory_list(path: &str, sorting: Sorting) -> Result<Vec<File>, Error> {
    let path: std::path::PathBuf = path.into();
    let mut ret = std::fs::read_dir(&path)?
        .enumerate()
        .map(|(index, entry)| {
            let entry = entry?;
            let id = index.to_string();
            let metadata = entry.metadata()?;
            let file_type = metadata.file_type();
            let is_dir = file_type.is_dir();
            let name = entry.file_name().into_string().unwrap();
            let size = metadata.len();
            let mode = metadata.mode();
            let modified = metadata
                .modified()
                .map(|t| {
                    t.duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                })
                .ok();
            let accessed = metadata
                .accessed()
                .map(|t| {
                    t.duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                })
                .ok();
            let created = metadata
                .created()
                .map(|t| {
                    t.duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_millis()
                })
                .ok();
            let is_hidden = name.starts_with('.');

            Ok(File {
                id,
                name,
                size,
                is_dir,
                is_hidden,
                mode,
                modified,
                accessed,
                created,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    ret.sort_by(|a, b| {
        // Directories first
        if a.is_dir && !b.is_dir {
            return std::cmp::Ordering::Less;
        } else if !a.is_dir && b.is_dir {
            return std::cmp::Ordering::Greater;
        }

        let (a, b) = if sorting.asc { (a, b) } else { (b, a) };
        match sorting.key {
            SortingKey::Name => a.name.cmp(&b.name),
            SortingKey::Size => a.size.cmp(&b.size),
            SortingKey::Modified => a.modified.unwrap_or(0).cmp(&b.modified.unwrap_or(0)),
            SortingKey::Accessed => a.accessed.unwrap_or(0).cmp(&b.accessed.unwrap_or(0)),
            SortingKey::Created => a.created.unwrap_or(0).cmp(&b.created.unwrap_or(0)),
        }
    });

    if let Some(parent) = path.parent() {
        let metadata = parent.metadata()?;

        let size = metadata.len();
        let mode = metadata.mode();
        let modified = metadata
            .modified()
            .map(|t| {
                t.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            })
            .ok();
        let accessed = metadata
            .accessed()
            .map(|t| {
                t.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            })
            .ok();
        let created = metadata
            .created()
            .map(|t| {
                t.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            })
            .ok();

        ret.insert(
            0,
            File {
                id: "..".to_string(),
                name: "..".to_string(),
                size,
                is_dir: true,
                is_hidden: false,
                mode,
                modified,
                accessed,
                created,
            },
        );
    }

    Ok(ret)
}

#[tauri::command]
fn navigate(base_path: &str, path: &str) -> Result<String, Error> {
    let mut ret: std::path::PathBuf = base_path.into();
    ret.push(path);
    ret = ret.canonicalize()?;

    // Check if we can list the directory (permissions, ...)
    std::fs::read_dir(&ret)?;

    Ok(ret.to_string_lossy().into_owned())
}

#[tauri::command]
fn open(base_path: &str, path: &str) -> Result<(), Error> {
    let path = std::path::PathBuf::from(base_path).join(path);

    opener::open(path).unwrap();

    Ok(())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![directory_list, navigate, open])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
