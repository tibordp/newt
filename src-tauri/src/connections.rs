use serde::{Deserialize, Serialize};

use tauri::Manager;

use crate::common::Error;

/// A saved connection profile. Secrets are stored in the system keychain,
/// not in this struct or the connections file.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: ConnectionKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionKind {
    S3 {
        #[serde(default)]
        region: Option<String>,
        #[serde(default)]
        bucket: Option<String>,
        #[serde(default)]
        endpoint_url: Option<String>,
        /// "default" | "profile" | "iam_user" | "assume_role"
        #[serde(default = "default_credential_mode")]
        credential_mode: String,
        #[serde(default)]
        profile: Option<String>,
        #[serde(default)]
        role_arn: Option<String>,
        #[serde(default)]
        external_id: Option<String>,
    },
    Sftp {
        host: String,
    },
    Ssh {
        host: String,
        /// `ssh -A`. Forwards the local SSH agent to the remote, so any
        /// downstream ssh / sftp invocation on the agent side can re-use the
        /// host's keys.
        #[serde(default)]
        forward_agent: bool,
    },
    Docker {
        container: String,
        #[serde(default)]
        user: Option<String>,
        /// When true (the default), the agent is delivered via `docker cp`
        /// and exec'd directly — fewer moving parts and works for sh-less
        /// images. Setting false uses the sh-based bootstrap, which has a
        /// hash-keyed cache and avoids re-uploading on every connect.
        #[serde(default = "default_true")]
        bootstrapless: bool,
    },
    Podman {
        container: String,
        #[serde(default)]
        user: Option<String>,
        #[serde(default = "default_true")]
        bootstrapless: bool,
    },
    Kube {
        #[serde(default)]
        context: Option<String>,
        #[serde(default)]
        namespace: Option<String>,
        pod: String,
        #[serde(default)]
        container: Option<String>,
    },
    Custom {
        /// Raw shell command. Executed locally via `sh -c <command>` with the
        /// bootstrap script exposed in the `NEWT_BOOTSTRAP` env var, so the
        /// user can splice it in wherever — e.g. `ssh host "$NEWT_BOOTSTRAP"`
        /// or `docker exec -i ctr sh -c "$NEWT_BOOTSTRAP"`.
        command: String,
        /// If true, skip the bootstrap handshake and hand the user's stdio
        /// straight to the RPC layer. Useful when the user already has an
        /// agent running and just needs a pipe to it.
        #[serde(default)]
        skip_bootstrap: bool,
    },
}

fn default_credential_mode() -> String {
    "default".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, specta::Type)]
struct ConnectionsFile {
    #[serde(default, rename = "connection")]
    connections: Vec<ConnectionProfile>,
}

fn connections_path(config_dir: &std::path::Path) -> std::path::PathBuf {
    config_dir.join("connections.toml")
}

pub fn list_connections(config_dir: &std::path::Path) -> Vec<ConnectionProfile> {
    let path = connections_path(config_dir);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    match toml::from_str::<ConnectionsFile>(&content) {
        Ok(file) => file.connections,
        Err(e) => {
            log::warn!("Failed to parse connections.toml: {}", e);
            Vec::new()
        }
    }
}

pub fn save_connection(
    config_dir: &std::path::Path,
    profile: ConnectionProfile,
) -> Result<(), Error> {
    let path = connections_path(config_dir);
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut file: ConnectionsFile = toml::from_str(&content).unwrap_or_default();

    // Update existing or append
    if let Some(existing) = file.connections.iter_mut().find(|c| c.id == profile.id) {
        *existing = profile;
    } else {
        file.connections.push(profile);
    }

    let serialized = toml::to_string_pretty(&file).map_err(|e| Error::Custom(e.to_string()))?;
    std::fs::write(&path, serialized).map_err(|e| Error::Custom(e.to_string()))?;
    Ok(())
}

pub fn delete_connection(config_dir: &std::path::Path, id: &str) -> Result<(), Error> {
    let path = connections_path(config_dir);
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut file: ConnectionsFile = toml::from_str(&content).unwrap_or_default();

    file.connections.retain(|c| c.id != id);

    let serialized = toml::to_string_pretty(&file).map_err(|e| Error::Custom(e.to_string()))?;
    std::fs::write(&path, serialized).map_err(|e| Error::Custom(e.to_string()))?;
    Ok(())
}

/// Build a `ConnectionTarget` for the spawn-style transport kinds. Returns
/// `None` for VFS-only kinds (S3, SFTP). The second element is a label used
/// for the window title / connection log.
pub fn connection_target_for(
    kind: &ConnectionKind,
) -> Option<(crate::main_window::ConnectionTarget, String)> {
    use crate::main_window::{
        ConnectionTarget, SpawnSpec, docker_direct_copy_plan, docker_transport_cmd,
        kube_transport_cmd, podman_direct_copy_plan, podman_transport_cmd, ssh_transport_cmd,
    };
    match kind {
        ConnectionKind::Ssh {
            host,
            forward_agent,
        } => Some((
            ConnectionTarget::Spawn(SpawnSpec::Bootstrap {
                transport_cmd: ssh_transport_cmd(host, *forward_agent),
                label: host.clone(),
                askpass: true,
                shell_join: true,
            }),
            host.clone(),
        )),
        ConnectionKind::Docker {
            container,
            user,
            bootstrapless,
        } => {
            let label = format!("docker:{}", container);
            let target = if *bootstrapless {
                ConnectionTarget::Spawn(SpawnSpec::DirectCopy(docker_direct_copy_plan(
                    container,
                    user.as_deref(),
                )))
            } else {
                ConnectionTarget::Spawn(SpawnSpec::Bootstrap {
                    transport_cmd: docker_transport_cmd(container, user.as_deref()),
                    label: label.clone(),
                    askpass: false,
                    shell_join: false,
                })
            };
            Some((target, label))
        }
        ConnectionKind::Podman {
            container,
            user,
            bootstrapless,
        } => {
            let label = format!("podman:{}", container);
            let target = if *bootstrapless {
                ConnectionTarget::Spawn(SpawnSpec::DirectCopy(podman_direct_copy_plan(
                    container,
                    user.as_deref(),
                )))
            } else {
                ConnectionTarget::Spawn(SpawnSpec::Bootstrap {
                    transport_cmd: podman_transport_cmd(container, user.as_deref()),
                    label: label.clone(),
                    askpass: false,
                    shell_join: false,
                })
            };
            Some((target, label))
        }
        ConnectionKind::Kube {
            context,
            namespace,
            pod,
            container,
        } => {
            let label = match (namespace, container) {
                (Some(ns), Some(c)) => format!("kube:{}/{}:{}", ns, pod, c),
                (Some(ns), None) => format!("kube:{}/{}", ns, pod),
                (None, Some(c)) => format!("kube:{}:{}", pod, c),
                (None, None) => format!("kube:{}", pod),
            };
            Some((
                ConnectionTarget::Spawn(SpawnSpec::Bootstrap {
                    transport_cmd: kube_transport_cmd(
                        context.as_deref(),
                        namespace.as_deref(),
                        pod,
                        container.as_deref(),
                    ),
                    label: label.clone(),
                    askpass: false,
                    shell_join: false,
                }),
                label,
            ))
        }
        ConnectionKind::Custom {
            command,
            skip_bootstrap,
        } => {
            let label = command
                .split_whitespace()
                .next()
                .unwrap_or("custom")
                .to_string();
            Some((
                ConnectionTarget::Spawn(SpawnSpec::CustomShell {
                    command: command.clone(),
                    label: label.clone(),
                    skip_bootstrap: *skip_bootstrap,
                }),
                label,
            ))
        }
        ConnectionKind::S3 { .. } | ConnectionKind::Sftp { .. } => None,
    }
}

// --- Keychain helpers for connection secrets ---

const KEYCHAIN_PREFIX: &str = "connection:";

pub fn get_connection_secret(id: &str) -> Result<Option<String>, Error> {
    crate::keychain::keychain_get(format!("{}{}", KEYCHAIN_PREFIX, id))
}

pub fn set_connection_secret(id: &str, value: &str) -> Result<(), Error> {
    crate::keychain::keychain_set(format!("{}{}", KEYCHAIN_PREFIX, id), value.to_string())
}

pub fn delete_connection_secret(id: &str) -> Result<(), Error> {
    crate::keychain::keychain_delete(format!("{}{}", KEYCHAIN_PREFIX, id))
}

/// Build a MountRequest from a connection profile, loading secrets from keychain.
pub fn build_mount_request(
    profile: &ConnectionProfile,
) -> Result<Option<newt_common::vfs::MountRequest>, Error> {
    match &profile.kind {
        ConnectionKind::S3 {
            region,
            bucket,
            endpoint_url,
            credential_mode,
            profile: aws_profile,
            role_arn,
            external_id,
        } => {
            let mut creds = newt_common::vfs::S3Credentials {
                profile: aws_profile.clone(),
                endpoint_url: endpoint_url.clone(),
                role_arn: role_arn.clone(),
                external_id: external_id.clone(),
                ..Default::default()
            };

            // Load IAM user secrets from keychain if applicable
            if credential_mode == "iam_user"
                && let Some(secret_json) = get_connection_secret(&profile.id)?
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&secret_json)
            {
                creds.access_key_id = parsed["access_key_id"].as_str().map(|s| s.to_string());
                creds.secret_access_key =
                    parsed["secret_access_key"].as_str().map(|s| s.to_string());
            }

            Ok(Some(newt_common::vfs::MountRequest::S3 {
                region: region.clone(),
                bucket: bucket.clone(),
                credentials: creds,
            }))
        }
        ConnectionKind::Sftp { host } => Ok(Some(newt_common::vfs::MountRequest::Sftp {
            host: host.clone(),
        })),
        // Spawn-style transports — not VFS mounts.
        ConnectionKind::Ssh { .. }
        | ConnectionKind::Docker { .. }
        | ConnectionKind::Podman { .. }
        | ConnectionKind::Kube { .. }
        | ConnectionKind::Custom { .. } => Ok(None),
    }
}

// --- Tauri commands ---

#[tauri::command]
#[specta::specta]
pub fn cmd_list_connections(
    global_ctx: tauri::State<'_, crate::GlobalContext>,
) -> Result<Vec<ConnectionProfile>, Error> {
    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
    Ok(list_connections(&config_dir))
}

#[tauri::command]
#[specta::specta]
pub fn cmd_save_connection(
    global_ctx: tauri::State<'_, crate::GlobalContext>,
    profile: ConnectionProfile,
    secret: Option<String>,
) -> Result<(), Error> {
    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
    save_connection(&config_dir, profile.clone())?;
    if let Some(secret) = secret {
        set_connection_secret(&profile.id, &secret)?;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn cmd_delete_connection(
    global_ctx: tauri::State<'_, crate::GlobalContext>,
    id: String,
) -> Result<(), Error> {
    let config_dir = global_ctx.preferences().config_dir().to_path_buf();
    delete_connection(&config_dir, &id)?;
    let _ = delete_connection_secret(&id);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn cmd_get_connection_secret(id: String) -> Result<Option<String>, Error> {
    get_connection_secret(&id)
}

#[tauri::command]
#[specta::specta]
pub async fn connect_profile(
    ctx: crate::main_window::MainWindowContext,
    pane_handle: crate::main_window::PaneHandle,
    id: String,
) -> Result<(), Error> {
    let config_dir = {
        let app_handle = ctx.window().app_handle().clone();
        let global_ctx: tauri::State<crate::GlobalContext> = app_handle.state();
        global_ctx.preferences().config_dir().to_path_buf()
    };

    let connections = list_connections(&config_dir);
    let profile = connections
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| Error::Custom(format!("connection profile '{}' not found", id)))?;

    if let Some((target, _label)) = connection_target_for(&profile.kind) {
        let app_handle = ctx.window().app_handle().clone();
        crate::main_window::spawn_main_window(
            &app_handle,
            target,
            format!("Newt [{}]", profile.name),
            [None, None],
        )?;
        ctx.with_update(|gs| {
            gs.close_modal();
            Ok(())
        })
    } else {
        let request = build_mount_request(&profile)?
            .ok_or_else(|| Error::Custom("unsupported connection type".into()))?;

        let response = ctx.mount_vfs(request).await?;
        let vfs_path = newt_common::vfs::VfsPath::root(response.vfs_id);

        ctx.with_pane_update_async(pane_handle, |gs, pane| async move {
            gs.close_modal();
            pane.navigate_to(vfs_path).await?;
            Ok(())
        })
        .await
    }
}
