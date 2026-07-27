use super::*;

use std::io;

use tokio::io::AsyncReadExt;

use crate::vfs::{UserGroup, VfsAsyncWriter};

// --- Execute CreateArchive (async pack loop, streams via archive_pack) ---

pub(super) async fn execute_create_archive(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    options: ArchiveOptions,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    // Follow any redirect_target hooks (e.g. flat search results), same as copy.
    let mut sources = sources;
    for s in sources.iter_mut() {
        *s = context.registry.dereference(s).await;
    }
    let first_source = sources
        .first()
        .ok_or_else(|| crate::Error::custom("no sources provided"))?;
    let (src_vfs, _) = context.registry.resolve(first_source)?;
    let (dst_vfs, dst_path) = context.registry.resolve(&destination)?;

    let src_vfs_id = first_source.vfs_id;
    if let Some(mismatched) = sources.iter().find(|s| s.vfs_id != src_vfs_id) {
        return Err(crate::Error::custom(format!(
            "all sources must be on the same VFS (expected {}, got {})",
            src_vfs_id, mismatched.vfs_id
        )));
    }
    if options.password.is_some() && options.format != ArchiveFormat::Zip {
        return Err(crate::Error::custom(
            "password protection is only supported for zip archives",
        ));
    }

    let src_descriptor = src_vfs.descriptor();
    debug!(
        "execute_create_archive: {} sources, src_vfs={} ({}), dst={} on vfs {} ({})",
        sources.len(),
        src_vfs_id,
        src_descriptor.type_name(),
        dst_path,
        destination.vfs_id,
        dst_vfs.descriptor().type_name(),
    );

    // Destination conflict before any work. The archive is a single artifact,
    // so declining to overwrite simply cancels the operation.
    if let Ok(existing) = dst_vfs.file_info(&dst_path).await {
        if existing.is_dir {
            return Err(crate::Error::custom(format!(
                "destination is a directory: {}",
                dst_path
            )));
        }
        match reporter
            .raise_issue(
                IssueKind::AlreadyExists,
                format!("File already exists: {}", dst_path),
                None,
                vec![IssueAction::Skip, IssueAction::Overwrite],
            )
            .await?
        {
            IssueAction::Overwrite => {}
            _ => {
                // Skipping the only artifact means nothing to do — surface
                // as a cancellation, not a failure.
                cancel.cancel();
                return Err(crate::Error::cancelled());
            }
        }
    }

    let source_paths: Vec<PathBuf> = sources.iter().map(|s| s.path.clone()).collect();
    let walk_options = WalkOptions {
        follow_symlinks: !options.preserve_symlinks,
        // Keep a same-VFS destination out of the walk, or the archive
        // would pack its growing self.
        exclude: (src_vfs_id == destination.vfs_id).then(|| dst_path.to_owned()),
    };
    let (walked, total_bytes) = walk_sources(
        &*src_vfs,
        src_descriptor,
        &source_paths,
        &walk_options,
        reporter,
        &cancel,
    )
    .await?;

    // Duplicate top-level names would silently collide inside the archive.
    let mut top_level = std::collections::HashSet::new();
    for entry in &walked {
        if !entry.rel.contains('/') && !top_level.insert(entry.rel.as_str()) {
            return Err(crate::Error::custom(format!(
                "duplicate top-level name in selection: {}",
                entry.rel
            )));
        }
    }

    reporter.send_prepared(total_bytes, walked.len() as u64);

    let writer = ArchiveWriter::new(&options)?;
    let mut sink = ArchiveSink::open(&*dst_vfs, &dst_path).await?;

    let result = match pack_entries(reporter, &*src_vfs, writer, &mut sink, &walked, &cancel).await
    {
        Ok(()) => sink.finish().await,
        Err(e) => {
            let _ = sink.abort().await;
            Err(e)
        }
    };
    if let Err(e) = result {
        // Append-only stream — a failed or cancelled archive can't be
        // salvaged; best-effort cleanup of the partial artifact.
        let _ = dst_vfs.remove_file(&dst_path).await;
        return Err(e);
    }
    Ok(())
}

async fn pack_entries(
    reporter: &mut ProgressReporter,
    src_vfs: &dyn Vfs,
    mut writer: ArchiveWriter,
    sink: &mut ArchiveSink,
    entries: &[WalkedEntry],
    cancel: &CancellationToken,
) -> Result<(), crate::Error> {
    let mut buf = Vec::new();
    let mut bytes_done = 0u64;
    let mut items_done = 0u64;

    for entry in entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }
        reporter.maybe_send_progress(bytes_done, items_done, &entry.rel);

        match &entry.kind {
            WalkedKind::Directory => {
                writer.add_directory(&entry.rel, &entry.file, &mut buf)?;
                cancellable(cancel, sink.write_all(std::mem::take(&mut buf))).await?;
            }
            WalkedKind::Symlink { target } => {
                writer.add_symlink(&entry.rel, target, &entry.file, &mut buf)?;
                cancellable(cancel, sink.write_all(std::mem::take(&mut buf))).await?;
            }
            WalkedKind::File => {
                let scanned_size = entry.file.size;
                let entry_start = bytes_done;

                // Open the source before the entry header is committed to the
                // stream — an open failure can still Skip/Retry cleanly.
                let reader = loop {
                    match SourceReader::open(src_vfs, &entry.source).await {
                        Ok(reader) => break Some(reader),
                        Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                        Err(e) => {
                            match reporter
                                .handle_io_error(
                                    e,
                                    "Error",
                                    Some(entry.source.as_wire_str().to_string()),
                                    cancel,
                                    true,
                                )
                                .await?
                            {
                                IssueOutcome::Skip => break None,
                                IssueOutcome::Retry => continue,
                            }
                        }
                    }
                };
                let Some(mut reader) = reader else {
                    bytes_done = entry_start + scanned_size.unwrap_or(0);
                    items_done += 1;
                    continue;
                };

                writer.begin_file(&entry.rel, scanned_size, &entry.file, &mut buf)?;
                cancellable(cancel, sink.write_all(std::mem::take(&mut buf))).await?;

                let mut read_error = None;
                loop {
                    if cancel.is_cancelled() {
                        return Err(crate::Error::cancelled());
                    }
                    match cancellable(cancel, reader.next()).await {
                        Ok(Some(chunk)) => {
                            let accepted = writer.write_data(&chunk, &mut buf)?;
                            cancellable(cancel, sink.write_all(std::mem::take(&mut buf))).await?;
                            bytes_done += chunk.len() as u64;
                            reporter.maybe_send_progress(bytes_done, items_done, &entry.rel);
                            if accepted < chunk.len() {
                                warn!(
                                    "archive entry {} truncated: source grew past its scanned size",
                                    entry.rel
                                );
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) if e.kind == crate::ErrorKind::Cancelled => return Err(e),
                        Err(e) => {
                            read_error = Some(e);
                            break;
                        }
                    }
                }

                let padded = writer.end_file(&mut buf)?;
                cancellable(cancel, sink.write_all(std::mem::take(&mut buf))).await?;
                if padded > 0 && read_error.is_none() {
                    warn!(
                        "archive entry {} zero-padded: source shrank below its scanned size",
                        entry.rel
                    );
                }
                if let Some(e) = read_error {
                    // The entry header is already on the append-only stream;
                    // the entry was finalized as truncated/padded, so a retry
                    // is structurally impossible — only Skip is offered.
                    reporter
                        .handle_io_error(
                            e,
                            &format!(
                                "Error reading {} (stored truncated in the archive)",
                                entry.rel
                            ),
                            Some(entry.source.as_wire_str().to_string()),
                            cancel,
                            false,
                        )
                        .await?;
                }
                // Snap to the scanned contribution so skips/shrinks still
                // drive the bar to 100% (mirrors execute_copy's accounting).
                bytes_done = bytes_done.max(entry_start + scanned_size.unwrap_or(0));
            }
        }
        items_done += 1;
    }

    writer.finish(&mut buf)?;
    cancellable(cancel, sink.write_all(std::mem::take(&mut buf))).await?;
    reporter.maybe_send_progress(bytes_done, items_done, "");
    Ok(())
}

// ---------------------------------------------------------------------------
// IO plumbing: a chunk source over VFS reads, a streaming byte sink over
// VFS writes, and a thin dispatch over the sans-IO `newt-archive` writers.
// Everything streams — archive bytes are produced chunk-at-a-time by the
// writers and flow straight into the destination VFS, so no temp files and
// no whole-archive buffering, regardless of which side is remote.
// ---------------------------------------------------------------------------

// --- Source: chunk stream over a VFS read ---

struct SourceReader(Box<dyn tokio::io::AsyncRead + Send + Unpin>);

impl SourceReader {
    async fn open(vfs: &dyn Vfs, path: &Path) -> Result<Self, crate::Error> {
        Ok(SourceReader(vfs.open_read_async(path).await?))
    }

    /// Next chunk of source bytes; `None` at EOF.
    async fn next(&mut self) -> Result<Option<Vec<u8>>, crate::Error> {
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

struct ArchiveSink(Box<dyn VfsAsyncWriter>);

impl ArchiveSink {
    async fn open(vfs: &dyn Vfs, path: &Path) -> Result<Self, crate::Error> {
        Ok(ArchiveSink(vfs.overwrite_async(path).await?))
    }

    /// A write failure poisons the sink — the caller must abort the
    /// operation (drop the sink and clean up the partial archive), never
    /// write again.
    async fn write_all(&mut self, chunk: Vec<u8>) -> Result<(), crate::Error> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.0.write(&chunk).await?;
        Ok(())
    }

    async fn finish(self) -> Result<(), crate::Error> {
        self.0.finish().await
    }

    /// Stop producing archive bytes and discard the writer without
    /// committing it (an S3 multipart writer aborts its upload on drop). A
    /// local writer with a write in flight may release its file handle a
    /// beat after this returns — partial-file cleanup is best-effort.
    async fn abort(self) -> Result<(), crate::Error> {
        drop(self.0);
        Ok(())
    }
}

// --- Writer dispatch over the sans-IO tar/zip writers ---

enum ArchiveWriter {
    Tar(Box<newt_archive::TarWriter>),
    Zip(Box<newt_archive::ZipWriter>),
}

impl ArchiveWriter {
    fn new(options: &ArchiveOptions) -> Result<Self, crate::Error> {
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

    fn add_directory(&mut self, rel: &str, file: &File, out: &mut Vec<u8>) -> io::Result<()> {
        match self {
            ArchiveWriter::Tar(w) => w.add_directory(rel, &entry_meta(file), out),
            ArchiveWriter::Zip(w) => w.add_directory(rel, &entry_meta(file), out),
        }
    }

    fn add_symlink(
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

    fn begin_file(
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
    fn write_data(&mut self, chunk: &[u8], out: &mut Vec<u8>) -> io::Result<usize> {
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
    fn end_file(&mut self, out: &mut Vec<u8>) -> io::Result<u64> {
        match self {
            ArchiveWriter::Tar(w) => w.end_file(out),
            ArchiveWriter::Zip(w) => {
                w.end_file(out)?;
                Ok(0)
            }
        }
    }

    fn finish(self, out: &mut Vec<u8>) -> io::Result<()> {
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
