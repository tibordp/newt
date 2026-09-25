use super::preserve::{SourceAttributes, resolve_preservation};
use super::*;

// --- Copy Entry ---

#[derive(Debug)]
pub(super) enum CopyEntryKind {
    File,
    Directory,
    Symlink { target: String },
}

pub(super) struct CopyEntry {
    source: PathBuf,
    dest: PathBuf,
    kind: CopyEntryKind,
    file: File,
    metadata: Option<crate::vfs::VfsMetadata>,
    #[allow(dead_code)]
    size_bytes: u64,
    mount_point: bool,
}

pub(super) struct CopyPlan {
    entries: Vec<CopyEntry>,
    total_bytes: u64,
}

// --- Plan copy (async, uses Vfs) ---

#[allow(clippy::too_many_arguments)]
pub(super) async fn plan_copy(
    src_vfs: &dyn Vfs,
    sources: &[PathBuf],
    destination: &Path,
    rename_to: Option<&str>,
    walk: &WalkOptions,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<CopyPlan, crate::Error> {
    let (walked, total_bytes) = walk_sources(src_vfs, sources, walk, reporter, cancel).await?;

    let mut entries = walked
        .into_iter()
        .map(|w| {
            // `rel` leads with the top-level source's file name; a rename
            // lands under a different leaf name in the destination.
            let rel = match rename_to {
                Some(new_name) => match w.rel.split_once('/') {
                    Some((_, rest)) => format!("{}/{}", new_name, rest),
                    None => new_name.to_string(),
                },
                None => w.rel,
            };
            CopyEntry {
                dest: destination.join(&rel),
                size_bytes: match w.kind {
                    WalkedKind::File => w.file.size.unwrap_or(0),
                    _ => 0,
                },
                kind: match w.kind {
                    WalkedKind::File => CopyEntryKind::File,
                    WalkedKind::Directory => CopyEntryKind::Directory,
                    WalkedKind::Symlink { target } => CopyEntryKind::Symlink { target },
                },
                source: w.source,
                file: w.file,
                metadata: w.metadata,
                mount_point: w.mount_point,
            }
        })
        .collect::<Vec<_>>();

    // An archive decodes cheapest in its own order, which rarely matches
    // the walk. Directories keep walk order (parents before children;
    // the reverse passes rely on it) and go first; files and links follow
    // in the source's order.
    let files = entries
        .iter()
        .filter(|e| !matches!(e.kind, CopyEntryKind::Directory))
        .map(|e| e.source.clone())
        .collect::<Vec<_>>();
    if files.len() > 1 {
        match src_vfs.read_order(&files).await {
            Ok(Some(order)) => {
                let (dirs, files): (Vec<_>, Vec<_>) = entries
                    .into_iter()
                    .partition(|e| matches!(e.kind, CopyEntryKind::Directory));
                let mut keyed = order.into_iter().zip(files).collect::<Vec<_>>();
                keyed.sort_by_key(|(key, _)| *key);
                entries = dirs
                    .into_iter()
                    .chain(keyed.into_iter().map(|(_, e)| e))
                    .collect();
            }
            Ok(None) => {}
            Err(e) => debug!("plan_copy: read_order unavailable: {}", e),
        }
    }

    debug!(
        "plan_copy: {} entries, {} total bytes",
        entries.len(),
        total_bytes
    );

    Ok(CopyPlan {
        entries,
        total_bytes,
    })
}

// --- Chunked byte copy ---

#[allow(clippy::too_many_arguments)]
pub(super) async fn copy_bytes_async(
    reader: &mut (dyn tokio::io::AsyncRead + Send + Unpin),
    writer: &mut dyn crate::vfs::VfsAsyncWriter,
    cancel: &CancellationToken,
    reporter: &mut ProgressReporter,
    bytes_done: &mut u64,
    items_done: u64,
    display: &str,
) -> Result<(), crate::Error> {
    use tokio::io::AsyncReadExt;

    let mut buf = [0u8; VFS_READ_CHUNK_SIZE];

    loop {
        let n = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(crate::Error::cancelled()),
            result = reader.read(&mut buf) => result?,
        };
        if n == 0 {
            break;
        }
        let mut offset = 0;
        while offset < n {
            let written = cancellable(cancel, writer.write(&buf[offset..n])).await?;
            if written == 0 {
                return Err(crate::Error::custom("copy write made no progress"));
            }
            offset += written;
        }
        *bytes_done += n as u64;
        reporter.maybe_send_progress(*bytes_done, items_done, display);
    }

    Ok(())
}

// --- Copy a single file through VFS, with strategy cascade ---

/// How a file copy went, short of an error.
pub(super) enum Written {
    Done,
    /// `create_new` was refused: something is at the destination.
    Refused,
    /// The destination cannot refuse this write; look before writing.
    CannotRefuse,
}

/// Copy one file. With `create_new` the destination refuses a write over
/// anything already there, and a write that fails leaves nothing, so a
/// retry is as exclusive as the first attempt.
#[allow(clippy::too_many_arguments)]
pub(super) async fn copy_single_file(
    src_vfs: &dyn Vfs,
    dst_vfs: &dyn Vfs,
    entry: &CopyEntry,
    same_vfs: bool,
    cancel: &CancellationToken,
    reporter: &mut ProgressReporter,
    bytes_done: &mut u64,
    items_done: u64,
    write_options: &mut crate::vfs::attributes::WriteOptions,
    display: &str,
    create_new: bool,
) -> Result<Written, crate::Error> {
    let src_descriptor = src_vfs.descriptor();
    let dst_descriptor = dst_vfs.descriptor();
    write_options.create_new = create_new;
    write_options.size_hint = Some(entry.size_bytes);

    // 1. Same-VFS copy_within fast path
    if same_vfs && dst_descriptor.can_copy_within() {
        debug!("copy_single_file: trying copy_within for {}", entry.source);
        match cancellable(
            cancel,
            src_vfs.copy_within(&entry.source, &entry.dest, write_options),
        )
        .await
        {
            Ok(()) => {
                *bytes_done += entry.size_bytes;
                return Ok(Written::Done);
            }
            Err(e) if create_new && e.kind == crate::ErrorKind::AlreadyExists => {
                return Ok(Written::Refused);
            }
            // Refusing may be the one thing it cannot do: the caller looks
            // and comes back without `create_new`, before a server-side
            // copy is given up for streaming.
            Err(e) if create_new && e.kind == crate::ErrorKind::NotSupported => {
                return Ok(Written::CannotRefuse);
            }
            // The descriptor can't see per-call quirks (a RootVfs spans
            // many real filesystems; server-side copies have size caps),
            // so "unsupported" is only known at call time — fall through
            // to the streaming strategies. Real failures surface as
            // issues instead of silently downgrading to a full re-stream.
            Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                debug!(
                    "copy_single_file: copy_within unsupported for {}: {}",
                    entry.source, e
                );
            }
            Err(e) => return Err(e),
        }
    }

    // 2. Streaming copy
    if src_descriptor.can_read() && dst_descriptor.can_overwrite() {
        debug!("copy_single_file: streaming copy for {}", entry.source);
        // Under `create_new` the destination answers first, so a refusal
        // costs no source open (and holds none open across a prompt); an
        // exclusive create abandoned for an unreadable source is removed by
        // its writer. Otherwise the source opens first, so one that cannot
        // be read never truncates what is there.
        let mut reader = if create_new {
            None
        } else {
            Some(cancellable(cancel, src_vfs.open_read_async(&entry.source)).await?)
        };
        let mut writer = loop {
            match cancellable(cancel, dst_vfs.overwrite_async(&entry.dest, write_options)).await {
                Ok(writer) => break writer,
                Err(e) if create_new && e.kind == crate::ErrorKind::AlreadyExists => {
                    return Ok(Written::Refused);
                }
                // Asked before the other options, whose own `NotSupported`
                // the caller meets once it comes back without this one.
                Err(e) if create_new && e.kind == crate::ErrorKind::NotSupported => {
                    return Ok(Written::CannotRefuse);
                }
                Err(e)
                    if e.kind == crate::ErrorKind::NotSupported && !write_options.is_default() =>
                {
                    let kind = if write_options.sparse {
                        AttributeKind::Sparse
                    } else if write_options.object_canned_acl.is_some() {
                        AttributeKind::ObjectAccess
                    } else {
                        AttributeKind::ObjectMetadata
                    };
                    if !resolve_preservation(reporter, kind, &entry.dest, e).await? {
                        if write_options.sparse {
                            write_options.sparse = false;
                        } else if write_options.object_canned_acl.is_some() {
                            write_options.object_canned_acl = None;
                        } else {
                            write_options.object_metadata = None;
                            write_options.object_storage_class = None;
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        };
        let mut reader = match reader.take() {
            Some(reader) => reader,
            None => cancellable(cancel, src_vfs.open_read_async(&entry.source)).await?,
        };

        let written = match copy_bytes_async(
            &mut *reader,
            &mut *writer,
            cancel,
            reporter,
            bytes_done,
            items_done,
            display,
        )
        .await
        {
            Ok(()) => cancellable(cancel, writer.finish()).await,
            Err(e) => Err(e),
        };
        return match written {
            Ok(()) => Ok(Written::Done),
            // A destination that refuses on finish, after every byte (S3).
            Err(e) if create_new && e.kind == crate::ErrorKind::AlreadyExists => {
                Ok(Written::Refused)
            }
            Err(e) => Err(e),
        };
    }

    Err(crate::Error::not_supported())
}

/// What is at an entry's destination, resolved against the copy.
enum Destination {
    /// Nothing: write it.
    Absent,
    /// A directory onto a directory.
    Merge,
    /// Something the user chose to write over, cleared of what a write
    /// cannot replace.
    Replace,
    Skip,
    /// The source itself, by another name (a hard link) or another path (a
    /// symlinked directory above it): already copied. Writing over it would
    /// truncate the source before it is read.
    Same,
}

/// Look at an entry's destination and ask about whatever is there.
async fn check_destination(
    dst_vfs: &dyn Vfs,
    same_vfs: bool,
    entry: &CopyEntry,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<Destination, crate::Error> {
    let existing = loop {
        match cancellable(cancel, dst_vfs.file_info(&entry.dest)).await {
            Ok(file) => break file,
            Err(e) if e.kind == crate::ErrorKind::NotFound => return Ok(Destination::Absent),
            // A failed look leaves the destination unknown, not empty.
            Err(e) => {
                match reporter
                    .handle_io_error(
                        e,
                        &format!("Cannot check {}", entry.dest),
                        None,
                        cancel,
                        true,
                    )
                    .await?
                {
                    IssueOutcome::Skip => return Ok(Destination::Skip),
                    IssueOutcome::Retry => {}
                }
            }
        }
    };
    if same_vfs && !matches!(entry.kind, CopyEntryKind::Directory) {
        match same_file_or_skip(dst_vfs, &entry.source, &entry.dest, reporter, cancel).await? {
            None => return Ok(Destination::Skip),
            Some(true) => return Ok(Destination::Same),
            Some(false) => {}
        }
    }
    // A link to a directory is a directory to a directory merged into it
    // (through the link, as `cp -R` does); to anything else it is a link,
    // replaced by unlinking it.
    let merging = matches!(entry.kind, CopyEntryKind::Directory);
    let directory = existing.is_dir && (merging || !existing.is_symlink);
    let mismatch = match (&entry.kind, directory) {
        (CopyEntryKind::Directory, true) => return Ok(Destination::Merge),
        (CopyEntryKind::Directory, false) => Some(format!(
            "Cannot replace file with directory: {}",
            entry.dest
        )),
        (_, true) => Some(format!(
            "Cannot replace directory with file: {}",
            entry.dest
        )),
        _ => None,
    };
    if let Some(message) = mismatch {
        return match reporter
            .raise_issue(
                IssueKind::AlreadyExists,
                message,
                None,
                vec![IssueAction::Skip],
            )
            .await?
        {
            IssueAction::Skip => Ok(Destination::Skip),
            _ => unreachable!("not offered"),
        };
    }
    if !reporter
        .replace_existing(&entry.dest, &entry.file, &existing)
        .await?
    {
        return Ok(Destination::Skip);
    }
    // A write goes through a symlink to its target, and a symlink cannot
    // be created over anything: clear the way.
    if existing.is_symlink || matches!(entry.kind, CopyEntryKind::Symlink { .. }) {
        cancellable(cancel, dst_vfs.remove_file(&entry.dest)).await?;
    }
    Ok(Destination::Replace)
}

// --- Execute Copy (async outer loop, uses Vfs) ---

/// Fail the whole operation when a source's destination *is* that source.
///
/// Not a per-item conflict: there is no sane resolution. Overwrite would
/// hand the copy machinery one file as both ends — the destination is
/// opened truncating while the source read is still pending, which empties
/// it. So this is refused up front, before any scanning, like `cp`'s
/// "'x' and 'x' are the same file".
///
/// Byte comparison can't answer it — `/a/Foo` and `/a/foo` are one file on
/// a case-insensitive volume, as are NFC and NFD spellings on HFS+ — so
/// the filesystem is asked ([`Vfs::same_file`]).
///
/// Copy refuses every spelling. Move refuses only the true no-op — same
/// file *and* byte-identical leaf; a differing leaf (`Foo` → `foo`) is a
/// re-spelling, which the rename fast path performs.
///
/// Costs one filesystem question per distinct source directory — one, for
/// an ordinary pane selection — since a source can only land on itself
/// when its own directory is the destination.
pub(super) async fn reject_self_destination(
    context: &OperationContext,
    sources: &[VfsPath],
    destination: &VfsPath,
    rename_to: Option<&str>,
    is_move: bool,
) -> Result<(), crate::Error> {
    let (dst_vfs, dst_dir) = context.registry.resolve(destination)?;
    let mut parent_is_destination: Vec<(PathBuf, bool)> = Vec::new();

    for source in sources {
        // Distinct VFSes are distinct storage as far as we can tell; two
        // mounts of one bucket are the same bytes but `same_file` is a
        // per-`Vfs` verb and can't see across.
        if source.vfs_id != destination.vfs_id {
            continue;
        }
        let (Some(parent), Some(leaf)) = (source.parent(), source.file_name()) else {
            continue;
        };
        let final_leaf = rename_to.unwrap_or(leaf);

        let is_destination = match parent_is_destination
            .iter()
            .find(|(seen, _)| *seen == parent.path)
        {
            Some((_, answer)) => *answer,
            None => {
                let answer = dst_vfs.same_file(&parent.path, &dst_dir).await?;
                parent_is_destination.push((parent.path.clone(), answer));
                answer
            }
        };
        if !is_destination {
            continue;
        }

        // Same directory: an identical leaf is unambiguously the same file;
        // a differing one still can be, so ask.
        let onto_itself = final_leaf == leaf
            || dst_vfs
                .same_file(&source.path, &dst_dir.join(final_leaf))
                .await?;
        if !onto_itself || (is_move && final_leaf != leaf) {
            continue;
        }

        return Err(crate::Error::custom(format!(
            "Cannot {} \"{}\" onto itself",
            if is_move { "move" } else { "copy" },
            leaf
        )));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_copy(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    options: CopyOptions,
    cancel: CancellationToken,
    is_move: bool,
    items_done_offset: u64,
    rename_to: Option<&str>,
) -> Result<(), crate::Error> {
    debug_assert!(
        rename_to.is_none() || sources.len() == 1,
        "rename_to requires exactly one source"
    );
    // Follow any redirect_target hooks (e.g. flat search results) so the
    // copy operates on the underlying real files, not on the synthetic
    // SearchVfs paths the user clicked.
    let mut sources = sources;
    for s in sources.iter_mut() {
        *s = context.registry.dereference(s).await;
    }

    reject_self_destination(context, &sources, &destination, rename_to, is_move).await?;

    let first_source = sources
        .first()
        .ok_or_else(|| crate::Error::custom("no sources provided"))?;
    let (src_vfs, _) = context.registry.resolve(first_source)?;
    let (dst_vfs, dst_path) = context.registry.resolve(&destination)?;

    let src_vfs_id = first_source.vfs_id;
    let dst_vfs_id = destination.vfs_id;
    let same_vfs = src_vfs_id == dst_vfs_id;

    if let Some(mismatched) = sources.iter().find(|s| s.vfs_id != src_vfs_id) {
        return Err(crate::Error::custom(format!(
            "all sources must be on the same VFS (expected {}, got {})",
            src_vfs_id, mismatched.vfs_id
        )));
    }

    let src_descriptor = src_vfs.descriptor();
    let dst_descriptor = dst_vfs.descriptor();

    debug!(
        "execute_copy: {} sources, src_vfs={} ({}), dst_vfs={} ({}), same_vfs={}",
        sources.len(),
        src_vfs_id,
        src_descriptor.type_name(),
        dst_vfs_id,
        dst_descriptor.type_name(),
        same_vfs
    );

    let source_paths: Vec<PathBuf> = sources.iter().map(|s| s.path.clone()).collect();

    if options.create_symlink {
        if !dst_descriptor.can_create_symlink() {
            return Err(crate::Error::custom(
                "Destination does not support symlink creation".to_string(),
            ));
        }
        if source_paths.len() != 1 {
            return Err(crate::Error::custom(
                "Symlink creation only supported for single file".to_string(),
            ));
        }
        let source = &source_paths[0];
        let file_name = match sources[0].file_name() {
            Some(f) => f,
            None => return Err(crate::Error::custom("source has no file name".to_string())),
        };
        let dest = dst_path.join(rename_to.unwrap_or(file_name));
        reporter.send_prepared(0, 1);
        dst_vfs.create_symlink(&dest, source.as_wire_str()).await?;
        return Ok(());
    }

    let plan = plan_copy(
        &*src_vfs,
        &source_paths,
        &dst_path,
        rename_to,
        &WalkOptions {
            follow_symlinks: options.follow_symlinks,
            capture_directory_metadata: dst_descriptor.can_set_metadata()
                && (options.preserve_permissions
                    || options.preserve_timestamps
                    || options.preserve_owner
                    || options.preserve_group),
            one_file_system: options.one_file_system,
            ..Default::default()
        },
        reporter,
        &cancel,
    )
    .await?;

    let total_items = plan.entries.len() as u64 + items_done_offset;
    reporter.send_prepared(plan.total_bytes, total_items);

    let mut directories = Vec::new();
    let mut hard_links = HashMap::<Vec<u8>, PathBuf>::new();
    let mut bytes_done = 0u64;
    let mut items_done = items_done_offset;

    for entry in &plan.entries {
        if cancel.is_cancelled() {
            return Err(crate::Error::cancelled());
        }

        let display = entry
            .dest
            .strip_prefix(&dst_path)
            .map(str::to_string)
            .unwrap_or_else(|| entry.dest.as_wire_str().to_string());
        reporter.maybe_send_progress(bytes_done, items_done, &display);

        // A file is written with `create_new` and hears about a conflict
        // from the refusal; everything else looks first. A directory on a
        // VFS whose directories are key prefixes (S3) has nothing to find.
        let is_file = matches!(entry.kind, CopyEntryKind::File);
        let mut merged_directory = false;
        if !is_file {
            let destination = if matches!(entry.kind, CopyEntryKind::Directory)
                && !dst_descriptor.can_stat_directories()
            {
                Destination::Absent
            } else {
                check_destination(&*dst_vfs, same_vfs, entry, reporter, &cancel).await?
            };
            match destination {
                Destination::Skip | Destination::Same => {
                    bytes_done += entry.size_bytes;
                    items_done += 1;
                    continue;
                }
                Destination::Merge => {
                    merged_directory = true;
                    if !options.preserve_merged_directories {
                        items_done += 1;
                        continue;
                    }
                }
                Destination::Absent | Destination::Replace => {}
            }
        }

        let mut attributes = SourceAttributes::read(
            &*src_vfs,
            &*dst_vfs,
            &entry.source,
            &entry.file,
            entry.metadata.clone(),
            &options,
            reporter,
            &cancel,
        )
        .await?;
        let identity = attributes.identity.clone();
        hard_links.retain(|_, target| target != &entry.dest);
        // A file goes only where nothing is until its destination has been
        // looked at.
        let mut create_new = is_file;
        let mut linked = false;
        if let Some(target) = identity.as_ref().and_then(|id| hard_links.get(id)).cloned() {
            match check_destination(&*dst_vfs, same_vfs, entry, reporter, &cancel).await? {
                Destination::Skip | Destination::Same => {
                    bytes_done += entry.size_bytes;
                    items_done += 1;
                    continue;
                }
                Destination::Absent | Destination::Merge | Destination::Replace => {}
            }
            create_new = false;
            loop {
                // link(2) refuses an existing name; the user already chose
                // to replace it.
                let result = async {
                    match dst_vfs.hard_link(&entry.dest, &target).await {
                        Err(e) if e.kind == crate::ErrorKind::AlreadyExists => {
                            dst_vfs.remove_file(&entry.dest).await?;
                            dst_vfs.hard_link(&entry.dest, &target).await
                        }
                        result => result,
                    }
                };
                match cancellable(&cancel, result).await {
                    Ok(()) => {
                        linked = true;
                        break;
                    }
                    Err(e) => {
                        if !resolve_preservation(reporter, AttributeKind::HardLinks, &entry.dest, e)
                            .await?
                        {
                            break;
                        }
                    }
                }
            }
        }

        // Perform the operation
        let bytes_before = bytes_done;
        let mut succeeded = false;
        // A refusal the destination's own stat then contradicts gets one
        // more attempt before it is an issue.
        let mut contradicted = false;
        loop {
            bytes_done = bytes_before; // Reset progress on retry to avoid double-counting

            let result = match &entry.kind {
                CopyEntryKind::Directory if merged_directory => Ok(Written::Done),
                CopyEntryKind::Directory => {
                    cancellable(&cancel, dst_vfs.create_directory(&entry.dest))
                        .await
                        .map(|()| Written::Done)
                }
                CopyEntryKind::Symlink { target } => {
                    if dst_descriptor.can_create_symlink() {
                        cancellable(
                            &cancel,
                            dst_vfs.create_symlink(&entry.dest, target.as_str()),
                        )
                        .await
                        .map(|()| Written::Done)
                    } else {
                        Err(crate::Error::custom(format!(
                            "Cannot create symlink on {}: not supported",
                            dst_descriptor.type_name()
                        )))
                    }
                }
                CopyEntryKind::File if linked => {
                    bytes_done += entry.size_bytes;
                    Ok(Written::Done)
                }
                CopyEntryKind::File => {
                    copy_single_file(
                        &*src_vfs,
                        &*dst_vfs,
                        entry,
                        same_vfs,
                        &cancel,
                        reporter,
                        &mut bytes_done,
                        items_done,
                        &mut attributes.write,
                        &display,
                        create_new,
                    )
                    .await
                }
            };

            let error = match result {
                Ok(Written::Done) => {
                    succeeded = true;
                    break;
                }
                Ok(written) => {
                    let refused = matches!(written, Written::Refused);
                    // Every prompt a file in the way can raise offers Skip.
                    let destination = if refused
                        && reporter.answers(&IssueKind::AlreadyExists, IssueAction::Skip)
                    {
                        Destination::Skip
                    } else {
                        check_destination(&*dst_vfs, same_vfs, entry, reporter, &cancel).await?
                    };
                    match destination {
                        // A move leaves the source too, as a rename of one
                        // hard link onto another does.
                        Destination::Skip | Destination::Same => {
                            bytes_done = bytes_before + entry.size_bytes;
                            break;
                        }
                        // Gone since the refusal, or a conflict the
                        // destination could not confirm (S3's 409).
                        Destination::Absent if refused && !contradicted => {
                            contradicted = true;
                            continue;
                        }
                        Destination::Absent if refused => crate::Error::custom(format!(
                            "{} was refused as existing, but is not there",
                            entry.dest
                        )),
                        Destination::Absent | Destination::Merge | Destination::Replace => {
                            create_new = false;
                            continue;
                        }
                    }
                }
                Err(e) => e,
            };
            match reporter
                .handle_io_error(
                    error,
                    "Error",
                    Some(format!("{} -> {}", entry.source, entry.dest)),
                    &cancel,
                    true,
                )
                .await?
            {
                IssueOutcome::Skip => {
                    // Advance bytes so progress reaches 100% even with skips
                    bytes_done = bytes_before + entry.size_bytes;
                    break;
                }
                IssueOutcome::Retry => contradicted = false,
            }
        }

        if succeeded {
            if matches!(entry.kind, CopyEntryKind::Directory) {
                directories.push((entry, attributes));
            } else {
                attributes
                    .apply(
                        &*src_vfs,
                        &*dst_vfs,
                        &entry.dest,
                        &options,
                        reporter,
                        &cancel,
                    )
                    .await?;
                if let Some(id) = identity {
                    hard_links.entry(id).or_insert_with(|| entry.dest.clone());
                }
            }
        }

        // For move: delete source file/symlink immediately after successful copy.
        // Directories are cleaned up in a separate reverse pass below.
        if is_move
            && succeeded
            && matches!(
                &entry.kind,
                CopyEntryKind::File | CopyEntryKind::Symlink { .. }
            )
        {
            let mut src_retry = true;
            while src_retry {
                src_retry = false;
                if let Err(e) = src_vfs.remove_file(&entry.source).await {
                    match reporter
                        .handle_io_error(
                            e,
                            &format!("Error removing source {}", entry.source),
                            None,
                            &cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => {}
                        IssueOutcome::Retry => {
                            src_retry = true;
                        }
                    }
                }
            }
        }

        items_done += 1;
    }

    reporter.maybe_send_progress(bytes_done, items_done, "");

    for (entry, attributes) in directories.into_iter().rev() {
        attributes
            .apply(
                &*src_vfs,
                &*dst_vfs,
                &entry.dest,
                &options,
                reporter,
                &cancel,
            )
            .await?;
    }

    // For move: reverse pass to clean up empty source directories (deepest first).
    // DirectoryNotEmpty is expected (items may have been skipped) and silently ignored.
    // Other errors (e.g. permission denied) are reported through issue resolution.
    // A mount point stays: it cannot be removed while mounted, and its
    // parent then fails as not empty.
    if is_move {
        for entry in plan.entries.iter().rev() {
            if cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }
            if entry.mount_point {
                continue;
            }
            if let CopyEntryKind::Directory = &entry.kind {
                let mut dir_retry = true;
                while dir_retry {
                    dir_retry = false;
                    if let Err(e) = src_vfs.remove_dir(&entry.source).await {
                        if e.kind == crate::ErrorKind::DirectoryNotEmpty {
                            // Expected when child items were skipped — leave intact
                        } else {
                            match reporter
                                .handle_io_error(
                                    e,
                                    &format!("Error removing source directory {}", entry.source),
                                    None,
                                    &cancel,
                                    true,
                                )
                                .await?
                            {
                                IssueOutcome::Skip => {}
                                IssueOutcome::Retry => {
                                    dir_retry = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
