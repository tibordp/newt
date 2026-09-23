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
    src_descriptor: &dyn VfsDescriptor,
    sources: &[PathBuf],
    destination: &Path,
    rename_to: Option<&str>,
    walk: &WalkOptions,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<CopyPlan, crate::Error> {
    let (walked, total_bytes) =
        walk_sources(src_vfs, src_descriptor, sources, walk, reporter, cancel).await?;

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
) -> Result<(), crate::Error> {
    let src_descriptor = src_vfs.descriptor();
    let dst_descriptor = dst_vfs.descriptor();

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
                return Ok(());
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
        let mut reader = cancellable(cancel, src_vfs.open_read_async(&entry.source)).await?;
        let mut writer = loop {
            match cancellable(cancel, dst_vfs.overwrite_async(&entry.dest, write_options)).await {
                Ok(writer) => break writer,
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

        copy_bytes_async(
            &mut *reader,
            &mut *writer,
            cancel,
            reporter,
            bytes_done,
            items_done,
            display,
        )
        .await?;
        cancellable(cancel, writer.finish()).await?;

        return Ok(());
    }

    Err(crate::Error::not_supported())
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
        src_descriptor,
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

        let mut merged_directory = false;
        let dest_file = dst_vfs.file_info(&entry.dest).await;
        if let Ok(dest_file) = dest_file {
            match &entry.kind {
                CopyEntryKind::Directory => {
                    if dest_file.is_dir {
                        merged_directory = true;
                        // Directory already exists — merge, skip mkdir.
                        if !options.preserve_merged_directories {
                            items_done += 1;
                            continue;
                        }
                    } else {
                        // Type mismatch
                        match reporter
                            .raise_issue(
                                IssueKind::AlreadyExists,
                                format!("Cannot replace file with directory: {}", entry.dest),
                                None,
                                vec![IssueAction::Skip],
                            )
                            .await
                        {
                            Ok(IssueAction::Skip) => {
                                items_done += 1;
                                continue;
                            }
                            Err(e) => return Err(e),
                            _ => unreachable!("not offered"),
                        }
                    }
                }
                CopyEntryKind::File | CopyEntryKind::Symlink { .. } => {
                    if dest_file.is_dir {
                        // Type mismatch
                        match reporter
                            .raise_issue(
                                IssueKind::AlreadyExists,
                                format!("Cannot replace directory with file: {}", entry.dest),
                                None,
                                vec![IssueAction::Skip],
                            )
                            .await
                        {
                            Ok(IssueAction::Skip) => {
                                bytes_done += entry.size_bytes;
                                items_done += 1;
                                continue;
                            }
                            Err(e) => return Err(e),
                            _ => unreachable!("not offered"),
                        }
                    } else {
                        // Both are files (or symlinks)
                        match reporter
                            .raise_issue(
                                IssueKind::AlreadyExists,
                                format!("File already exists: {}", entry.dest),
                                None,
                                vec![IssueAction::Skip, IssueAction::Overwrite],
                            )
                            .await
                        {
                            Ok(IssueAction::Skip) => {
                                bytes_done += entry.size_bytes;
                                items_done += 1;
                                continue;
                            }
                            Ok(IssueAction::Overwrite) => {
                                let source_is_symlink =
                                    matches!(&entry.kind, CopyEntryKind::Symlink { .. });
                                if dest_file.is_symlink || source_is_symlink {
                                    // Remove when either side is a symlink:
                                    // - dest is symlink: writing would go through to the
                                    //   target rather than replacing the symlink itself
                                    // - source is symlink: create_symlink can't overwrite
                                    //   an existing file
                                    dst_vfs.remove_file(&entry.dest).await?;
                                }
                                // For regular file → regular file: overwrite in place.
                            }
                            Err(e) => return Err(e),
                            _ => unreachable!("not offered"),
                        }
                    }
                }
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
        let mut linked = false;
        if let Some(target) = identity.as_ref().and_then(|id| hard_links.get(id)) {
            loop {
                let result = async {
                    if dst_vfs.file_info(&entry.dest).await.is_ok() {
                        dst_vfs.remove_file(&entry.dest).await?;
                    }
                    dst_vfs.hard_link(&entry.dest, target).await
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
        let mut retry = true;
        let mut succeeded = false;
        while retry {
            retry = false;
            bytes_done = bytes_before; // Reset progress on retry to avoid double-counting

            let result = match &entry.kind {
                CopyEntryKind::Directory if merged_directory => Ok(()),
                CopyEntryKind::Directory => {
                    cancellable(&cancel, dst_vfs.create_directory(&entry.dest)).await
                }
                CopyEntryKind::Symlink { target } => {
                    if dst_descriptor.can_create_symlink() {
                        cancellable(
                            &cancel,
                            dst_vfs.create_symlink(&entry.dest, target.as_str()),
                        )
                        .await
                    } else {
                        Err(crate::Error::custom(format!(
                            "Cannot create symlink on {}: not supported",
                            dst_descriptor.type_name()
                        )))
                    }
                }
                CopyEntryKind::File if linked => {
                    bytes_done += entry.size_bytes;
                    Ok(())
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
                    )
                    .await
                }
            };

            match result {
                Ok(()) => {
                    succeeded = true;
                }
                Err(e) => {
                    match reporter
                        .handle_io_error(
                            e,
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
                        }
                        IssueOutcome::Retry => {
                            retry = true;
                        }
                    }
                }
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
