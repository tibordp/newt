use super::*;
use crate::vfs::VfsMetadata;
use crate::vfs::attributes::{Attribute, WriteOptions};

pub(super) struct SourceAttributes {
    metadata: Option<VfsMetadata>,
    properties: Vec<(AttributeKind, Attribute)>,
    pub write: WriteOptions,
    pub identity: Option<Vec<u8>>,
}

pub(super) async fn resolve_preservation(
    reporter: &mut ProgressReporter,
    kind: AttributeKind,
    path: &Path,
    error: crate::Error,
) -> Result<bool, crate::Error> {
    if error.kind == crate::ErrorKind::Cancelled {
        return Err(error);
    }
    match reporter
        .raise_issue(
            IssueKind::PreservationFailed(kind),
            format!("Cannot preserve {} for {path}: {error}", kind.label()),
            None,
            vec![IssueAction::Retry, IssueAction::Skip],
        )
        .await?
    {
        IssueAction::Retry => Ok(true),
        IssueAction::Skip => Ok(false),
        _ => unreachable!("not offered"),
    }
}

impl SourceAttributes {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn read(
        vfs: &dyn Vfs,
        destination: &dyn Vfs,
        path: &Path,
        file: &File,
        directory_metadata: Option<VfsMetadata>,
        options: &CopyOptions,
        reporter: &mut ProgressReporter,
        cancel: &CancellationToken,
    ) -> Result<Self, crate::Error> {
        let mut result = Self {
            metadata: directory_metadata,
            properties: Vec::new(),
            write: WriteOptions::default(),
            identity: None,
        };
        // A destination that declares no metadata support (object stores)
        // gets no stat-derived attributes and no prompts about them: the
        // permissions default is on for everyone.
        if (!file.is_dir || file.is_symlink)
            && destination.descriptor().can_set_metadata()
            && (options.preserve_permissions
                || options.preserve_owner
                || options.preserve_group
                || options.preserve_timestamps)
        {
            loop {
                match cancellable(cancel, vfs.get_metadata(path)).await {
                    Ok(meta) => {
                        result.metadata = Some(meta);
                        break;
                    }
                    Err(e) => {
                        if !resolve_preservation(reporter, AttributeKind::Metadata, path, e).await?
                        {
                            break;
                        }
                    }
                }
            }
        }
        let by_name = options.ownership_by_name;
        let regular = !file.is_dir && !file.is_symlink;
        // Numeric ownership rides on the stat; the attribute verb serves
        // names, and native identities where there is no numeric id.
        let uid_known = result.metadata.as_ref().and_then(|m| m.uid).is_some();
        let gid_known = result.metadata.as_ref().and_then(|m| m.gid).is_some();
        for (enabled, kind) in [
            (
                options.preserve_owner && (by_name || !uid_known),
                AttributeKind::Owner { by_name },
            ),
            (
                options.preserve_group && (by_name || !gid_known),
                AttributeKind::Group { by_name },
            ),
            (
                options.preserve_hard_links && regular,
                AttributeKind::HardLinks,
            ),
            (options.preserve_sparse && regular, AttributeKind::Sparse),
            (options.preserve_xattrs, AttributeKind::ExtendedAttributes),
            (options.preserve_acl, AttributeKind::AccessControl),
            (options.preserve_streams, AttributeKind::Streams),
            (
                options.preserve_object_metadata && regular,
                AttributeKind::ObjectMetadata,
            ),
            (
                options.preserve_object_tags && regular,
                AttributeKind::ObjectTags,
            ),
            (
                options.preserve_object_access && regular && options.object_canned_acl.is_none(),
                AttributeKind::ObjectAccess,
            ),
        ] {
            if !enabled {
                continue;
            }
            loop {
                match cancellable(cancel, vfs.read_attribute(path, kind)).await {
                    Ok(Some(Attribute::Identity(identity))) => {
                        result.identity = Some(identity);
                        break;
                    }
                    Ok(Some(Attribute::Sparse(sparse))) => {
                        result.write.sparse = sparse;
                        break;
                    }
                    Ok(Some(Attribute::ObjectMetadata(meta))) => {
                        result.write.object_metadata = Some(meta);
                        break;
                    }
                    Ok(Some(property)) => {
                        result.properties.push((kind, property));
                        break;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        if !resolve_preservation(reporter, kind, path, e).await? {
                            break;
                        }
                    }
                }
            }
        }
        if regular {
            result.write.object_storage_class = options.object_storage_class.clone();
            result.write.object_canned_acl = options.object_canned_acl.clone();
        }
        Ok(result)
    }

    pub(super) async fn apply(
        &self,
        source: &dyn Vfs,
        destination: &dyn Vfs,
        path: &Path,
        options: &CopyOptions,
        reporter: &mut ProgressReporter,
        cancel: &CancellationToken,
    ) -> Result<(), crate::Error> {
        // Ownership first: chown can clear set-id bits. Streams precede final
        // times and permissions because writing them can change both.
        if let Some(meta) = &self.metadata {
            for (enabled, kind, patch) in [
                (
                    options.preserve_owner && !options.ownership_by_name && meta.uid.is_some(),
                    AttributeKind::Owner { by_name: false },
                    VfsMetadata {
                        uid: meta.uid,
                        ..Default::default()
                    },
                ),
                (
                    options.preserve_group && !options.ownership_by_name && meta.gid.is_some(),
                    AttributeKind::Group { by_name: false },
                    VfsMetadata {
                        gid: meta.gid,
                        ..Default::default()
                    },
                ),
            ] {
                if enabled {
                    apply_metadata(destination, path, &patch, kind, reporter, cancel).await?;
                }
            }
        }
        for (kind, property) in self
            .properties
            .iter()
            .filter(|(kind, _)| *kind != AttributeKind::AccessControl)
        {
            loop {
                let outcome = match property {
                    Attribute::Streams(streams) => {
                        copy_streams(source, destination, path, streams, cancel).await
                    }
                    _ => cancellable(cancel, destination.write_attribute(path, property)).await,
                };
                match outcome {
                    Ok(()) => break,
                    Err(e) => {
                        if !resolve_preservation(reporter, *kind, path, e).await? {
                            break;
                        }
                    }
                }
            }
        }
        if let Some(meta) = &self.metadata
            && options.preserve_permissions
            && meta.permissions.is_some()
        {
            apply_metadata(
                destination,
                path,
                &VfsMetadata {
                    permissions: meta.permissions,
                    ..Default::default()
                },
                AttributeKind::Permissions,
                reporter,
                cancel,
            )
            .await?;
        }
        // chmod changes POSIX ACL masks, so the complete ACL goes last.
        for (_, property) in self
            .properties
            .iter()
            .filter(|(kind, _)| *kind == AttributeKind::AccessControl)
        {
            loop {
                match cancellable(cancel, destination.write_attribute(path, property)).await {
                    Ok(()) => break,
                    Err(e) => {
                        if !resolve_preservation(reporter, AttributeKind::AccessControl, path, e)
                            .await?
                        {
                            break;
                        }
                    }
                }
            }
        }
        if let Some(meta) = &self.metadata
            && options.preserve_timestamps
            && (meta.atime.is_some() || meta.mtime.is_some())
        {
            apply_metadata(
                destination,
                path,
                &VfsMetadata {
                    atime: meta.atime,
                    mtime: meta.mtime,
                    ..Default::default()
                },
                AttributeKind::Timestamps,
                reporter,
                cancel,
            )
            .await?;
        }
        Ok(())
    }
}

async fn apply_metadata(
    vfs: &dyn Vfs,
    path: &Path,
    metadata: &VfsMetadata,
    kind: AttributeKind,
    reporter: &mut ProgressReporter,
    cancel: &CancellationToken,
) -> Result<(), crate::Error> {
    loop {
        match cancellable(cancel, vfs.set_metadata(path, metadata)).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if !resolve_preservation(reporter, kind, path, e).await? {
                    return Ok(());
                }
            }
        }
    }
}

async fn copy_streams(
    source: &dyn Vfs,
    destination: &dyn Vfs,
    path: &Path,
    streams: &[crate::vfs::attributes::NamedStream],
    cancel: &CancellationToken,
) -> Result<(), crate::Error> {
    use tokio::io::AsyncReadExt;
    for stream in streams {
        let target = cancellable(cancel, destination.stream_path(path, &stream.name)).await?;
        let mut reader = cancellable(cancel, source.open_read_async(&stream.path)).await?;
        let mut writer = cancellable(
            cancel,
            destination.overwrite_async(&target, &Default::default()),
        )
        .await?;
        let mut buf = vec![0; VFS_READ_CHUNK_SIZE];
        loop {
            let n = cancellable(cancel, async { Ok(reader.read(&mut buf).await?) }).await?;
            if n == 0 {
                break;
            }
            let mut offset = 0;
            while offset < n {
                let written = cancellable(cancel, writer.write(&buf[offset..n])).await?;
                if written == 0 {
                    return Err(crate::Error::custom("stream write made no progress"));
                }
                offset += written;
            }
        }
        cancellable(cancel, writer.finish()).await?;
    }
    Ok(())
}
