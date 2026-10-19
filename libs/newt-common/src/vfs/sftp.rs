use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use bytes::Buf;
use futures::StreamExt;
use log::{debug, error, info, warn};
use openssh_sftp_client::Sftp;
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, oneshot};

use crate::proc::NoConsoleWindow;
use crate::vfs::FileChunk;
use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{File, FsStats, Mode, Symlink, UserGroup};
use crate::{Error, ErrorKind};

impl From<openssh_sftp_client::Error> for Error {
    fn from(e: openssh_sftp_client::Error) -> Self {
        use openssh_sftp_client::Error as SftpErr;
        use openssh_sftp_protocol_error::ErrorCode;

        let kind = match &e {
            SftpErr::SftpError(ErrorCode::NoSuchFile, _) => ErrorKind::NotFound,
            SftpErr::SftpError(ErrorCode::PermDenied, _) => ErrorKind::PermissionDenied,
            SftpErr::SftpError(ErrorCode::OpUnsupported, _) => ErrorKind::NotSupported,
            SftpErr::IOError(io_err) if io_err.kind() == std::io::ErrorKind::UnexpectedEof => {
                ErrorKind::Connection
            }
            SftpErr::IOError(_) | SftpErr::AwaitableError(_) | SftpErr::TaskJoinError(_) => {
                ErrorKind::Connection
            }
            _ => ErrorKind::Other,
        };

        Self {
            kind,
            message: e.to_string(),
        }
    }
}

/// The sftp client speaks `std::path`. Remote is Unix, so our canonical
/// wire string *is* the correct remote path.
fn sftp_path(p: &Path) -> std::path::PathBuf {
    std::path::PathBuf::from(p.as_wire_str())
}

use super::{
    Breadcrumb, DisplayPathMatch, MountRequest, RegisteredDescriptor, VFS_READ_CHUNK_SIZE, Vfs,
    VfsAsyncWriter, VfsChangeNotifier, VfsDescriptor, VfsMetadata, VfsRandomReader,
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
    fn metadata_traits(&self, _mount_meta: &[u8]) -> super::MetadataTraits {
        super::MetadataTraits {
            unix_owner: true,
            windows_attributes: false,
        }
    }
    fn auto_mount_request(&self) -> Option<MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read(&self) -> bool {
        true
    }
    fn can_overwrite(&self) -> bool {
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
    fn can_write_range(&self) -> bool {
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
    fn can_stat_directories(&self) -> bool {
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
    fn auto_refresh(&self) -> bool {
        false
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        let host = String::from_utf8_lossy(mount_meta);
        format!("sftp://{}{}", host, super::unix_display_path(path))
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        let host = String::from_utf8_lossy(mount_meta);
        let mut crumbs = super::unix_breadcrumbs(path);
        // Rewrite the root crumb to show the SFTP host prefix.
        if let Some(root) = crumbs.first_mut() {
            root.label = format!("sftp://{}/", host);
        }
        crumbs
    }

    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        let rest = input.strip_prefix("sftp://")?;
        let host = String::from_utf8_lossy(mount_meta);
        let after_host = rest.strip_prefix(host.as_ref())?;
        if !after_host.is_empty() && !after_host.starts_with('/') {
            return None;
        }
        Some(DisplayPathMatch::exact(PathBuf::from_wire_str(after_host)))
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
    sftp: Sftp,
    host: String,
    notifier: VfsChangeNotifier,
    /// Set by the death-watcher task when the ssh process exits. Operations
    /// short-circuit with a clear connection error instead of hanging on
    /// the next request.
    dead: Arc<AtomicBool>,
    /// Sent on `Drop` to ask the death-watcher to kill the ssh process.
    kill_tx: Option<oneshot::Sender<()>>,
    /// Askpass listener kept alive for the lifetime of the ssh process so
    /// that resumed prompts (e.g. ServerAliveInterval reauth) keep working.
    /// `None` when no askpass was configured.
    _askpass_listener: Option<crate::askpass::AskpassListener>,
}

/// Timeout for the SSH connection + SFTP handshake.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Number of concurrent symlink-target metadata fetches during list_files.
const SYMLINK_RESOLVE_CONCURRENCY: usize = 16;

/// Number of files per streaming batch sent to the UI from list_files.
const LIST_BATCH_SIZE: usize = 200;

impl SftpVfs {
    /// Build an `SftpVfs` from a `MountRequest::Sftp` payload, picking
    /// up the askpass binary/provider from `ctx`.
    pub async fn mount(
        host: String,
        ctx: &crate::vfs::mount::MountContext<'_>,
    ) -> Result<std::sync::Arc<dyn Vfs>, Error> {
        info!("mounting SFTP VFS for host={}", host);
        let vfs = Self::connect(&host, ctx.sftp_askpass.cloned()).await?;
        Ok(std::sync::Arc::new(vfs))
    }

    pub async fn connect(
        host: &str,
        askpass: Option<crate::vfs::mount::SftpAskpass>,
    ) -> Result<Self, Error> {
        info!("sftp: connecting to {}", host);

        match tokio::time::timeout(CONNECT_TIMEOUT, Self::connect_inner(host, askpass)).await {
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

    async fn connect_inner(
        host: &str,
        askpass: Option<crate::vfs::mount::SftpAskpass>,
    ) -> Result<Self, Error> {
        // Application-level keepalive prevents idle TCP connections from being
        // silently killed by NAT / firewalls / load balancers between us and
        // the server (typical idle window: 60–300s on home routers, much
        // shorter on cellular). Three failed probes 30s apart = ~90s before we
        // consider the connection dead.
        let mut cmd = tokio::process::Command::new("ssh");
        cmd.no_console_window();
        cmd.arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            // `-s` is a flag: the subsystem name goes where the remote
            // command does, after the host. `--` keeps the host from being
            // read as an option (`-oProxyCommand=…`).
            .arg("-s")
            .arg("--")
            .arg(host)
            .arg("sftp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // If an askpass config was provided, spawn a listener and route
        // password / host-key prompts through it. Whether the listener forwards
        // to the host UI directly (host session) or to the host UI via a
        // reverse RPC (agent session) is up to the callback.
        let askpass_listener = match askpass {
            Some(cfg) => {
                let listener =
                    crate::askpass::AskpassListener::spawn(cfg.provider).map_err(|e| Error {
                        kind: ErrorKind::Connection,
                        message: format!("Failed to start askpass listener: {}", e),
                    })?;
                cmd.env("SSH_ASKPASS", &cfg.askpass_binary)
                    .env("SSH_ASKPASS_REQUIRE", "force")
                    .env("NEWT_ASKPASS_SOCK", &listener.socket_path);
                Some(listener)
            }
            None => None,
        };

        let mut child = cmd.spawn().map_err(|e| {
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

        // We always spawn the child with piped stdio just above, so these
        // takes are infallible — but use structured errors anyway so a
        // future caller that forgets the piped() doesn't panic.
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::custom("sftp: child stdin not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::custom("sftp: child stdout not piped"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::custom("sftp: child stderr not piped"))?;

        // Race the SFTP handshake against the ssh process exiting.
        // If ssh exits first (auth failure, bad host, etc.), we read stderr
        // and produce a clear error. If the handshake succeeds, we start
        // background tasks to drain stderr and watch for the ssh process
        // exiting unexpectedly.
        let handshake = Sftp::new(stdin, stdout, openssh_sftp_client::SftpOptions::default());

        tokio::select! {
            result = handshake => {
                match result {
                    Ok(sftp) => {
                        info!("sftp: connected to {}", host);

                        let dead = Arc::new(AtomicBool::new(false));
                        let (kill_tx, kill_rx) = oneshot::channel::<()>();

                        // Stderr drainer
                        let host_owned = host.to_string();
                        tokio::spawn(async move {
                            use tokio::io::{AsyncBufReadExt, BufReader};
                            let mut lines = BufReader::new(stderr).lines();
                            while let Ok(Some(line)) = lines.next_line().await {
                                warn!("sftp [{}] stderr: {}", host_owned, line);
                            }
                        });

                        // Death watcher: races the child exiting against a
                        // kill signal from `Drop`. Either way it sets `dead`,
                        // so subsequent operations short-circuit.
                        let host_owned = host.to_string();
                        let dead_for_watcher = dead.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                status = child.wait() => {
                                    match status {
                                        Ok(s) => warn!(
                                            "sftp [{}] ssh exited unexpectedly: {}",
                                            host_owned, s
                                        ),
                                        Err(e) => warn!(
                                            "sftp [{}] failed to wait on ssh: {}",
                                            host_owned, e
                                        ),
                                    }
                                }
                                _ = kill_rx => {
                                    let _ = child.start_kill();
                                    let _ = child.wait().await;
                                    debug!("sftp [{}] ssh terminated on unmount", host_owned);
                                }
                            }
                            dead_for_watcher.store(true, Ordering::SeqCst);
                        });

                        Ok(Self {
                            sftp,
                            host: host.to_string(),
                            notifier: VfsChangeNotifier::new(),
                            dead,
                            kill_tx: Some(kill_tx),
                            _askpass_listener: askpass_listener,
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

    /// Returns `Err(ErrorKind::Connection)` if the ssh process has exited.
    /// Called at the start of each operation so subsequent calls fail fast
    /// instead of hanging on the SFTP client's internal queues.
    fn check_alive(&self) -> Result<(), Error> {
        if self.dead.load(Ordering::Relaxed) {
            Err(Error {
                kind: ErrorKind::Connection,
                message: format!("SFTP connection to '{}' has been lost", self.host),
            })
        } else {
            Ok(())
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
        // Signal the death-watcher task to kill the ssh process. If the
        // watcher is already gone (ssh exited on its own), the send fails
        // silently — there's nothing to clean up.
        if let Some(tx) = self.kill_tx.take() {
            let _ = tx.send(());
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

/// A link as the server's `readlink` reports it; `Unknown` when it refuses.
async fn read_symlink(sftp: &Sftp, path: &std::path::Path) -> Symlink {
    Symlink::from_read(
        sftp.fs()
            .read_link(path)
            .await
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
    )
}

/// `symlink` is the link's target read, for an entry `meta` says is one.
fn metadata_to_file(
    name: String,
    meta: &openssh_sftp_client::metadata::MetaData,
    follow_meta: Option<&openssh_sftp_client::metadata::MetaData>,
    symlink: Option<Symlink>,
) -> File {
    let is_symlink = meta.file_type().is_some_and(|ft| ft.is_symlink());

    let effective_meta = if is_symlink {
        follow_meta.unwrap_or(meta)
    } else {
        meta
    };
    let is_dir = effective_meta.file_type().is_some_and(|ft| ft.is_dir());
    let size = if is_dir { None } else { meta.len() };

    let mode = meta.permissions().map(|p| Mode(permissions_to_mode(&p)));

    File {
        attributes: None,
        is_hidden: name.starts_with('.'),
        name,
        size,
        allocated_size: None,
        device_id: None,
        inode: None,
        hard_links: None,
        is_dir,
        symlink,
        user: meta.uid().map(UserGroup::Id),
        group: meta.gid().map(UserGroup::Id),
        mode,
        modified: meta
            .modified()
            .map(|t| sftp_time_to_system_time(t).to_unix()),
        accessed: meta
            .accessed()
            .map(|t| sftp_time_to_system_time(t).to_unix()),
        created: None,
        key: None,
        source: None,
    }
}

use crate::vfs::ToUnix;

impl SftpVfs {
    /// A `replace` write: a temporary file beside the target, given its
    /// mode, renamed over it with posix-rename on `finish`. Refused
    /// without posix-rename (a plain rename won't replace), for anything
    /// but a regular file, and when the new file would have another owner.
    /// SFTP reports no link count, so a hard-linked file is split off.
    async fn replacement_writer(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        if !self.sftp.support_posix_rename() {
            return Err(Error::not_supported());
        }
        let mut fs = self.sftp.fs();
        let existing = match fs.symlink_metadata(sftp_path(path)).await {
            Ok(_) => {
                let target = fs
                    .canonicalize(sftp_path(path))
                    .await
                    .map_err(|_| Error::not_supported())?;
                let meta = fs.metadata(&target).await?;
                if !meta.file_type().is_some_and(|ft| ft.is_file()) {
                    return Err(Error::not_supported());
                }
                Some((meta, target))
            }
            Err(e) => match Error::from(e) {
                e if e.kind == ErrorKind::NotFound => None,
                e => return Err(e),
            },
        };
        let target = existing
            .as_ref()
            .map_or_else(|| sftp_path(path), |(_, target)| target.clone());
        let dir = target
            .parent()
            .map(std::path::Path::to_owned)
            .unwrap_or_default();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = dir.join(format!(".newt-{}-{nanos}", std::process::id()));
        let mut file = self
            .sftp
            .options()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await
            .map_err(|_| Error::not_supported())?;
        let replacement = Replacement {
            temp,
            target,
            fs,
            finished: false,
        };
        if let Some((meta, _)) = &existing {
            let created = file.metadata().await?;
            if created.uid() != meta.uid() || created.gid() != meta.gid() {
                drop(replacement);
                return Err(Error::not_supported());
            }
            if let Some(permissions) = meta.permissions() {
                file.set_permissions(permissions).await?;
            }
        }
        Ok(Box::new(SftpAsyncWriter {
            file,
            notifier: self.notifier.clone(),
            path: path.to_owned(),
            replacement: Some(replacement),
        }))
    }
}

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
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::VfsFileList, Error> {
        debug!("sftp: list_files {}", path);
        self.check_alive()?;

        let dir = self.sftp.fs().open_dir(sftp_path(path)).await?;

        // Collect raw entries first so we can resolve symlink targets
        // concurrently below. read_dir itself batches multiple entries per
        // SSH_FXP_READDIR round-trip, so this isn't a streaming loss.
        let mut raw_entries = Vec::new();
        {
            let read_dir = dir.read_dir();
            tokio::pin!(read_dir);
            while let Some(entry) = read_dir.next().await {
                let entry = entry?;
                let name = entry.filename().to_string_lossy().to_string();
                if name == "." || name == ".." {
                    continue;
                }
                if !super::path::is_valid_name(&name) {
                    log::warn!("sftp: skipping an entry of {path} named {name:?}");
                    continue;
                }
                raw_entries.push((name, entry.metadata()));
            }
        }

        let path_owned = path.to_owned();
        let sftp = &self.sftp;

        // Resolve symlink targets in parallel (capped). Order is preserved
        // by `buffered`, so the resulting file list is in the same order as
        // the directory entries arrived.
        let resolve_stream =
            futures::stream::iter(raw_entries.into_iter().map(move |(name, meta)| {
                let target_path = sftp_path(&path_owned.join(&name));
                async move {
                    let is_symlink = meta.file_type().is_some_and(|ft| ft.is_symlink());
                    let (follow_meta, symlink) = if is_symlink {
                        let (follow, link) = futures::join!(
                            async { sftp.fs().metadata(&target_path).await.ok() },
                            read_symlink(sftp, &target_path),
                        );
                        (follow, Some(link))
                    } else {
                        (None, None)
                    };
                    metadata_to_file(name, &meta, follow_meta.as_ref(), symlink)
                }
            }))
            .buffered(SYMLINK_RESOLVE_CONCURRENCY);
        tokio::pin!(resolve_stream);

        let mut files = Vec::new();
        let mut batch = Vec::new();

        // ".." goes first and is included in the first batch so the UI
        // shows it immediately.
        if !path.is_root() {
            let dotdot = File::parent_dir();
            files.push(dotdot.clone());
            batch.push(dotdot);
        }

        while let Some(file) = resolve_stream.next().await {
            files.push(file.clone());
            batch.push(file);
            if batch.len() >= LIST_BATCH_SIZE {
                if let Some(ref tx) = batch_tx {
                    if tx.send(std::mem::take(&mut batch)).await.is_err() {
                        // Receiver dropped — caller cancelled the listing.
                        return Ok(files.into());
                    }
                } else {
                    batch.clear();
                }
            }
        }

        if let Some(ref tx) = batch_tx
            && !batch.is_empty()
        {
            let _ = tx.send(batch).await;
        }

        Ok(files.into())
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        self.notifier.watch(path).await;
        Ok(())
    }

    async fn file_info(&self, path: &Path, follow: bool) -> Result<File, Error> {
        debug!("sftp: file_info {} (follow: {})", path, follow);
        self.check_alive()?;
        let link_path = sftp_path(path);
        let name = path.file_name().unwrap_or_default().to_string();

        if follow {
            let meta = self.sftp.fs().metadata(&link_path).await?;
            return Ok(metadata_to_file(name, &meta, None, None));
        }

        let symlink_meta = self.sftp.fs().symlink_metadata(&link_path).await?;
        let is_symlink = symlink_meta.file_type().is_some_and(|ft| ft.is_symlink());
        let (follow_meta, symlink) = if is_symlink {
            let (follow, link) = futures::join!(
                async { self.sftp.fs().metadata(&link_path).await.ok() },
                read_symlink(&self.sftp, &link_path),
            );
            (follow, Some(link))
        } else {
            (None, None)
        };

        Ok(metadata_to_file(
            name,
            &symlink_meta,
            follow_meta.as_ref(),
            symlink,
        ))
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        debug!(
            "sftp: read_range {} offset={} length={}",
            path, offset, length
        );
        self.check_alive()?;

        let meta = self.sftp.fs().metadata(sftp_path(path)).await?;
        let total_size = meta.len().unwrap_or(0);
        let mut file = self.sftp.open(sftp_path(path)).await?;

        Self::read_range_from_file(&mut file, offset, length, total_size).await
    }

    async fn open_read_at(&self, path: &Path) -> Result<Box<dyn VfsRandomReader>, Error> {
        debug!("sftp: open_read_at {}", path);
        self.check_alive()?;

        let meta = self.sftp.fs().metadata(sftp_path(path)).await?;
        let total_size = meta.len().unwrap_or(0);
        let file = self.sftp.open(sftp_path(path)).await?;

        Ok(Box::new(SftpRandomReader { file, total_size }))
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        debug!("sftp: open_read_async {}", path);
        self.check_alive()?;

        let file = self.sftp.open(sftp_path(path)).await?;

        // Bridge the !Unpin SFTP file into AsyncRead via a channel + task.
        let (tx, rx) = mpsc::channel::<Result<bytes::Bytes, Error>>(4);
        tokio::spawn(async move {
            let mut file = file;
            const CHUNK_SIZE: u32 = VFS_READ_CHUNK_SIZE as u32;
            loop {
                match file.read(CHUNK_SIZE, bytes::BytesMut::new()).await {
                    Ok(Some(data)) if !data.is_empty() => {
                        if tx.send(Ok(data.freeze())).await.is_err() {
                            break; // receiver dropped
                        }
                    }
                    Ok(_) => break, // EOF
                    Err(e) => {
                        let _ = tx.send(Err(e.into())).await;
                        break;
                    }
                }
            }
        });

        Ok(Box::new(SftpAsyncReader {
            rx,
            current_chunk: bytes::Bytes::new(),
        }))
    }

    async fn overwrite_async(
        &self,
        path: &Path,
        options: &super::attributes::WriteOptions,
    ) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        debug!("sftp: overwrite_async {}", path);
        let replace_only = {
            let mut others = options.clone();
            others.replace = false;
            options.replace && others.is_default()
        };
        if replace_only {
            self.check_alive()?;
            return self.replacement_writer(path).await;
        }
        if !options.is_default() {
            return Err(Error::not_supported());
        }
        self.check_alive()?;
        let file = self
            .sftp
            .options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(sftp_path(path))
            .await?;

        Ok(Box::new(SftpAsyncWriter {
            file,
            notifier: self.notifier.clone(),
            path: path.to_owned(),
            replacement: None,
        }))
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: create_directory {}", path);
        self.check_alive()?;
        self.sftp.fs().create_dir(sftp_path(path)).await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn create_symlink(&self, link: &Path, target: &str) -> Result<(), Error> {
        debug!("sftp: create_symlink {} -> {}", link, target);
        self.check_alive()?;
        self.sftp
            .fs()
            .symlink(std::path::PathBuf::from(target), sftp_path(link))
            .await?;
        self.notifier.notify(link);
        Ok(())
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: remove_file {}", path);
        self.check_alive()?;
        self.sftp.fs().remove_file(sftp_path(path)).await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: remove_dir {}", path);
        self.check_alive()?;
        self.sftp.fs().remove_dir(sftp_path(path)).await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn same_file(&self, a: &Path, b: &Path) -> Result<bool, Error> {
        self.check_alive()?;
        // SFTP exposes no inode, so identity comes from the server's own
        // `realpath`: it folds case the way that server's filesystem does
        // (an SFTP server on Windows or a case-insensitive macOS volume is
        // exactly the case we're here for). `realpath` also sees through
        // symlinks, which would make a link the same file as its target,
        // so a link is only ever the same as itself: same directory, same
        // name.
        let mut fs = self.sftp.fs();
        let is_link = |m: openssh_sftp_client::metadata::MetaData| {
            m.file_type().is_some_and(|ft| ft.is_symlink())
        };
        let (Ok(meta_a), Ok(meta_b)) = (
            fs.symlink_metadata(sftp_path(a)).await,
            fs.symlink_metadata(sftp_path(b)).await,
        ) else {
            // A path that can't be stat'ed doesn't exist (or the server
            // won't say) — nothing's twin either way.
            return Ok(false);
        };
        if is_link(meta_a) || is_link(meta_b) {
            if a.file_name() != b.file_name() {
                return Ok(false);
            }
            let (Some(dir_a), Some(dir_b)) = (a.parent(), b.parent()) else {
                return Ok(false);
            };
            let (Ok(dir_a), Ok(dir_b)) = (
                fs.canonicalize(sftp_path(dir_a)).await,
                fs.canonicalize(sftp_path(dir_b)).await,
            ) else {
                return Ok(false);
            };
            return Ok(dir_a == dir_b);
        }
        let (Ok(a), Ok(b)) = (
            fs.canonicalize(sftp_path(a)).await,
            fs.canonicalize(sftp_path(b)).await,
        ) else {
            return Ok(false);
        };
        Ok(a == b)
    }

    /// Whether a server refuses an existing destination depends on the
    /// server, so `no_replace` is not offered.
    async fn rename(
        &self,
        from: &Path,
        to: &Path,
        options: &super::attributes::RenameOptions,
    ) -> Result<(), Error> {
        if options.no_replace {
            return Err(Error::not_supported());
        }
        debug!("sftp: rename {} -> {}", from, to);
        self.check_alive()?;
        let mut fs = self.sftp.fs();
        if let Err(e) = fs.rename(sftp_path(from), sftp_path(to)).await {
            let mut e = Error::from(e);
            // v3 has no "exists" status: a server without posix-rename
            // refuses an existing target with a bare failure.
            if e.kind == ErrorKind::Other && fs.symlink_metadata(sftp_path(to)).await.is_ok() {
                e.kind = ErrorKind::AlreadyExists;
            }
            return Err(e);
        }
        self.notifier.notify(from);
        self.notifier.notify(to);
        Ok(())
    }

    async fn hard_link(&self, link: &Path, target: &Path) -> Result<(), Error> {
        debug!("sftp: hard_link {} -> {}", link, target);
        self.check_alive()?;
        self.sftp
            .fs()
            .hard_link(sftp_path(target), sftp_path(link))
            .await?;
        self.notifier.notify(link);
        Ok(())
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: touch {}", path);
        self.check_alive()?;
        let file = self
            .sftp
            .options()
            .create(true)
            .write(true)
            .open(sftp_path(path))
            .await?;
        file.close().await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn truncate(&self, path: &Path) -> Result<(), Error> {
        debug!("sftp: truncate {}", path);
        self.check_alive()?;
        let file = self
            .sftp
            .options()
            .write(true)
            .truncate(true)
            .open(sftp_path(path))
            .await?;
        file.close().await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn write_range(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), Error> {
        use tokio::io::AsyncSeekExt;
        debug!(
            "sftp: write_range {} at {} ({} bytes)",
            path,
            offset,
            data.len()
        );
        self.check_alive()?;
        let mut file = self
            .sftp
            .options()
            .write(true)
            .open(sftp_path(path))
            .await?;
        let len = file.metadata().await?.len().unwrap_or(0);
        if offset.saturating_add(data.len() as u64) > len {
            file.close().await?;
            return Err(Error::custom("write_range past the end of the file"));
        }
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(data).await?;
        file.close().await?;
        self.notifier.notify(path);
        Ok(())
    }

    async fn resolve_link(&self, path: &Path) -> Result<PathBuf, Error> {
        let target = self.sftp.fs().canonicalize(sftp_path(path)).await?;
        Ok(PathBuf::from_wire_str(&target.to_string_lossy()))
    }

    async fn get_metadata(&self, path: &Path) -> Result<VfsMetadata, Error> {
        debug!("sftp: get_metadata {}", path);
        self.check_alive()?;
        let meta = self.sftp.fs().symlink_metadata(sftp_path(path)).await?;

        Ok(VfsMetadata {
            permissions: meta.permissions().map(|p| permissions_to_mode(&p)),
            uid: meta.uid(),
            gid: meta.gid(),
            atime: meta.accessed().map(sftp_time_to_system_time),
            mtime: meta.modified().map(sftp_time_to_system_time),
        })
    }

    async fn set_metadata(&self, path: &Path, meta: &VfsMetadata) -> Result<(), Error> {
        debug!("sftp: set_metadata {}", path);
        self.check_alive()?;
        let mut fs = self.sftp.fs();

        if fs
            .symlink_metadata(sftp_path(path))
            .await?
            .file_type()
            .is_some_and(|ft| ft.is_symlink())
        {
            return Err(Error::not_supported());
        }
        let mut sftp_meta = openssh_sftp_client::metadata::MetaDataBuilder::new();

        if let Some(permissions) = meta.permissions {
            sftp_meta.permissions(mode_to_permissions(permissions));
        }

        if meta.uid.is_some() || meta.gid.is_some() {
            let current = fs.symlink_metadata(sftp_path(path)).await.ok();
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
            let current = fs.symlink_metadata(sftp_path(path)).await.ok();
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

        fs.set_metadata(sftp_path(path), sftp_meta.create()).await?;
        self.notifier.notify(path);

        Ok(())
    }
}

impl SftpVfs {
    /// Read a range from an already-opened file handle. Does not require the
    /// SFTP mutex since file handles are independent after opening.
    async fn read_range_from_file(
        file: &mut openssh_sftp_client::file::File,
        offset: u64,
        length: u64,
        total_size: u64,
    ) -> Result<FileChunk, Error> {
        use tokio::io::AsyncSeekExt;

        let to_read = length.min(total_size.saturating_sub(offset)) as usize;
        if to_read == 0 {
            return Ok(FileChunk {
                data: Vec::new(),
                offset,
                total_size,
            });
        }

        // Seek is client-side bookkeeping (SFTP reads carry explicit
        // offsets), so always repositioning is free — and required when the
        // handle is shared across calls (`SftpRandomReader`).
        file.seek(std::io::SeekFrom::Start(offset)).await?;

        const CHUNK_SIZE: u32 = VFS_READ_CHUNK_SIZE as u32;
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
// SftpRandomReader — positioned reads on one held-open SFTP handle
// ---------------------------------------------------------------------------

/// `total_size` is pinned at open — the handle reads the file as it was
/// when the session started, and clamping against a stale size is the
/// caller-visible consequence of that.
struct SftpRandomReader {
    file: openssh_sftp_client::file::File,
    total_size: u64,
}

#[async_trait::async_trait]
impl VfsRandomReader for SftpRandomReader {
    async fn read_at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>, Error> {
        let chunk =
            SftpVfs::read_range_from_file(&mut self.file, offset, len, self.total_size).await?;
        Ok(chunk.data)
    }
}

// ---------------------------------------------------------------------------
// SftpAsyncReader — bridges the !Unpin SFTP File into AsyncRead via a channel
// ---------------------------------------------------------------------------

struct SftpAsyncReader {
    rx: mpsc::Receiver<Result<bytes::Bytes, Error>>,
    current_chunk: bytes::Bytes,
}

impl tokio::io::AsyncRead for SftpAsyncReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // Serve from the current buffered chunk first.
        if !self.current_chunk.is_empty() {
            let n = self.current_chunk.len().min(buf.remaining());
            buf.put_slice(&self.current_chunk[..n]);
            self.current_chunk.advance(n);
            return std::task::Poll::Ready(Ok(()));
        }

        match self.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(Ok(chunk))) => {
                let n = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.current_chunk = chunk.slice(n..);
                }
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(Some(Err(e))) => std::task::Poll::Ready(Err(e.into())),
            std::task::Poll::Ready(None) => {
                // Channel closed — EOF.
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// SftpAsyncWriter
// ---------------------------------------------------------------------------

struct SftpAsyncWriter {
    file: openssh_sftp_client::file::File,
    notifier: VfsChangeNotifier,
    path: PathBuf,
    replacement: Option<Replacement>,
}

/// The temporary file a `replace` write goes to, renamed over `target` on
/// `finish` and removed if the write ends any other way.
struct Replacement {
    temp: std::path::PathBuf,
    target: std::path::PathBuf,
    fs: openssh_sftp_client::fs::Fs,
    finished: bool,
}

impl Drop for Replacement {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut fs = self.fs.clone();
        let temp = std::mem::take(&mut self.temp);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = fs.remove_file(temp).await;
            });
        }
    }
}

#[async_trait::async_trait]
impl VfsAsyncWriter for SftpAsyncWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.file.write_all(buf).await?;
        Ok(buf.len())
    }

    async fn finish(self: Box<Self>) -> Result<(), Error> {
        let SftpAsyncWriter {
            file,
            notifier,
            path,
            replacement,
        } = *self;
        file.close().await?;
        if let Some(mut replacement) = replacement {
            replacement
                .fs
                .rename(&replacement.temp, &replacement.target)
                .await?;
            replacement.finished = true;
        }
        notifier.notify(&path);
        Ok(())
    }
}
