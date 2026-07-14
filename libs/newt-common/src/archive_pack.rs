//! IO plumbing for the CreateArchive operation: a uniform chunk source over
//! sync/async VFS reads, a streaming byte sink over sync/async VFS writes,
//! and a thin dispatch over the sans-IO `newt-archive` writers.
//!
//! Everything streams — archive bytes are produced chunk-at-a-time by the
//! writers and flow straight into the destination VFS, so no temp files and
//! no whole-archive buffering, regardless of which side is remote.

use std::io;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::filesystem::{File, UserGroup};
use crate::operation::{ArchiveFormat, ArchiveOptions, bridge_sync_reader};
use crate::vfs::path::Path;
use crate::vfs::{VFS_READ_CHUNK_SIZE, Vfs, VfsAsyncWriter};

// --- Source: uniform chunk stream over sync/async reads ---

pub(crate) enum SourceReader {
    Async(Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    Bridged(tokio::sync::mpsc::Receiver<Result<Vec<u8>, crate::Error>>),
}

impl SourceReader {
    pub(crate) async fn open(
        vfs: &dyn Vfs,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<Self, crate::Error> {
        let descriptor = vfs.descriptor();
        if descriptor.can_read_async() {
            Ok(SourceReader::Async(vfs.open_read_async(path).await?))
        } else if descriptor.can_read_sync() {
            let reader = vfs.open_read_sync(path).await?;
            Ok(SourceReader::Bridged(bridge_sync_reader(
                reader,
                cancel.clone(),
            )))
        } else {
            Err(crate::Error::not_supported())
        }
    }

    /// Next chunk of source bytes; `None` at EOF.
    pub(crate) async fn next(&mut self) -> Result<Option<Vec<u8>>, crate::Error> {
        match self {
            SourceReader::Async(reader) => {
                let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
                let n = reader.read(&mut buf).await?;
                if n == 0 {
                    return Ok(None);
                }
                buf.truncate(n);
                Ok(Some(buf))
            }
            SourceReader::Bridged(rx) => rx.recv().await.transpose(),
        }
    }
}

// --- Sink: one streaming destination for the whole archive ---

/// Streaming destination for archive bytes. Async-capable destinations are
/// written directly; sync-only ones (the local FS) get a single blocking
/// writer task for the whole archive, fed through a bounded channel. The
/// archive encoder is one stateful stream spanning every entry and both
/// sync-only and async-only VFSes, which is why the bridge exists — hoisted
/// to per-operation, one task total (the copy engine pays one per file).
pub(crate) struct ArchiveSink(SinkState);

enum SinkState {
    Async(Box<dyn VfsAsyncWriter>),
    Sync {
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        pump: tokio::task::JoinHandle<Result<(), crate::Error>>,
    },
}

impl ArchiveSink {
    pub(crate) async fn open(vfs: &dyn Vfs, path: &Path) -> Result<Self, crate::Error> {
        let descriptor = vfs.descriptor();
        if descriptor.can_overwrite_async() {
            Ok(ArchiveSink(SinkState::Async(
                vfs.overwrite_async(path).await?,
            )))
        } else if descriptor.can_overwrite_sync() {
            let mut writer = vfs.overwrite_sync(path).await?;
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
            let pump = tokio::task::spawn_blocking(move || {
                while let Some(chunk) = rx.blocking_recv() {
                    io::Write::write_all(&mut *writer, &chunk)?;
                }
                io::Write::flush(&mut *writer)?;
                Ok(())
            });
            Ok(ArchiveSink(SinkState::Sync { tx, pump }))
        } else {
            Err(crate::Error::not_supported())
        }
    }

    /// A write failure poisons the sink — the caller must abort the
    /// operation (drop the sink and clean up the partial archive), never
    /// write again.
    pub(crate) async fn write_all(&mut self, chunk: Vec<u8>) -> Result<(), crate::Error> {
        if chunk.is_empty() {
            return Ok(());
        }
        match &mut self.0 {
            SinkState::Async(writer) => {
                writer.write(&chunk).await?;
                Ok(())
            }
            SinkState::Sync { tx, pump } => {
                if tx.send(chunk).await.is_err() {
                    // The writer task bailed — surface its real error.
                    return Err(match (&mut *pump).await {
                        Ok(Err(e)) => e,
                        Ok(Ok(())) => crate::Error::custom("archive writer closed unexpectedly"),
                        Err(join) => {
                            crate::Error::custom(format!("archive writer task failed: {join}"))
                        }
                    });
                }
                Ok(())
            }
        }
    }

    pub(crate) async fn finish(self) -> Result<(), crate::Error> {
        match self.0 {
            SinkState::Async(writer) => writer.finish().await,
            SinkState::Sync { tx, pump } => {
                drop(tx);
                pump.await
                    .map_err(|e| crate::Error::custom(format!("archive writer task failed: {e}")))?
            }
        }
    }
}

// --- Writer dispatch over the sans-IO tar/zip writers ---

pub(crate) enum ArchiveWriter {
    Tar(Box<newt_archive::TarWriter>),
    Zip(Box<newt_archive::ZipWriter>),
}

impl ArchiveWriter {
    pub(crate) fn new(options: &ArchiveOptions) -> Result<Self, crate::Error> {
        Ok(match options.format {
            ArchiveFormat::Zip => ArchiveWriter::Zip(Box::new(newt_archive::ZipWriter::new(
                options.level,
                options.password.as_deref(),
            ))),
            format => {
                let compression = match format {
                    ArchiveFormat::Tar => newt_archive::Compression::None,
                    ArchiveFormat::TarGz => newt_archive::Compression::Gzip,
                    ArchiveFormat::TarXz => newt_archive::Compression::Xz,
                    ArchiveFormat::TarZst => newt_archive::Compression::Zstd,
                    ArchiveFormat::Zip => unreachable!(),
                };
                ArchiveWriter::Tar(Box::new(newt_archive::TarWriter::new(
                    compression,
                    options.level,
                )?))
            }
        })
    }

    pub(crate) fn add_directory(
        &mut self,
        rel: &str,
        file: &File,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        match self {
            ArchiveWriter::Tar(w) => w.add_directory(rel, &entry_meta(file), out),
            ArchiveWriter::Zip(w) => w.add_directory(rel, &entry_meta(file), out),
        }
    }

    pub(crate) fn add_symlink(
        &mut self,
        rel: &str,
        target: &str,
        file: &File,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        match self {
            ArchiveWriter::Tar(w) => w.add_symlink(rel, target, &entry_meta(file), out),
            ArchiveWriter::Zip(w) => w.add_symlink(rel, target, &entry_meta(file), out),
        }
    }

    pub(crate) fn begin_file(
        &mut self,
        rel: &str,
        size: Option<u64>,
        file: &File,
        out: &mut Vec<u8>,
    ) -> io::Result<()> {
        match self {
            // Tar headers precede data, so the size is a hard commitment.
            ArchiveWriter::Tar(w) => w.begin_file(rel, size.unwrap_or(0), &entry_meta(file), out),
            ArchiveWriter::Zip(w) => w.begin_file(rel, size, &entry_meta(file), out),
        }
    }

    /// Returns the number of bytes accepted; less than `chunk.len()` when a
    /// tar entry reached its declared size (the source file grew).
    pub(crate) fn write_data(&mut self, chunk: &[u8], out: &mut Vec<u8>) -> io::Result<usize> {
        match self {
            ArchiveWriter::Tar(w) => w.write_data(chunk, out),
            ArchiveWriter::Zip(w) => {
                w.write_data(chunk, out)?;
                Ok(chunk.len())
            }
        }
    }

    /// Returns the shortfall zero-padded into a tar entry (the source file
    /// shrank); always 0 for zip, which records actual sizes after the fact.
    pub(crate) fn end_file(&mut self, out: &mut Vec<u8>) -> io::Result<u64> {
        match self {
            ArchiveWriter::Tar(w) => w.end_file(out),
            ArchiveWriter::Zip(w) => {
                w.end_file(out)?;
                Ok(0)
            }
        }
    }

    pub(crate) fn finish(self, out: &mut Vec<u8>) -> io::Result<()> {
        match self {
            ArchiveWriter::Tar(w) => w.finish(out),
            ArchiveWriter::Zip(w) => w.finish(out),
        }
    }
}

fn entry_meta(file: &File) -> newt_archive::EntryMeta {
    let (uid, uname) = match &file.user {
        Some(UserGroup::Id(id)) => (Some(*id as u64), None),
        Some(UserGroup::Name(name)) => (None, Some(name.clone())),
        None => (None, None),
    };
    let (gid, gname) = match &file.group {
        Some(UserGroup::Id(id)) => (Some(*id as u64), None),
        Some(UserGroup::Name(name)) => (None, Some(name.clone())),
        None => (None, None),
    };
    newt_archive::EntryMeta {
        mode: file.mode.as_ref().map(|m| m.0),
        uid,
        gid,
        uname,
        gname,
        mtime_ms: file.modified,
    }
}
