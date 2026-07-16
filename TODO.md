# TODO

## Host VFS (local ↔ remote bridge)

Basic Remote VFS is implemented. Remaining work:
- Hairpin diversion for additional methods (touch, create_directory, etc.) — only `list_files`, `poll_changes`, `read_range`, `read_file`, `write_file` are diverted today. (Rename is no longer a `Filesystem` verb — it runs as `OperationRequest::Rename`.)

## Generalized remote session transport

Implemented: SSH, pkexec, Docker, Podman, Kubernetes (kubectl exec), and Custom (caller-supplied argv) all share the same `SpawnSpec::Bootstrap` path. Docker and Podman additionally support an opt-in `bootstrapless` (`docker cp` / `podman cp`) path for distroless / sh-less containers. WSL (Windows) is a separate `ConnectionTarget::Wsl` path: `wslapi!WslLaunch` (loaded at runtime) execs the bundled Linux-musl agent directly from its `/mnt/<drive>/…` path — no bootstrap, no upload — with distros enumerated from the `Lxss` registry key. Elevated mode now works on Windows too (`ConnectionTarget::Elevated`): `ShellExecuteEx "runas"` (UAC) launches the native agent, which speaks RPC over a single-instance named pipe (`ShellExecuteEx` can't redirect stdio); the host GUI stays unelevated. Remaining:
- WSL assumes the default `[automount] root = /mnt`; a custom automount root isn't detected. Read `/etc/wsl.conf` if this proves limiting.
- WSL agent arch is derived from the Windows host arch (correct for WSL2 / x64 WSL1); no in-distro `uname` probe.
- Windows elevated agent stderr/logs are not captured (`runas` carries no console). Optional: have the elevated agent log to `%TEMP%\newt-agent-elevated.log` for debugging.
- Host-key prompts and "are you sure you want to add fingerprint" flows for SSH still ride the existing askpass channel; verify the same UX with `StrictHostKeyChecking=accept-new` setups.
- The bootstrapless flow trusts the daemon's reported OS/arch from `inspect`. If the container runs a foreign-arch userspace under qemu the agent will be the wrong arch; consider a `--print-triple` self-check post-launch.
- Kubernetes is bootstrap-only (kubectl cp itself needs tar). If we want sh-less k8s pods later we'd need to teach the agent how to be `tar`-injected.

## Pane-mounted agent connections (agent-as-VFS)

Shipped (design: `design_docs/DESIGN_AGENT_VFS_MOUNTS.md`): `newt_common::connect` spawn infra behind the `ConnectLog` seam, `--serve-vfs` FS-only agent mode (e2e-tested in `newt-agent/tests/serve_vfs.rs`), `MountRequest::Agent` under the `"agent"` descriptor, streaming agent-binary provisioning (`API_HOST_FETCH_AGENT`, self-exe fast path, materialize cache for `docker cp` modes), and the Connect dialog / profile `open_in` knob. Follow-ups:
- Quick Connect affordance to override a profile's `open_in` at activation time (modifier key, submenu, or two entries).
- Auto-reconnect for dead agent mounts — folds into the dead-history-entry remount item below.
- Real-world pass over double-hop askpass and foreign-arch provisioning (unit/e2e covered; not yet exercised against a live remote+container).

## VFS property sheets (S3 ACLs / metadata)

Shipped (design: `design_docs/DESIGN_VFS_PROPERTY_SHEETS.md`): `Vfs::get_property_sheet`/`apply_properties` + `has_extended_properties` capability, schema-driven `PropertySheet`/`PropertyPatch` (`vfs/properties.rs`, with host-side fold for multi-select), reads via a `FileReader` verb, writes via `OperationRequest::ApplyProperties` (recursive/prefix apply included), S3 sheet (user metadata, storage class, Content-Type/Cache-Control, grants, write-only canned ACL; CopyObject-REPLACE rewrite that preserves untouched headers and non-default ACLs), open-then-fill sheet groups in the Properties dialog with a generic per-kind renderer. Follow-ups:
- `Vfs`-level remoting of the two verbs (`API_HOST_VFS_*` constants + `RemoteVfs`/`VfsHostDispatcher` arms) — deferred until `LocalVfs` grows a sheet (xattrs); nothing crosses that layer today.
- Recursive prefix apply is unreachable for an all-directories S3 selection (no file entry to source the sheet's fields from); revisit if it bites.
- CopyObject-based rewrite fails on objects >5 GiB (needs multipart copy) and on unrestored Glacier objects; both surface as per-item operation issues.

## Dialog visual uplift

Shipped: shared dialog primitives (`modals/primitives/` — DialogShell/Header/Body/Footer with pinned chrome-tinted footer, Field/FieldGroup/CheckboxField/FieldRow, DialogTabs, extended DialogSubmitButton with a `destructive` variant), deep elevation shadow instead of a backdrop scrim (a scrim was tried and dropped as too web-modal), `_dialog-mixins.scss` for floating containers (centered/top-anchored/tab-bar/cmdk chrome), font-size + `--font-mono` tokens, and all ~26 dialogs migrated (legacy `dialogContents`/`dialogButtons` classes deleted; About/Search/mount dialogs no longer hand-roll inline styles). Follow-ups:
- Settings editor *internals* (SettingControls/CommandsEditor) still use ad-hoc inline `style={{}}` for widget sizing; migrate to module classes if they get touched again.
- `HotPaths.module.scss` deleteBtn hover keeps an `opacity !important`; HistoryNavigator keeps two `!important`s fighting Menu.module's `data-highlighted` styling — both need a structural fix in Menu.module.scss to remove.

## Drag and drop

Drag-out shipped (escalation at the window edge → native OS drag via the `drag` crate, copy-only, host-local files only; self-drops route as internal drops; cross-window drags work via the external-drop path). Follow-ups:
- Drag-out for non-host-local sources (S3/SFTP/remote sessions) needs materialization: either download-to-tempdir before the native drag starts (reuse the `download_and_open` pattern), or per-platform file-promise APIs (NSFilePromiseProvider / CFSTR_FILEDESCRIPTOR / XDS) — no cross-platform crate wraps those today.
- Known upstream gaps to re-test on drag-rs upgrades: Windows >260-char paths crash (drag-rs #76), GTK3/X11 drops occasionally land nothing (#84), Wayland untested.

## Archive unpacking

(as an operation, not a VFS — packing shipped as Pack to Archive / Alt+F5; a dedicated extract operation with conflict handling remains, today unpacking means copying out of a mounted archive VFS)

## Enrichers

Subsystem + git enricher shipped (design: `design_docs/DESIGN_ENRICHERS_AND_RESOURCES.md`): symmetric `newt_common::enrich` (Enrichers registry beside the VfsRegistry, EnricherClient Local/Remote, `API_START_ENRICHMENT`/`API_ENRICHMENT_EVENT`, drop-based cancellation), pane overlay anchored to the history cursor (survives refresh, cleared on navigation), git status via shell-out (row colors, dir rollups, branch badge, `behavior.git_status` toggle). Remaining:
- Du enricher (recursive directory sizes) — manual keybinds ("size entry under cursor" / "size all children"), streaming running totals into the size column, per-visit ephemeral (anchored to the history cursor like everything else, which dissolves the cache-invalidation problem), walk via `registry.list_files`.
- Agent-VFS pane mounts aren't enriched (enrichment is session-level; the sub-agent's `--serve-vfs` mode has no enricher dispatcher). Needs per-VFS enricher routing, the way `Vfs` verbs remote, if it proves wanted.
- Git status taxonomy nuances to revisit if they bite: deleted-only directories render as Modified rollups; copied (`C`) shows as Renamed; submodule status changes render as Modified.

## Bug fixes and strengthening

- Auto-remount VFSes when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.
- Persist column widths across sessions (today they only persist for the lifetime of the session)
- Implement `Vfs::revalidate` for archive VFSes (zip + tar). Trait is wired through to the navigation layer (called when a pane crosses into a VFS that advertises `VfsDescriptor::can_revalidate`); the archive impl should stat the origin file's mtime against the value captured at mount time and rebuild the central directory / entry index in place if it drifted, returning `Refreshed`. Mount identity (`VfsId`, `mount_meta`, `origin`) must be preserved so history entries remain valid. Don't forget to flip `can_revalidate` to true on the descriptors.
