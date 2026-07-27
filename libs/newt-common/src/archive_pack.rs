//! IO plumbing for the CreateArchive operation: a chunk source over VFS
//! reads, a streaming byte sink over VFS writes, and a thin dispatch over
//! the sans-IO `newt-archive` writers.
//!
//! Everything streams — archive bytes are produced chunk-at-a-time by the
//! writers and flow straight into the destination VFS, so no temp files and
//! no whole-archive buffering, regardless of which side is remote.

use std::io;

use tokio::io::AsyncReadExt;

use crate::operation::{ArchiveFormat, ArchiveOptions};
use crate::vfs::path::Path;
use crate::vfs::{File, UserGroup};
use crate::vfs::{VFS_READ_CHUNK_SIZE, Vfs, VfsAsyncWriter};

// --- Source: chunk stream over a VFS read ---

pub(crate) struct SourceReader(Box<dyn tokio::io::AsyncRead + Send + Unpin>);

impl SourceReader {
    pub(crate) async fn open(vfs: &dyn Vfs, path: &Path) -> Result<Self, crate::Error> {
        Ok(SourceReader(vfs.open_read_async(path).await?))
    }

    /// Next chunk of source bytes; `None` at EOF.
    pub(crate) async fn next(&mut self) -> Result<Option<Vec<u8>>, crate::Error> {
        let mut buf = vec![0u8; VFS_READ_CHUNK_SIZE];
        let n = self.0.read(&mut buf).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.truncate(n);
        Ok(Some(buf))
    }
}

// --- Sink: one streaming destination for the whole archive ---

pub(crate) struct ArchiveSink(Box<dyn VfsAsyncWriter>);

impl ArchiveSink {
    pub(crate) async fn open(vfs: &dyn Vfs, path: &Path) -> Result<Self, crate::Error> {
        Ok(ArchiveSink(vfs.overwrite_async(path).await?))
    }

    /// A write failure poisons the sink — the caller must abort the
    /// operation (drop the sink and clean up the partial archive), never
    /// write again.
    pub(crate) async fn write_all(&mut self, chunk: Vec<u8>) -> Result<(), crate::Error> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.0.write(&chunk).await?;
        Ok(())
    }

    pub(crate) async fn finish(self) -> Result<(), crate::Error> {
        self.0.finish().await
    }

    /// Stop producing archive bytes and discard the writer without
    /// committing it (an S3 multipart writer aborts its upload on drop). A
    /// local writer with a write in flight may release its file handle a
    /// beat after this returns — partial-file cleanup is best-effort.
    pub(crate) async fn abort(self) -> Result<(), crate::Error> {
        drop(self.0);
        Ok(())
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
