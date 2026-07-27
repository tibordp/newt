//! The mount subsystem: the mount/unmount/remount wire types, the
//! `VfsManager` trait with its local (`VfsRegistryManager`) and RPC-proxy
//! (`VfsManagerRemote`) implementations, and the `MountContext` dependency
//! bundle handed to per-VFS `mount` helpers.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::api::PendingVfsReadStreams;
use crate::filesystem::Filesystem;
use crate::rpc::Communicator;

use super::path::VfsPath;
use super::s3::S3Credentials;
use super::{
    NoopProgressSink, ProgressReporter, ScopedReporter, Vfs, VfsDescriptor, VfsId, VfsProgressSink,
    VfsRegistry, is_archive_name, is_disc_image_name, search,
};

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub enum MountRequest {
    S3 {
        region: Option<String>,
        /// When set, the VFS is scoped to this bucket (root = bucket contents).
        /// When None, root lists all buckets.
        bucket: Option<String>,
        #[serde(default)]
        credentials: S3Credentials,
    },
    Sftp {
        host: String,
    },
    Archive {
        origin: VfsPath,
    },
    /// Browse into an ISO 9660 / UDF disc image file.
    Disc {
        origin: VfsPath,
    },
    Search {
        root: VfsPath,
        params: search::SearchParams,
    },
    /// Expose the requesting side's own filesystem (the client-local FS
    /// in a remote session). `mount_meta` is supplied by the requester —
    /// only the owner can describe its FS (style, drive roots), and the
    /// resulting `RemoteVfs` can't derive it (its `mount_meta()` is sync,
    /// no RPC). Refreshed via `VfsManager::remount` on drive changes.
    Remote {
        mount_meta: Vec<u8>,
    },
    /// Spawn an FS-only sub-agent over a transport (SSH, docker, …) and
    /// mount its local filesystem. See `vfs::agent`.
    Agent {
        spec: crate::connect::SpawnSpec,
        /// Transport kind shown as the VFS display name, e.g. `Docker`.
        kind: String,
        /// Mount target shown as the VFS label, e.g. the container name.
        label: String,
    },
}

/// The mount request for entering a file entry as a browsable VFS
/// (archives, disc images), or `None` when the name isn't enterable.
pub fn enterable_mount_request(name: &str, origin: VfsPath) -> Option<MountRequest> {
    if is_archive_name(name) {
        Some(MountRequest::Archive { origin })
    } else if is_disc_image_name(name) {
        Some(MountRequest::Disc { origin })
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountResponse {
    pub vfs_id: VfsId,
    pub type_name: String,
    pub mount_meta: Vec<u8>,
    pub origin: Option<VfsPath>,
}

/// Client-side descriptor + metadata for a mounted VFS.
pub struct MountedVfsInfo {
    pub vfs_id: VfsId,
    pub descriptor: &'static dyn VfsDescriptor,
    pub mount_meta: Vec<u8>,
    pub origin: Option<VfsPath>,
}

#[async_trait::async_trait]
pub trait VfsManager: Send + Sync {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error>;
    async fn unmount(&self, vfs_id: VfsId) -> Result<(), Error>;

    /// Logical remount: refresh a mount's `mount_meta` in place, keeping
    /// its identity (`VfsId`, descriptor, origin). Returns the new meta.
    ///
    /// `mount_meta: None` asks the VFS to re-derive it (revalidate where
    /// supported, then a fresh `Vfs::mount_meta()` — for a local FS that
    /// re-enumerates drive roots). `Some` injects owner-supplied meta and
    /// is only valid for `MountRequest::Remote` mounts, whose meta the
    /// requesting side owns (see the variant docs).
    async fn remount(&self, vfs_id: VfsId, mount_meta: Option<Vec<u8>>) -> Result<Vec<u8>, Error>;
}

pub struct VfsManagerRemote {
    communicator: Communicator,
}

impl VfsManagerRemote {
    pub fn new(communicator: Communicator) -> Self {
        Self { communicator }
    }
}

#[async_trait::async_trait]
impl VfsManager for VfsManagerRemote {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error> {
        let ret: Result<MountResponse, Error> = self
            .communicator
            .invoke(crate::api::API_MOUNT_VFS, &request)
            .await?;
        Ok(ret?)
    }

    async fn unmount(&self, vfs_id: VfsId) -> Result<(), Error> {
        let ret: Result<(), Error> = self
            .communicator
            .invoke(crate::api::API_UNMOUNT_VFS, &vfs_id)
            .await?;
        Ok(ret?)
    }

    async fn remount(&self, vfs_id: VfsId, mount_meta: Option<Vec<u8>>) -> Result<Vec<u8>, Error> {
        let ret: Result<Vec<u8>, Error> = self
            .communicator
            .invoke(crate::api::API_REMOUNT_VFS, &(vfs_id, mount_meta))
            .await?;
        Ok(ret?)
    }
}

/// Shared state passed to per-VFS `mount` helpers. Bundles the registry
/// (needed by archive mounts to resolve their upstream), the host
/// communicator and pending-stream map (needed by the Remote VFS), the
/// SFTP askpass configuration (binary path + provider, used by SFTP),
/// and a generic askpass provider (used by encrypted-archive mounts).
/// Any VFS may ignore fields it doesn't need.
pub struct MountContext<'a> {
    pub registry: &'a VfsRegistry,
    pub host_communicator: &'a std::sync::OnceLock<Communicator>,
    pub pending_read_streams: &'a PendingVfsReadStreams,
    pub sftp_askpass: Option<&'a SftpAskpass>,
    pub askpass_provider: Option<&'a Arc<dyn crate::askpass::AskpassProvider>>,
    /// Resolves agent binaries for spawn-style agent mounts (`None` ⇒
    /// such mounts are rejected).
    pub agent_resolver: Option<&'a Arc<dyn crate::agent_resolver::AgentResolver>>,
    /// Extra PATH entries for transport binary resolution on agent mounts.
    pub extra_path: &'a [String],
    /// Per-mount progress reporter, scoped to the `VfsId` the manager
    /// is about to assign to this mount. VFSes that report progress
    /// (e.g. SearchVfs) clone the inner `Arc` and call `report()`
    /// without ever needing to know their own id.
    pub progress_reporter: &'a Arc<dyn ProgressReporter>,
}

/// Askpass configuration used by SFTP (and any future SSH-spawning VFS).
#[derive(Clone)]
pub struct SftpAskpass {
    /// Path to the agent binary to set as `SSH_ASKPASS` (its
    /// `NEWT_ASKPASS_SOCK` mode connects to the listener spawned for
    /// `provider`).
    pub askpass_binary: std::path::PathBuf,
    pub provider: Arc<dyn crate::askpass::AskpassProvider>,
}

pub struct VfsRegistryManager {
    registry: Arc<VfsRegistry>,
    /// When set, allows mounting a Remote VFS that proxies calls back to
    /// the host. Used by the agent in remote sessions.
    host_communicator: Arc<std::sync::OnceLock<Communicator>>,
    /// Shared map for routing read-chunk notifications to the correct stream.
    pending_read_streams: PendingVfsReadStreams,
    /// SFTP askpass configuration. When `None`, SFTP mounts inherit the
    /// process environment with no special password handling.
    sftp_askpass: Option<SftpAskpass>,
    /// Generic askpass provider used for prompts that aren't tied to
    /// SFTP's `SSH_ASKPASS` plumbing — currently encrypted-archive
    /// passwords. When set with `with_sftp_askpass`, this is also
    /// populated from the SFTP askpass's provider.
    askpass_provider: Option<Arc<dyn crate::askpass::AskpassProvider>>,
    /// Sink used to build a per-mount `ScopedReporter`. Defaults to a
    /// no-op so manager construction outside of a real session (tests,
    /// agent boot before the outbox is wired, etc.) keeps working.
    progress_sink: Arc<dyn VfsProgressSink>,
    /// Resolves agent binaries for spawn-style agent mounts. When `None`,
    /// `MountRequest::Agent` is rejected.
    agent_resolver: Option<Arc<dyn crate::agent_resolver::AgentResolver>>,
    /// Extra PATH entries for resolving transport binaries (docker, ssh, …)
    /// on spawn-style agent mounts. Host sessions populate this from
    /// preferences; the agent's ambient PATH is used otherwise.
    extra_path: Vec<String>,
    /// Typed handles to `MountRequest::Remote` mounts, kept at
    /// construction so `remount` can inject owner-supplied meta via the
    /// `RemoteVfs`-only setter without downcasting through the registry.
    remote_mounts: parking_lot::Mutex<HashMap<VfsId, Arc<super::RemoteVfs>>>,
}

impl VfsRegistryManager {
    pub fn new(registry: Arc<VfsRegistry>) -> Self {
        Self {
            registry,
            host_communicator: Arc::new(std::sync::OnceLock::new()),
            pending_read_streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            sftp_askpass: None,
            askpass_provider: None,
            progress_sink: Arc::new(NoopProgressSink),
            agent_resolver: None,
            extra_path: Vec::new(),
            remote_mounts: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    pub fn new_with_host_communicator(
        registry: Arc<VfsRegistry>,
        host_communicator: Arc<std::sync::OnceLock<Communicator>>,
        pending_read_streams: PendingVfsReadStreams,
    ) -> Self {
        Self {
            registry,
            host_communicator,
            pending_read_streams,
            sftp_askpass: None,
            askpass_provider: None,
            progress_sink: Arc::new(NoopProgressSink),
            agent_resolver: None,
            extra_path: Vec::new(),
            remote_mounts: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    pub fn with_sftp_askpass(mut self, askpass: SftpAskpass) -> Self {
        // Mirror the provider into the generic slot so encrypted-archive
        // mounts get an askpass for free wherever SFTP already has one.
        if self.askpass_provider.is_none() {
            self.askpass_provider = Some(askpass.provider.clone());
        }
        self.sftp_askpass = Some(askpass);
        self
    }

    pub fn with_askpass_provider(
        mut self,
        provider: Arc<dyn crate::askpass::AskpassProvider>,
    ) -> Self {
        self.askpass_provider = Some(provider);
        self
    }

    pub fn with_progress_sink(mut self, sink: Arc<dyn VfsProgressSink>) -> Self {
        self.progress_sink = sink;
        self
    }

    pub fn with_agent_resolver(
        mut self,
        resolver: Arc<dyn crate::agent_resolver::AgentResolver>,
    ) -> Self {
        self.agent_resolver = Some(resolver);
        self
    }

    pub fn with_extra_path(mut self, extra_path: Vec<String>) -> Self {
        self.extra_path = extra_path;
        self
    }
}

#[async_trait::async_trait]
impl VfsManager for VfsRegistryManager {
    async fn mount(&self, request: MountRequest) -> Result<MountResponse, Error> {
        // Allocate the id up front so the mount gets a progress reporter
        // already scoped to its final VfsId. The id isn't visible until
        // `insert` below, so a failed mount just leaves it unused.
        let vfs_id = self.registry.allocate_id();
        let progress_reporter: Arc<dyn ProgressReporter> =
            Arc::new(ScopedReporter::new(self.progress_sink.clone(), vfs_id));
        let ctx = MountContext {
            registry: &self.registry,
            host_communicator: &self.host_communicator,
            pending_read_streams: &self.pending_read_streams,
            sftp_askpass: self.sftp_askpass.as_ref(),
            askpass_provider: self.askpass_provider.as_ref(),
            agent_resolver: self.agent_resolver.as_ref(),
            extra_path: &self.extra_path,
            progress_reporter: &progress_reporter,
        };

        let vfs: Arc<dyn Vfs> = match request {
            MountRequest::S3 {
                region,
                bucket,
                credentials,
            } => super::S3Vfs::mount(region, bucket, credentials, &ctx).await?,
            MountRequest::Sftp { host } => super::SftpVfs::mount(host, &ctx).await?,
            MountRequest::Remote { mount_meta } => {
                let remote = super::RemoteVfs::mount(&ctx, mount_meta)?;
                self.remote_mounts.lock().insert(vfs_id, remote.clone());
                remote
            }
            MountRequest::Agent { spec, kind, label } => {
                super::agent::mount(spec, kind, label, &ctx).await?
            }
            MountRequest::Archive { origin } => super::archive::mount(origin, &ctx).await?,
            MountRequest::Disc { origin } => super::disc::mount(origin, &ctx).await?,
            MountRequest::Search { root, params } => {
                // Content matching runs `Filesystem::find_in_file`; the
                // registry-backed impl makes search inside a SearchVfs's
                // source follow registry redirects.
                let file_reader: Arc<dyn Filesystem> =
                    Arc::new(super::VfsRegistryFs::new(self.registry.clone()));
                search::mount(root, params, file_reader, &ctx).await?
            }
        };

        let mount_meta = vfs.mount_meta();
        let type_name = vfs.descriptor().type_name().to_string();
        let origin = vfs.origin().cloned();
        self.registry.insert(vfs_id, vfs);
        log::info!("mounted {} VFS as vfs_id={:?}", type_name, vfs_id);

        Ok(MountResponse {
            vfs_id,
            type_name,
            mount_meta,
            origin,
        })
    }

    async fn unmount(&self, vfs_id: VfsId) -> Result<(), Error> {
        self.remote_mounts.lock().remove(&vfs_id);
        self.registry
            .unmount(vfs_id)
            .map(|_| ())
            .ok_or_else(|| Error::custom(format!("cannot unmount VFS {}", vfs_id)))
    }

    async fn remount(&self, vfs_id: VfsId, mount_meta: Option<Vec<u8>>) -> Result<Vec<u8>, Error> {
        let vfs = self
            .registry
            .get(vfs_id)
            .ok_or_else(|| Error::custom(format!("VFS {} not found", vfs_id)))?;
        match mount_meta {
            Some(meta) => {
                let remote = self
                    .remote_mounts
                    .lock()
                    .get(&vfs_id)
                    .cloned()
                    .ok_or_else(|| {
                        Error::custom(format!(
                            "mount_meta override is only valid for a Remote mount (VFS {})",
                            vfs_id
                        ))
                    })?;
                remote.set_mount_meta(meta.clone());
                Ok(meta)
            }
            None => {
                if vfs.descriptor().can_revalidate() {
                    vfs.revalidate().await?;
                }
                Ok(vfs.mount_meta())
            }
        }
    }
}

#[cfg(test)]
mod remount_tests {
    use super::*;

    #[tokio::test]
    async fn remount_rederives_local_meta_and_rejects_override() {
        let registry = Arc::new(VfsRegistry::with_root(Arc::new(
            super::super::LocalVfs::new(),
        )));
        let manager = VfsRegistryManager::new(registry.clone());

        let meta = manager.remount(VfsId::ROOT, None).await.unwrap();
        assert_eq!(meta, registry.get(VfsId::ROOT).unwrap().mount_meta());

        // Owner-supplied meta is only valid for Remote mounts.
        assert!(
            manager
                .remount(VfsId::ROOT, Some(vec![1, 2, 3]))
                .await
                .is_err()
        );
    }
}
