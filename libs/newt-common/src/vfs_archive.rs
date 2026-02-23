use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use crate::file_reader::{FileChunk, FileInfo};
use crate::filesystem::{File, FileList, ListFilesOptions, Mode};
use crate::vfs::{Vfs, VfsCapabilities, VfsDirEntry, VfsEntryMetadata, VfsPath};
use crate::Error;

/// A read-only VFS backed by a ZIP archive loaded in memory.
pub struct ArchiveVfs {
    origin_path: VfsPath,
    archive_data: Vec<u8>,
    name: String,
}

impl ArchiveVfs {
    /// Open an archive by reading the entire file into memory via the host VFS.
    pub async fn open(host_vfs: &dyn Vfs, host_path: &Path, origin: VfsPath) -> Result<Self, Error> {
        let mut reader = host_vfs.open_read(host_path).await?;

        let data = tokio::task::spawn_blocking(move || {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf)?;
            Ok::<_, Error>(buf)
        })
        .await??;

        // Validate it's a valid zip
        {
            let cursor = Cursor::new(&data);
            zip::ZipArchive::new(cursor).map_err(|e| {
                Error::Custom(format!("not a valid archive: {}", e))
            })?;
        }

        let name = host_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".to_string());

        Ok(Self {
            origin_path: origin,
            archive_data: data,
            name,
        })
    }

    fn open_archive(&self) -> Result<zip::ZipArchive<Cursor<&[u8]>>, Error> {
        let cursor = Cursor::new(self.archive_data.as_slice());
        zip::ZipArchive::new(cursor).map_err(|e| Error::Custom(format!("archive error: {}", e)))
    }

    /// Normalize a path: strip leading "/" and ensure consistent representation
    fn normalize_path(path: &Path) -> PathBuf {
        let s = path.to_string_lossy();
        let s = s.trim_start_matches('/');
        if s.is_empty() {
            PathBuf::from("")
        } else {
            PathBuf::from(s)
        }
    }
}

#[async_trait::async_trait]
impl Vfs for ArchiveVfs {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities::READ
    }

    async fn list_files(&self, path: &Path, _opts: ListFilesOptions) -> Result<FileList, Error> {
        let prefix = Self::normalize_path(path);
        let prefix_str = if prefix.as_os_str().is_empty() {
            String::new()
        } else {
            format!("{}/", prefix.display())
        };

        let mut archive = self.open_archive()?;

        let mut files = Vec::new();
        let mut seen_dirs = HashSet::new();

        for i in 0..archive.len() {
            let entry = archive.by_index_raw(i).map_err(|e| {
                Error::Custom(format!("archive entry error: {}", e))
            })?;

            let entry_name = entry.name().to_string();

            // Check if this entry is a direct child of our prefix
            let relative = if prefix_str.is_empty() {
                entry_name.as_str()
            } else if let Some(rel) = entry_name.strip_prefix(&prefix_str) {
                rel
            } else {
                continue;
            };

            // Skip self
            if relative.is_empty() {
                continue;
            }

            // For direct children: no "/" in the remaining path (or only trailing "/")
            let trimmed = relative.trim_end_matches('/');
            if trimmed.contains('/') {
                // Not a direct child, but may imply a virtual directory
                // Extract the first path component
                let first_component = trimmed.split('/').next().unwrap();
                if seen_dirs.insert(first_component.to_string()) {
                    files.push(File {
                        name: first_component.to_string(),
                        size: None,
                        is_dir: true,
                        is_hidden: first_component.starts_with('.'),
                        is_symlink: false,
                        user: None,
                        group: None,
                        mode: Mode::default(),
                        modified: None,
                        accessed: None,
                        created: None,
                    });
                }
                continue;
            }

            let is_dir = relative.ends_with('/');
            let display_name = trimmed.to_string();

            if is_dir {
                if seen_dirs.insert(display_name.clone()) {
                    files.push(File {
                        name: display_name,
                        size: None,
                        is_dir: true,
                        is_hidden: trimmed.starts_with('.'),
                        is_symlink: false,
                        user: None,
                        group: None,
                        mode: Mode::default(),
                        modified: None,
                        accessed: None,
                        created: None,
                    });
                }
            } else {
                files.push(File {
                    name: display_name,
                    size: Some(entry.size()),
                    is_dir: false,
                    is_hidden: trimmed.starts_with('.'),
                    is_symlink: false,
                    user: None,
                    group: None,
                    mode: Mode::default(),
                    modified: entry.last_modified().and_then(|dt| {
                        // Convert zip DateTime to unix timestamp (approximate)
                        let ts = chrono_from_zip_datetime(dt)?;
                        Some(ts as i128 * 1_000_000_000)
                    }),
                    accessed: None,
                    created: None,
                });
            }
        }

        Ok(FileList::new(
            VfsPath::default(), // will be set by caller
            files,
            None,
        ))
    }

    async fn poll_changes(&self, _path: &Path) -> Result<(), Error> {
        // Archives don't change — block forever
        futures::future::pending().await
    }

    async fn file_info(&self, path: &Path) -> Result<FileInfo, Error> {
        let normalized = Self::normalize_path(path);
        let name = normalized.to_string_lossy().to_string();

        let mut archive = self.open_archive()?;

        let entry = archive.by_name(&name).map_err(|_| {
            Error::Custom(format!("entry not found: {}", name))
        })?;

        let size = entry.size();
        // Read first chunk to detect binary
        let mut buf = vec![0u8; 8192.min(size as usize)];
        let reader = entry;
        let mut limited = reader.take(8192);
        let n = limited.read(&mut buf).map_err(|e| Error::Io(e))?;
        let is_binary = buf[..n].contains(&0);

        Ok(FileInfo { size, is_binary })
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let normalized = Self::normalize_path(path);
        let name = normalized.to_string_lossy().to_string();

        let mut archive = self.open_archive()?;

        let mut entry = archive.by_name(&name).map_err(|_| {
            Error::Custom(format!("entry not found: {}", name))
        })?;

        let total_size = entry.size();

        // Read entire entry (zip doesn't support seeking efficiently)
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(Error::Io)?;

        let start = (offset as usize).min(data.len());
        let end = ((offset + length) as usize).min(data.len());
        let chunk = data[start..end].to_vec();

        Ok(FileChunk {
            data: chunk,
            offset,
            total_size,
        })
    }

    async fn open_read(&self, path: &Path) -> Result<Box<dyn Read + Send>, Error> {
        let normalized = Self::normalize_path(path);
        let name = normalized.to_string_lossy().to_string();

        let mut archive = self.open_archive()?;

        let mut entry = archive.by_name(&name).map_err(|_| {
            Error::Custom(format!("entry not found: {}", name))
        })?;

        // Decompress entire entry into memory
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(Error::Io)?;

        Ok(Box::new(Cursor::new(data)))
    }

    async fn read_link(&self, _path: &Path) -> Result<PathBuf, Error> {
        Err(Error::NotSupported)
    }

    async fn symlink_metadata(&self, path: &Path) -> Result<VfsEntryMetadata, Error> {
        let normalized = Self::normalize_path(path);
        let name = normalized.to_string_lossy().to_string();

        let mut archive = self.open_archive()?;

        // Try exact match first (file)
        for i in 0..archive.len() {
            let entry = archive.by_index_raw(i).map_err(|e| {
                Error::Custom(format!("archive entry error: {}", e))
            })?;
            let entry_name = entry.name().to_string();
            let trimmed = entry_name.trim_end_matches('/');

            if trimmed == name {
                let is_dir = entry_name.ends_with('/');
                return Ok(VfsEntryMetadata {
                    is_file: !is_dir,
                    is_dir,
                    is_symlink: false,
                    size: entry.size(),
                });
            }
        }

        // Check if it's a virtual directory (entries exist under this prefix)
        let dir_prefix = format!("{}/", name);
        for i in 0..archive.len() {
            let entry = archive.by_index_raw(i).map_err(|e| {
                Error::Custom(format!("archive entry error: {}", e))
            })?;
            if entry.name().starts_with(&dir_prefix) {
                return Ok(VfsEntryMetadata {
                    is_file: false,
                    is_dir: true,
                    is_symlink: false,
                    size: 0,
                });
            }
        }

        Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("entry not found: {}", name),
        )))
    }

    async fn read_dir(&self, path: &Path) -> Result<Vec<VfsDirEntry>, Error> {
        let prefix = Self::normalize_path(path);
        let prefix_str = if prefix.as_os_str().is_empty() {
            String::new()
        } else {
            format!("{}/", prefix.display())
        };

        let mut archive = self.open_archive()?;

        let mut entries = Vec::new();
        let mut seen_dirs = HashSet::new();

        for i in 0..archive.len() {
            let entry = archive.by_index_raw(i).map_err(|e| {
                Error::Custom(format!("archive entry error: {}", e))
            })?;

            let entry_name = entry.name().to_string();

            let relative = if prefix_str.is_empty() {
                entry_name.as_str()
            } else if let Some(rel) = entry_name.strip_prefix(&prefix_str) {
                rel
            } else {
                continue;
            };

            if relative.is_empty() {
                continue;
            }

            let trimmed = relative.trim_end_matches('/');
            if trimmed.contains('/') {
                let first_component = trimmed.split('/').next().unwrap();
                if seen_dirs.insert(first_component.to_string()) {
                    entries.push(VfsDirEntry {
                        name: OsString::from(first_component),
                        metadata: VfsEntryMetadata {
                            is_file: false,
                            is_dir: true,
                            is_symlink: false,
                            size: 0,
                        },
                    });
                }
                continue;
            }

            let is_dir = relative.ends_with('/');
            let display_name = trimmed.to_string();

            if is_dir {
                if seen_dirs.insert(display_name.clone()) {
                    entries.push(VfsDirEntry {
                        name: OsString::from(display_name),
                        metadata: VfsEntryMetadata {
                            is_file: false,
                            is_dir: true,
                            is_symlink: false,
                            size: 0,
                        },
                    });
                }
            } else {
                entries.push(VfsDirEntry {
                    name: OsString::from(display_name),
                    metadata: VfsEntryMetadata {
                        is_file: true,
                        is_dir: false,
                        is_symlink: false,
                        size: entry.size(),
                    },
                });
            }
        }

        Ok(entries)
    }

    fn origin(&self) -> Option<&VfsPath> {
        Some(&self.origin_path)
    }
}

/// Convert a zip DateTime to a Unix timestamp.
fn chrono_from_zip_datetime(dt: zip::DateTime) -> Option<i64> {
    // zip::DateTime fields: year, month, day, hour, minute, second
    // Simple conversion without proper timezone handling
    // Use a rough calculation
    let year = dt.year() as i64;
    let month = dt.month() as i64;
    let day = dt.day() as i64;
    let hour = dt.hour() as i64;
    let minute = dt.minute() as i64;
    let second = dt.second() as i64;

    // Approximate days from epoch (1970-01-01)
    // This is a simplified calculation
    let mut days = 0i64;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[m as usize] as i64;
        if m == 2 && is_leap_year(year) {
            days += 1;
        }
    }
    days += day - 1;

    let timestamp = days * 86400 + hour * 3600 + minute * 60 + second;
    Some(timestamp)
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
