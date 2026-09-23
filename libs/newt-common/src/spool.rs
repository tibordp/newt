//! Temporary storage for bytes that must be held before they can be handed
//! on: an archive whose destination cannot patch its start header, a
//! download staged for a sequential upload. A [`Spool`] keeps its bytes in
//! memory up to a threshold and spills to a temp file past it; every byte
//! held counts against the [`Spooler`]'s quota, released when the spool
//! is dropped. The temp file goes with it.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::Error;

#[derive(Debug, Clone)]
pub struct SpoolConfig {
    /// Bytes a spool holds in memory before it spills to disk.
    pub memory_threshold: u64,
    /// Bytes all of a spooler's spools may hold together, in memory and
    /// on disk; `None` is unlimited.
    pub quota: Option<u64>,
    /// Where spills go; `None` is the system temp directory.
    pub dir: Option<PathBuf>,
}

impl Default for SpoolConfig {
    fn default() -> Self {
        Self {
            memory_threshold: 32 << 20,
            quota: None,
            dir: None,
        }
    }
}

/// Hands out spools and accounts for what they hold.
pub struct Spooler {
    config: SpoolConfig,
    used: AtomicU64,
}

impl Spooler {
    pub fn new(config: SpoolConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            used: AtomicU64::new(0),
        })
    }

    pub fn open(self: &Arc<Self>) -> Spool {
        Spool {
            spooler: self.clone(),
            storage: Storage::Memory(Vec::new()),
            len: 0,
        }
    }

    /// Bytes currently held by every spool of this spooler.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    fn reserve(&self, bytes: u64) -> Result<(), Error> {
        let limit = self.config.quota.unwrap_or(u64::MAX);
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let next = used.saturating_add(bytes);
            if next > limit {
                return Err(Error::custom(format!(
                    "temporary storage quota of {} MiB exceeded",
                    limit >> 20
                )));
            }
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Ok(()),
                Err(now) => used = now,
            }
        }
    }

    fn release(&self, bytes: u64) {
        self.used.fetch_sub(bytes, Ordering::Relaxed);
    }
}

enum Storage {
    Memory(Vec<u8>),
    /// The temp path deletes the file when dropped.
    Disk(tokio::fs::File, tempfile::TempPath),
}

/// A spool being read back, with the temp path kept for its drop.
enum Inner {
    Memory(std::io::Cursor<Vec<u8>>),
    Disk {
        file: tokio::fs::File,
        _path: tempfile::TempPath,
    },
}

/// An append-only byte buffer with in-place patching, read back once it
/// is complete.
pub struct Spool {
    spooler: Arc<Spooler>,
    storage: Storage,
    len: u64,
}

impl Spool {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub async fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        self.spooler.reserve(data.len() as u64)?;
        self.len += data.len() as u64;
        if let Storage::Memory(buf) = &mut self.storage
            && self.len > self.spooler.config.memory_threshold
        {
            let held = std::mem::take(buf);
            let dir = self
                .spooler
                .config
                .dir
                .clone()
                .unwrap_or_else(std::env::temp_dir);
            let (file, path) = tempfile::Builder::new()
                .prefix("newt-spool-")
                .tempfile_in(dir)
                .map_err(|e| Error::custom(format!("could not create spool file: {}", e)))?
                .into_parts();
            let mut file = tokio::fs::File::from_std(file);
            file.write_all(&held).await?;
            self.storage = Storage::Disk(file, path);
        }
        match &mut self.storage {
            Storage::Memory(buf) => buf.extend_from_slice(data),
            Storage::Disk(file, _) => file.write_all(data).await?,
        }
        Ok(())
    }

    /// Overwrite bytes already written; the range must lie within
    /// [`len`](Self::len).
    pub async fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), Error> {
        if offset.saturating_add(data.len() as u64) > self.len {
            return Err(Error::custom("spool patch past the end of the data"));
        }
        match &mut self.storage {
            Storage::Memory(buf) => {
                buf[offset as usize..offset as usize + data.len()].copy_from_slice(data)
            }
            Storage::Disk(file, _) => {
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                file.write_all(data).await?;
                file.seek(std::io::SeekFrom::End(0)).await?;
            }
        }
        Ok(())
    }

    /// Read the spool back from the start; the storage and its quota are
    /// released when the reader is dropped.
    pub async fn into_reader(mut self) -> Result<SpoolReader, Error> {
        let storage = std::mem::replace(&mut self.storage, Storage::Memory(Vec::new()));
        // The quota moves to the reader; the spool's own drop then has
        // nothing to release.
        let held = Held {
            spooler: self.spooler.clone(),
            len: self.len,
        };
        self.len = 0;
        let inner = match storage {
            Storage::Memory(buf) => Inner::Memory(std::io::Cursor::new(buf)),
            Storage::Disk(mut file, path) => {
                file.flush().await?;
                file.seek(std::io::SeekFrom::Start(0)).await?;
                Inner::Disk { file, _path: path }
            }
        };
        Ok(SpoolReader { inner, _held: held })
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        self.spooler.release(self.len);
    }
}

/// Quota held until the bytes are gone.
struct Held {
    spooler: Arc<Spooler>,
    len: u64,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.spooler.release(self.len);
    }
}

pub struct SpoolReader {
    inner: Inner,
    _held: Held,
}

impl AsyncRead for SpoolReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut self.inner {
            Inner::Memory(cursor) => Pin::new(cursor).poll_read(cx, buf),
            Inner::Disk { file, .. } => Pin::new(file).poll_read(cx, buf),
        }
    }
}

impl SpoolReader {
    /// Convenience for tests and small spools.
    pub async fn read_all(mut self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.read_to_end(&mut out).await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spooler(threshold: u64, quota: Option<u64>) -> Arc<Spooler> {
        Spooler::new(SpoolConfig {
            memory_threshold: threshold,
            quota,
            dir: None,
        })
    }

    #[tokio::test]
    async fn stays_in_memory_under_the_threshold_and_spills_past_it() {
        let spooler = spooler(100, None);
        let mut spool = spooler.open();
        spool.write(&[1u8; 60]).await.unwrap();
        assert!(matches!(spool.storage, Storage::Memory(_)));
        spool.write(&[2u8; 60]).await.unwrap();
        assert!(matches!(spool.storage, Storage::Disk(..)));
        spool.write_at(59, &[9, 9]).await.unwrap();
        spool.write(&[3u8; 5]).await.unwrap();
        assert_eq!(spool.len(), 125);
        assert_eq!(spooler.used(), 125);
        let path = match &spool.storage {
            Storage::Disk(_, path) => path.to_path_buf(),
            _ => unreachable!(),
        };
        let data = spool.into_reader().await.unwrap().read_all().await.unwrap();
        let mut expected = vec![1u8; 60];
        expected.extend_from_slice(&[2u8; 60]);
        expected.extend_from_slice(&[3u8; 5]);
        expected[59] = 9;
        expected[60] = 9;
        assert_eq!(data, expected);
        assert_eq!(spooler.used(), 0);
        assert!(!path.exists(), "spool file survives its reader");
    }

    #[tokio::test]
    async fn in_memory_patch_and_read_back() {
        let spooler = spooler(1 << 20, None);
        let mut spool = spooler.open();
        spool.write(b"hello world").await.unwrap();
        spool.write_at(6, b"there").await.unwrap();
        assert!(spool.write_at(7, b"there").await.is_err());
        let data = spool.into_reader().await.unwrap().read_all().await.unwrap();
        assert_eq!(data, b"hello there");
    }

    #[tokio::test]
    async fn quota_spans_the_spooler_and_frees_on_drop() {
        let spooler = spooler(10, Some(100));
        let mut a = spooler.open();
        a.write(&[0u8; 70]).await.unwrap();
        let mut b = spooler.open();
        assert!(b.write(&[0u8; 40]).await.is_err());
        b.write(&[0u8; 30]).await.unwrap();
        drop(a);
        assert_eq!(spooler.used(), 30);
        b.write(&[0u8; 70]).await.unwrap();
        drop(b);
        assert_eq!(spooler.used(), 0);
    }
}
