# TODO

## Host VFS (local ↔ remote bridge)

Basic Remote VFS is implemented. Remaining work:
- Hairpin diversion for additional methods (rename, touch, create_directory, etc.) — only `list_files`, `poll_changes`, `read_range`, `read_file`, `write_file` are diverted today.

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

## Archive unpacking

(as an operation, not a VFS — packing shipped as Pack to Archive / Alt+F5; a dedicated extract operation with conflict handling remains, today unpacking means copying out of a mounted archive VFS)

## Bug fixes and strengthening

- Auto-remount VFSes when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.
- Persist column widths across sessions (today they only persist for the lifetime of the session)
- Implement `Vfs::revalidate` for archive VFSes (zip + tar). Trait is wired through to the navigation layer (called when a pane crosses into a VFS that advertises `VfsDescriptor::can_revalidate`); the archive impl should stat the origin file's mtime against the value captured at mount time and rebuild the central directory / entry index in place if it drifted, returning `Refreshed`. Mount identity (`VfsId`, `mount_meta`, `origin`) must be preserved so history entries remain valid. Don't forget to flip `can_revalidate` to true on the descriptors.

## Compute dir sizes recursively (with caching)

Cache invalidation is the hard problem — filesystem events don't bubble up from subdirectories reliably.
