//! Connect-dialog discovery commands. The enumerators themselves live in
//! `newt_common::discovery` (symmetric Local/Remote service); these handlers
//! only pick which side runs them: window-scoped connections open from this
//! host, pane-scoped agent mounts open from the session owner — so in a
//! remote session the dialog lists the containers/hosts/pods the mount
//! would actually reach.
//!
//! `wsl` reads the registry rather than spawning a CLI, but is still a
//! stateless transport enumerator. It also compiles under `specta-bindings`
//! (inert off-Windows) so the bindings export is host-independent;
//! `allow(dead_code)` covers that build, where only `#[cfg(windows)]`
//! callers use it.

#[cfg(any(windows, feature = "specta-bindings"))]
#[cfg_attr(not(windows), allow(dead_code))]
pub mod wsl;

use std::sync::Arc;

use newt_common::discovery::{
    ContainerEntry, DiscoveryProvider, DiscoveryResult, KubePodEntry, SshHostEntry,
};

use crate::common::Error;
use crate::connections::OpenIn;
use crate::main_window::MainWindowContext;

fn provider(
    ctx: &MainWindowContext,
    global_ctx: &tauri::State<'_, crate::GlobalContext>,
    open_in: OpenIn,
) -> Result<Arc<dyn DiscoveryProvider>, Error> {
    match open_in {
        // Pane mounts are established by the session owner.
        OpenIn::Pane => ctx.discovery_provider(),
        // New windows are spawned from this host.
        OpenIn::Window => Ok(Arc::new(newt_common::discovery::Local::new(
            global_ctx
                .preferences()
                .settings()
                .environment
                .extra_path
                .clone(),
        ))),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn discover_ssh_hosts(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, crate::GlobalContext>,
    open_in: OpenIn,
) -> Result<DiscoveryResult<SshHostEntry>, Error> {
    Ok(provider(&ctx, &global_ctx, open_in)?.ssh_hosts().await?)
}

#[tauri::command]
#[specta::specta]
pub async fn discover_docker_containers(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, crate::GlobalContext>,
    open_in: OpenIn,
) -> Result<DiscoveryResult<ContainerEntry>, Error> {
    Ok(provider(&ctx, &global_ctx, open_in)?
        .containers("docker".to_string())
        .await?)
}

#[tauri::command]
#[specta::specta]
pub async fn discover_podman_containers(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, crate::GlobalContext>,
    open_in: OpenIn,
) -> Result<DiscoveryResult<ContainerEntry>, Error> {
    Ok(provider(&ctx, &global_ctx, open_in)?
        .containers("podman".to_string())
        .await?)
}

#[tauri::command]
#[specta::specta]
pub async fn discover_kube_contexts(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, crate::GlobalContext>,
    open_in: OpenIn,
) -> Result<DiscoveryResult<String>, Error> {
    Ok(provider(&ctx, &global_ctx, open_in)?
        .kube_contexts()
        .await?)
}

#[tauri::command]
#[specta::specta]
pub async fn discover_kube_pods(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, crate::GlobalContext>,
    open_in: OpenIn,
    context: Option<String>,
    namespace: Option<String>,
) -> Result<DiscoveryResult<KubePodEntry>, Error> {
    Ok(provider(&ctx, &global_ctx, open_in)?
        .kube_pods(context, namespace)
        .await?)
}
