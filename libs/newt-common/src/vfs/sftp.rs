use std::path::{Path, PathBuf};
use std::time::SystemTime;

use log::{debug, error, info, warn};
use openssh_sftp_client::Sftp;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats, Mode, UserGroup};
use crate::{Error, ErrorKind};

use super::{
    Breadcrumb, MountRequest, RegisteredDescriptor, Vfs, VfsAsyncWriter, VfsChangeNotifier,
    VfsDescriptor, VfsMetadata,
};

// ---------------------------------------------------------------------------
// SftpVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SftpVfsDescriptor;

impl VfsDescriptor for SftpVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "sftp"
    }
    fn display_name(&self) -> &'static str {
        "SFTP"
    }
    fn auto_mount_request(&self) -> Option<MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read_sync(&self) -> bool {
        false
    }
    fn can_read_async(&self) -> bool {
        true
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        true
    }
    fn can_create_directory(&self) -> bool {
        true
    }
    fn can_create_symlink(&self) -> bool {
        true
    }
    fn can_touch(&self) -> bool {
        true
    }
    fn can_truncate(&self) -> bool {
        true
    }
    fn can_set_metadata(&self) -> bool {
        true
    }
    fn can_remove(&self) -> bool {
        true
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_fs_stats(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        true
    }
    fn can_copy_within(&self) -> bool {
        false
    }
    fn can_hard_link(&self) -> bool {
        true
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        let host = String::from_utf8_lossy(mount_meta);
        let s = path.to_string_lossy();
        format!("sftp://{}{}", host, s)
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        let host = String::from_utf8_lossy(mount_meta);
        let mut crumbs = Vec::new();
        let s = path.to_string_lossy();
        let segments: Vec<&str> = s.split('/').filter(|s| !s.is_empty()).collect();

        crumbs.push(Breadcrumb {
            label: format!("sftp://{}/", host),
            nav_path: "/".to_string(),
        });

        let mut accumulated = String::new();
        for (i, seg) in segments.iter().enumerate() {
            accumulated.push('/');
            accumulated.push_str(seg);
            let is_last = i == segments.len() - 1;
            crumbs.push(Breadcrumb {
                label: if is_last {
                    seg.to_string()
                } else {
                    format!("{}/", seg)
                },
                nav_path: accumulated.clone(),
            });
        }

        crumbs
    }

    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<PathBuf> {
        let rest = input.strip_prefix("sftp://")?;
        let host = String::from_utf8_lossy(mount_meta);
        let after_host = rest.strip_prefix(host.as_ref())?;
        if after_host.is_empty() || after_host == "/" {
            Some(PathBuf::from("/"))
        } else if after_host.starts_with('/') {
            Some(PathBuf::from(after_host))
        } else {
            None
        }
    }

    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        let host = String::from_utf8_lossy(mount_meta);
        if host.is_empty() {
            None
        } else {
            Some(host.into_owned())
        }
    }
}

pub static SFTP_VFS_DESCRIPTOR: SftpVfsDescriptor = SftpVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&SFTP_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// SftpVfs
// ---------------------------------------------------------------------------

pub struct SftpVfs {
    sftp: tokio::sync::Mutex<Sftp>,
    host: String,
    notifier: VfsChangeNotifier,
    _child: tokio::sync::Mutex<tokio::process::Child>,
}

/// Timeout for the SSH connection + SFTP handshake.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

impl SftpVfs {
    pub async fn connect(host: &str) -> Result<Self, Error> {
        info!("sftp: connecting to {}", host);

        match tokio::time::timeout(CONNECT_TIMEOUT, Self::connect_inner(host)).await {
            Ok(result) => result,
            Err(_) => {
                error!(
                    "sftp: connection to {} timed out after {:?}",
                    host, CONNECT_TIMEOUT
                );
                Err(Error {
                    kind: ErrorKind::Connection,
                    message: format!(
                        "SSH connection to '{}' timed out after {} seconds",
                        host,
                        CONNECT_TIMEOUT.as_secs()
                    ),
                })
            }
        }
    }

    async fn connect_inner(host: &str) -> Result<Self, Error> {
        let mut child = tokio::process::Command::new("ssh")
            .arg(host)
            .arg("-s")
            .arg("sftp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                error!("sftp: failed to spawn ssh for {}: {}", host, e);
                Error {
                    kind: ErrorKind::Connection,
                    message: format!("Failed to start SSH: {}", e),
                }
            })?;

        debug!(
            "sftp: ssh process spawned for {}, pid={:?}",
            host,
            child.id()
        );

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();

        // Race the SFTP handshake against the ssh process exiting.
        // If ssh exits first (auth failure, bad host, etc.), we read stderr
        // and produce a clear error. If the handshake succeeds, we start a
        // background task to log any subsequent stderr output.
        let handshake = Sftp::new(stdin, stdout, openssh_sftp_client::SftpOptions::default());

        tokio::select! {
            result = handshake => {
                match result {
                    Ok(sftp) => {
                        info!("sftp: connected to {}", host);

                        // Connection succeeded — log any future stderr in the background
                        let host_owned = host.to_string();
                        tokio::spawn(async move {
                            use tokio::io::{AsyncBufReadExt, BufReader};
                            let mut lines = BufReader::new(stderr).lines();
                            while let Ok(Some(line)) = lines.next_line().await {
                                warn!("sftp [{}] stderr: {}", host_owned, line);
                            }
                        });

                        Ok(Self {
                            sftp: tokio::sync::Mutex::new(sftp),
                            host: host.to_string(),
                            notifier: VfsChangeNotifier::new(),
                            _child: tokio::sync::Mutex::new(child),
                        })
                    }
                    Err(e) => {
                        error!("sftp: handshake with {} failed: {}", host, e);
                        let _ = child.kill().await;
                        let stderr_text = Self::read_stderr(&mut stderr).await;
                        Err(Self::connect_error(host, &e.to_string(), &stderr_text, child.try_wait().ok().flatten()))
                    }
                }
            }
            status = child.wait() => {
                // ssh exited before the handshake completed
                let stderr_text = Self::read_stderr(&mut stderr).await;
                let exit_msg = match &status {
                    Ok(s) => format!("ssh exited with {}", s),
                    Err(e) => format!("failed to wait on ssh: {}", e),
                };
                error!("sftp: {} for {}", exit_msg, host);
                Err(Self::connect_error(host, &exit_msg, &stderr_text, status.ok()))
            }
        }
    }

    async fn read_stderr(stderr: &mut tokio::process::ChildStderr) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = Vec::new();
        // Read whatever is available (ssh has exited, so this won't block long)
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).trim().to_string()
    }

    fn connect_error(
        host: &str,
        error: &str,
        stderr_text: &str,
        status: Option<std::process::ExitStatus>,
    ) -> Error {
        let exit_info = status
            .map(|s| format!(" (exit status: {})", s))
            .unwrap_or_default();

        // Prefer stderr (e.g. "Permission denied (publickey)") over the
        // generic SFTP-level error ("unexpected end of file").
        let detail = if stderr_text.is_empty() {
            error.to_string()
        } else {
            stderr_text.to_string()
        };

        Error {
            kind: ErrorKind::Connection,
            message: format!("Failed to connect to '{}': {}{}", host, detail, exit_info),
        }
    }
}

impl Drop for SftpVfs {
    fn drop(&mut self) {
        // Kill the SSH process on cleanup
        let child = self._child.get_mut();
        if let Err(e) = child.start_kill() {
            warn!("sftp: failed to kill ssh process: {}", e);
        }
    }
}

fn sftp_time_to_system_time(ts: openssh_sftp_client::UnixTimeStamp) -> SystemTime {
    ts.as_system_time()
}

fn permissions_to_mode(p: &openssh_sftp_client::metadata::Permissions) -> u32 {
    let mut mode: u32 = 0;
    if p.suid() {
        mode |= 0o4000;
    }
    if p.sgid() {
        mode |= 0o2000;
    }
    if p.svtx() {
        mode |= 0o1000;
    }
    if p.read_by_owner() {
        mode |= 0o400;
    }
    if p.write_by_owner() {
        mode |= 0o200;
    }
    if p.execute_by_owner() {
        mode |= 0o100;
    }
    if p.read_by_group() {
        mode |= 0o040;
    }
    if p.write_by_group() {
        mode |= 0o020;
    }
    if p.execute_by_group() {
        mode |= 0o010;
    }
    if p.read_by_other() {
        mode |= 0o004;
    }
    if p.write_by_other() {
        mode |= 0o002;
    }
    if p.execute_by_other() {
        mode |= 0o001;
    }
    mode
}

fn mode_to_permissions(mode: u32) -> openssh_sftp_client::metadata::Permissions {
    let mut p = openssh_sftp_client::metadata::Permissions::new();
    p.set_suid((mode & 0o4000) != 0);
    p.set_sgid((mode & 0o2000) != 0);
    p.set_vtx((mode & 0o1000) != 0);
    p.set_read_by_owner((mode & 0o400) != 0);
    p.set_write_by_owner((mode & 0o200) != 0);
    p.set_execute_by_owner((mode & 0o100) != 0);
    p.set_read_by_group((mode & 0o040) != 0);
    p.set_write_by_group((mode & 0o020) != 0);
    p.set_execute_by_group((mode & 0o010) != 0);
    p.set_read_by_other((mode & 0o004) != 0);
    p.set_write_by_other((mode & 0o002) != 0);
    p.set_execute_by_other((mode & 0o001) != 0);
    p
}

fn metadata_to_file(
    name: String,
    meta: &openssh_sftp_client::metadata::MetaData,
    follow_meta: Option<&openssh_sftp_client::metadata::MetaData>,
) -> File {
    let is_symlink = meta.file_type().is_some_and(|ft| ft.is_symlink());

    let effective_meta = if is_symlink {
        follow_meta.unwrap_or(meta)
    } else {
        meta
    };
    let is_dir = effective_meta.file_type().is_some_and(|ft| ft.is_dir());
    let size = if is_dir { None } else { meta.len() };

    let mode = meta.permissions().map_or(0, |p| permissions_to_mode(&p));

    File {
        is_hidden: name.starts_with('.'),
        name,
        size,
        is_dir,
        is_symlink,
        symlink_target: None,
        user: meta.uid().map(UserGroup::Id),
        group: meta.gid().map(UserGroup::Id),
        mode: Mode(mode),
        modified: meta
            .modified()
            .map(|t| sftp_time_to_system_time(t).to_unix()),
        accessed: meta
            .accessed()
            .map(|t| sftp_time_to_system_time(t).to_unix()),
        created: None,
    }
}

use crate::ToUnix;

#[async_trait::async_trait]
impl Vfs for SftpVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &SFTP_VFS_DESCRIPTOR
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.host.as_bytes().to_vec()
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
        debug!("sftp: list_files {}", path.display());

        let dir = {
            let sftp = self.sftp.lock().await;
            let mut fs = sftp.fs();
            fs.open_dir(path).await?
        };

        let mut files = Vec::new();

        // ".." entry
        if let Some(parent) = path.parent()
            && parent != path
        {
            files.push(File {
                name: "..".to_string(),
                size: None,
                is_dir: true,
                is_hidden: false,
                is_symlink: false,
                symlink_target: None,
                user: None,
                group: None,
                mode: Mode(0),
                modified: None,
                accessed: None,
                created: None,
            });
        }

        use futures::StreamExt;
        let read_dir = dir.read_dir();
        tokio::pin!(read_dir);
        while let Some(entry) = read_dir.next().await {
            let entry = entry?;
            let name = entry.filename().to_string_lossy().to_string();
            if name == "." || name == ".." {
                continue;
            }

            let meta = entry.metadata();
            let is_symlink = meta.file_type().is_some_and(|ft| ft.is_symlink());

            // For symlinks, try to stat the target to determine if it's a directory
            let follow_meta = if is_symlink {
                let target_path = path.join(&name);
                let sftp = self.sftp.lock().await;
                let mut fs = sftp.fs();
                fs.metadata(target_path).await.ok()
            } else {
                None
            };

            let file = metadata_to_file(name, &meta, follow_meta.as_ref());
            files.push(file);
        }

        if let Some(tx) = batch_tx
            && !files.is_empty()
        {
            let _ = tx.send(files.clone());
        }

        Ok(files)
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        self.notifier.watch(path).await;
        Ok(())
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        debug!("sftp: file_info {}", path.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();

        let symlink_meta = fs.symlink_metadata(path).await?;

        let is_symlink = symlink_meta.file_type().is_some_and(|ft| ft.is_symlink());

        let follow_meta = if is_symlink {
            fs.metadata(path).await.ok()
        } else {
            None
        };

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        Ok(metadata_to_file(name, &symlink_meta, follow_meta.as_ref()))
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        debug!("sftp: file_details {}", path.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();

        let symlink_meta = fs.symlink_metadata(path).await?;

        let is_symlink = symlink_meta.file_type().is_some_and(|ft| ft.is_symlink());

        let effective_meta = if is_symlink {
            fs.metadata(path).await.unwrap_or(symlink_meta)
        } else {
            symlink_meta
        };

        let is_dir = effective_meta.file_type().is_some_and(|ft| ft.is_dir());
        let size = effective_meta.len().unwrap_or(0);
        let mode = effective_meta
            .permissions()
            .map(|p| Mode(permissions_to_mode(&p)));

        let symlink_target = if is_symlink {
            fs.read_link(path)
                .await
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        } else {
            None
        };

        drop(fs);

        // Detect MIME type from file header
        let mime_type = if is_dir {
            None
        } else {
            match self.read_range_inner(&sftp, path, 0, 8192).await {
                Ok(chunk) => {
                    let detected = mimetype_detector::detect(&chunk.data);
                    if detected.is("application/octet-stream") {
                        if !chunk.data.contains(&0) {
                            Some("text/plain".to_string())
                        } else {
                            Some("application/octet-stream".to_string())
                        }
                    } else {
                        Some(detected.mime().to_string())
                    }
                }
                Err(_) => None,
            }
        };

        Ok(FileDetails {
            size,
            mime_type,
            is_dir,
            is_symlink,
            symlink_target: symlink_target.map(PathBuf::from),
            user: symlink_meta.uid().map(UserGroup::Id),
            group: symlink_meta.gid().map(UserGroup::Id),
            mode,
            modified: symlink_meta
                .modified()
                .map(|t| sftp_time_to_system_time(t).to_unix()),
            accessed: symlink_meta
                .accessed()
                .map(|t| sftp_time_to_system_time(t).to_unix()),
            created: None,
        })
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        debug!(
            "sftp: read_range {} offset={} length={}",
            path.display(),
            offset,
            length
        );
        let sftp = self.sftp.lock().await;
        self.read_range_inner(&sftp, path, offset, length).await
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        debug!("sftp: open_read_async {}", path.display());
        // Read entire file into memory since TokioCompatFile is !Unpin
        // and we need to return Box<dyn AsyncRead + Send + Unpin>.
        let sftp = self.sftp.lock().await;
        let mut file = sftp.open(path).await?;

        // Get file size for read_all, or use a large default
        let file_len = {
            let mut fs = sftp.fs();
            fs.metadata(path)
                .await
                .ok()
                .and_then(|m| m.len())
                .unwrap_or(1024 * 1024) as usize
        };
        let data = file.read_all(file_len, bytes::BytesMut::new()).await?;

        Ok(Box::new(std::io::Cursor::new(data.freeze())))
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        debug!("sftp: overwrite_async {}", path.display());
        let sftp = self.sftp.lock().await;
        let file = sftp
            .options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await?;

        Ok(Box::new(SftpAsyncWriter {
            file,
            notifier: self.notifier.clone(),
            path: path.to_path_buf(),
        }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: create_directory {}", path.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        fs.create_dir(path).await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn create_symlink(&self, link: &Path, target: &Path) -> Result<(), Error> {
        debug!(
            "sftp: create_symlink {} -> {}",
            link.display(),
            target.display()
        );
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        fs.symlink(target, link).await?;
        self.notifier.notify(link);
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: remove_file {}", path.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        fs.remove_file(path).await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: remove_dir {}", path.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        fs.remove_dir(path).await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        debug!("sftp: rename {} -> {}", from.display(), to.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        fs.rename(from, to).await?;
        self.notifier.notify(from);
        self.notifier.notify(to);
        Ok(())
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        debug!("sftp: hard_link {} -> {}", link.display(), target.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        fs.hard_link(target, link).await?;
        self.notifier.notify(link);
        Ok(())
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: touch {}", path.display());
        let sftp = self.sftp.lock().await;
        let file = sftp.options().create(true).write(true).open(path).await?;
        file.close().await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: truncate {}", path.display());
        let sftp = self.sftp.lock().await;
        let file = sftp.options().write(true).truncate(true).open(path).await?;
        file.close().await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        debug!("sftp: get_metadata {}", path.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();
        let meta = fs.symlink_metadata(path).await?;

        Ok(VfsMetadata {
            permissions: meta.permissions().map(|p| permissions_to_mode(&p)),
            uid: meta.uid(),
            gid: meta.gid(),
            atime: meta.accessed().map(sftp_time_to_system_time),
            mtime: meta.modified().map(sftp_time_to_system_time),
        })
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        debug!("sftp: set_metadata {}", path.display());
        let sftp = self.sftp.lock().await;
        let mut fs = sftp.fs();

        let mut sftp_meta = openssh_sftp_client::metadata::MetaDataBuilder::new();

        if let Some(permissions) = meta.permissions {
            sftp_meta.permissions(mode_to_permissions(permissions));
        }

        if meta.uid.is_some() || meta.gid.is_some() {
            let current = fs.symlink_metadata(path).await.ok();
            let uid = meta
                .uid
                .or_else(|| current.as_ref().and_then(|m| m.uid()))
                .unwrap_or(0);
            let gid = meta
                .gid
                .or_else(|| current.as_ref().and_then(|m| m.gid()))
                .unwrap_or(0);
            sftp_meta.id((uid, gid));
        }

        if meta.atime.is_some() || meta.mtime.is_some() {
            use openssh_sftp_client::UnixTimeStamp;
            let current = fs.symlink_metadata(path).await.ok();
            let atime = meta
                .atime
                .and_then(|t| UnixTimeStamp::new(t).ok())
                .or_else(|| current.as_ref().and_then(|m| m.accessed()))
                .unwrap_or(UnixTimeStamp::unix_epoch());
            let mtime = meta
                .mtime
                .and_then(|t| UnixTimeStamp::new(t).ok())
                .or_else(|| current.as_ref().and_then(|m| m.modified()))
                .unwrap_or(UnixTimeStamp::unix_epoch());
            sftp_meta.time(atime, mtime);
        }

        fs.set_metadata(path, sftp_meta.create()).await?;

        Ok(())
    }
}

impl SftpVfs {
    async fn read_range_inner(
        &self,
        sftp: &Sftp,
        path: &Path,
        offset: u64,
        length: u64,
    ) -> Result<FileChunk, Error> {
        use tokio::io::AsyncSeekExt;

        let mut fs = sftp.fs();
        let meta = fs.metadata(path).await?;
        let total_size = meta.len().unwrap_or(0);

        let to_read = length.min(total_size.saturating_sub(offset)) as usize;
        if to_read == 0 {
            return Ok(FileChunk {
                data: Vec::new(),
                offset,
                total_size,
            });
        }

        let mut file = sftp.open(path).await?;

        // Seek to offset — File implements AsyncSeek by adjusting the internal offset
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
        }

        const CHUNK_SIZE: u32 = 65536;
        let mut data = Vec::with_capacity(to_read);
        while data.len() < to_read {
            let remaining = (to_read - data.len()).min(CHUNK_SIZE as usize) as u32;
            let buf = bytes::BytesMut::new();
            let chunk = file.read(remaining, buf).await?;
            match chunk {
                Some(bytes) => data.extend_from_slice(&bytes),
                None => break,
            }
        }
        data.truncate(to_read);

        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }
}

// ---------------------------------------------------------------------------
// SftpAsyncWriter
// ---------------------------------------------------------------------------

struct SftpAsyncWriter {
    file: openssh_sftp_client::file::File,
    notifier: VfsChangeNotifier,
    path: PathBuf,
}

#[async_trait::async_trait]
impl VfsAsyncWriter for SftpAsyncWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.file.write_all(buf).await?;
        Ok(buf.len())
    }

    async fn finish(self: Box<Self>) -> Result<(), Error> {
        self.file.close().await?;
        self.notifier.notify(&self.path);
        Ok(())
    }
}
