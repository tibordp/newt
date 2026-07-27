use super::*;

// --- Execute Move (async, uses Vfs) ---

pub(super) async fn execute_move(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    sources: Vec<VfsPath>,
    destination: VfsPath,
    options: CopyOptions,
    cancel: CancellationToken,
    rename_to: Option<&str>,
) -> Result<(), crate::Error> {
    debug_assert!(
        rename_to.is_none() || sources.len() == 1,
        "rename_to requires exactly one source"
    );
    // Follow redirect_target so moves from a SearchVfs operate on real files.
    let mut sources = sources;
    for s in sources.iter_mut() {
        *s = context.registry.dereference(s).await;
    }

    reject_self_destination(context, &sources, &destination, rename_to, true).await?;

    let src_vfs_id = sources
        .first()
        .ok_or_else(|| crate::Error::custom("no sources provided"))?
        .vfs_id;
    let dst_vfs_id = destination.vfs_id;
    let same_vfs = src_vfs_id == dst_vfs_id;

    let (src_vfs, _) = context.registry.resolve(&sources[0])?;
    let (_, dst_path) = context.registry.resolve(&destination)?;
    let src_descriptor = src_vfs.descriptor();

    let mut needs_copy = Vec::new();
    let mut renamed_count = 0u64;

    if same_vfs && src_descriptor.can_rename() {
        debug!(
            "execute_move: trying rename for {} sources (same VFS)",
            sources.len()
        );
        // Try rename first for each source (instant for same-VFS, same-device)
        for source in &sources {
            if cancel.is_cancelled() {
                return Err(crate::Error::cancelled());
            }

            let file_name = match source.file_name() {
                Some(f) => f,
                None => return Err(crate::Error::custom("source has no file name".to_string())),
            };
            let dest_local = dst_path.join(rename_to.unwrap_or(file_name));
            let source_local = source.path.clone();
            let mut overwrite_approved = false;

            // Check for destination conflicts before renaming (rename silently
            // overwrites). A destination that *is* the source — `Foo` moved to
            // `foo` on a case-insensitive volume — is a re-spelling, not a
            // conflict, so let the rename through. Asked only once something
            // is actually in the way, keeping bulk moves to one probe apiece.
            // Hard links land here as well; see `execute_rename` for why
            // that ends in a deliberate no-op.
            if let Ok(dest_file) = src_vfs.file_info(&dest_local).await
                && !src_vfs.same_file(&source_local, &dest_local).await?
            {
                let source_file = src_vfs.file_info(&source_local).await?;
                if dest_file.is_dir != source_file.is_dir {
                    // Type mismatch (file vs directory) — can only skip
                    let msg = if dest_file.is_dir {
                        format!("Cannot replace directory with file: {}", dest_local)
                    } else {
                        format!("Cannot replace file with directory: {}", dest_local)
                    };
                    match reporter
                        .raise_issue(IssueKind::AlreadyExists, msg, None, vec![IssueAction::Skip])
                        .await
                    {
                        Ok(IssueAction::Skip) => continue,
                        Err(e) => return Err(e),
                        _ => unreachable!("not offered"),
                    }
                } else if !dest_file.is_dir {
                    // Both are files — offer skip/overwrite
                    match reporter
                        .raise_issue(
                            IssueKind::AlreadyExists,
                            format!("File already exists: {}", dest_local),
                            None,
                            vec![IssueAction::Skip, IssueAction::Overwrite],
                        )
                        .await
                    {
                        Ok(IssueAction::Skip) => continue,
                        Ok(IssueAction::Overwrite) => {
                            // Proceed with rename — an atomic replace on
                            // backends that support it (POSIX rename,
                            // posix-rename SFTP servers). Backends that
                            // refuse report AlreadyExists, handled below.
                            overwrite_approved = true;
                        }
                        Err(e) => return Err(e),
                        _ => unreachable!("not offered"),
                    }
                } else {
                    // Both are directories: merge — the copy machinery
                    // merges into an existing destination; rename can't.
                    needs_copy.push(source.clone());
                    continue;
                }
            }

            let mut retry = true;
            while retry {
                retry = false;
                match src_vfs.rename(&source_local, &dest_local).await {
                    Ok(()) => {
                        debug!("execute_move: renamed {} -> {}", source_local, dest_local);
                        renamed_count += 1;
                    }
                    // Only "rename not supported" — for the backend or for
                    // this particular pair (cross-device in a RootVfs) —
                    // falls back to copy+delete; real failures surface as
                    // issues rather than silently degrading.
                    Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                        debug!(
                            "execute_move: rename unsupported for {}, falling back to copy+delete",
                            source_local
                        );
                        needs_copy.push(source.clone());
                    }
                    // A backend whose rename won't replace an existing
                    // destination (SFTP servers without posix-rename):
                    // the user approved the overwrite, so clear the
                    // destination and retry once. Keyed on the approval —
                    // an unexpected AlreadyExists still surfaces below.
                    Err(e) if e.kind == crate::ErrorKind::AlreadyExists && overwrite_approved => {
                        overwrite_approved = false;
                        match src_vfs.remove_file(&dest_local).await {
                            Ok(()) => retry = true,
                            Err(e) => {
                                match reporter
                                    .handle_io_error(
                                        e,
                                        &format!("Error replacing {}", dest_local),
                                        None,
                                        &cancel,
                                        false,
                                    )
                                    .await?
                                {
                                    IssueOutcome::Skip => {}
                                    IssueOutcome::Retry => unreachable!("not offered"),
                                }
                            }
                        }
                    }
                    Err(e) => {
                        match reporter
                            .handle_io_error(
                                e,
                                &format!("Error renaming {}", source_local),
                                Some(format!("{} -> {}", source_local, dest_local)),
                                &cancel,
                                true,
                            )
                            .await?
                        {
                            IssueOutcome::Skip => {}
                            IssueOutcome::Retry => retry = true,
                        }
                    }
                }
            }
        }
    } else {
        // Cross-VFS or VFS doesn't support rename: all sources need copy+delete
        needs_copy = sources.clone();
    }

    if needs_copy.is_empty() {
        reporter.send_prepared(0, renamed_count);
        reporter.maybe_send_progress(0, renamed_count, "");
        return Ok(());
    }

    // Fall back to copy-then-delete-per-file for cross-device/cross-VFS moves.
    // execute_copy with is_move=true deletes each source file immediately after
    // a successful copy, then cleans up empty source directories in reverse order.
    execute_copy(
        reporter,
        context,
        needs_copy,
        destination,
        options,
        cancel,
        true,
        renamed_count,
        rename_to,
    )
    .await
}

// --- Execute Rename ---

pub(super) async fn execute_rename(
    reporter: &mut ProgressReporter,
    context: &OperationContext,
    source: VfsPath,
    new_name: String,
    cancel: CancellationToken,
) -> Result<(), crate::Error> {
    // Follow redirect_target so renames from a SearchVfs operate on the
    // real file.
    let source = context.registry.dereference(&source).await;
    let parent = source
        .parent()
        .ok_or_else(|| crate::Error::custom("cannot rename the VFS root"))?;
    let new_path = parent.join(&new_name);
    if new_path.path == source.path {
        reporter.send_prepared(0, 0);
        return Ok(());
    }

    let (vfs, _) = context.registry.resolve(&source)?;
    let descriptor = vfs.descriptor();

    if descriptor.can_rename() {
        // Check for destination conflicts before renaming (rename silently
        // overwrites) — same policy as the Move fast path, including the
        // re-spelling exemption: `Foo` → `foo` on a case-insensitive volume
        // (or NFC → NFD on HFS+) resolves to the source itself, which is
        // the whole point of the rename rather than an obstacle to it.
        //
        // Renaming one hard link onto another lands here too, and is a
        // deliberate no-op: POSIX has `rename` succeed without acting when
        // both names are links to one file, so both survive and nothing is
        // reported — BSD `mv` to the letter (GNU `mv` refuses instead).
        // Rare, and doing nothing is the safe end of the trade.
        let mut attempt_rename = true;
        let mut overwrite_approved = false;
        if let Ok(dest_file) = vfs.file_info(&new_path.path).await
            && !vfs.same_file(&source.path, &new_path.path).await?
        {
            let source_file = vfs.file_info(&source.path).await?;
            if dest_file.is_dir != source_file.is_dir {
                let msg = if dest_file.is_dir {
                    format!("Cannot replace directory with file: {}", new_path.path)
                } else {
                    format!("Cannot replace file with directory: {}", new_path.path)
                };
                match reporter
                    .raise_issue(IssueKind::AlreadyExists, msg, None, vec![IssueAction::Skip])
                    .await
                {
                    Ok(IssueAction::Skip) => {
                        reporter.send_prepared(0, 0);
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                    _ => unreachable!("not offered"),
                }
            } else if !dest_file.is_dir {
                match reporter
                    .raise_issue(
                        IssueKind::AlreadyExists,
                        format!("File already exists: {}", new_path.path),
                        None,
                        vec![IssueAction::Skip, IssueAction::Overwrite],
                    )
                    .await
                {
                    Ok(IssueAction::Skip) => {
                        reporter.send_prepared(0, 0);
                        return Ok(());
                    }
                    Ok(IssueAction::Overwrite) => {
                        // Proceed with rename — an atomic replace on
                        // backends that support it (POSIX rename,
                        // posix-rename SFTP servers). Backends that refuse
                        // report AlreadyExists, handled below.
                        overwrite_approved = true;
                    }
                    Err(e) => return Err(e),
                    _ => unreachable!("not offered"),
                }
            } else {
                // Both are directories: merge — the copy machinery merges
                // into an existing destination; rename can't.
                attempt_rename = false;
            }
        }

        let mut retry = attempt_rename;
        while retry {
            retry = false;
            match vfs.rename(&source.path, &new_path.path).await {
                Ok(()) => {
                    debug!("execute_rename: renamed {} -> {}", source, new_path);
                    reporter.send_prepared(0, 1);
                    reporter.maybe_send_progress(0, 1, &new_name);
                    return Ok(());
                }
                // "Not supported" — for the backend or this particular pair
                // — falls back to copy+delete below; real failures surface
                // as issues.
                Err(e) if e.kind == crate::ErrorKind::NotSupported => {
                    debug!(
                        "execute_rename: rename unsupported for {}, falling back to copy+delete",
                        source
                    );
                }
                // A backend whose rename won't replace an existing
                // destination (SFTP servers without posix-rename): the
                // user approved the overwrite, so clear the destination
                // and retry once.
                Err(e) if e.kind == crate::ErrorKind::AlreadyExists && overwrite_approved => {
                    overwrite_approved = false;
                    match vfs.remove_file(&new_path.path).await {
                        Ok(()) => retry = true,
                        Err(e) => {
                            match reporter
                                .handle_io_error(
                                    e,
                                    &format!("Error replacing {}", new_path.path),
                                    None,
                                    &cancel,
                                    false,
                                )
                                .await?
                            {
                                IssueOutcome::Skip => {
                                    reporter.send_prepared(0, 0);
                                    return Ok(());
                                }
                                IssueOutcome::Retry => unreachable!("not offered"),
                            }
                        }
                    }
                }
                Err(e) => {
                    match reporter
                        .handle_io_error(
                            e,
                            &format!("Error renaming {}", source),
                            Some(format!("{} -> {}", source, new_path)),
                            &cancel,
                            true,
                        )
                        .await?
                    {
                        IssueOutcome::Skip => {
                            reporter.send_prepared(0, 0);
                            return Ok(());
                        }
                        IssueOutcome::Retry => retry = true,
                    }
                }
            }
        }
    }

    // No native rename (S3, …) or it failed: copy to the new name and
    // delete the source. Same-VFS copies take the copy_within fast path
    // (server-side CopyObject on S3), so no data flows through the app.
    // Timestamps are preserved where the VFS allows it — a rename should
    // not look like a fresh file.
    let options = CopyOptions {
        preserve_timestamps: true,
        ..CopyOptions::default()
    };
    execute_copy(
        reporter,
        context,
        vec![source],
        parent,
        options,
        cancel,
        true,
        0,
        Some(&new_name),
    )
    .await
}
