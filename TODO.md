# TODO

## Host VFS (local ↔ remote bridge)

Basic Remote VFS is implemented. Remaining work:
- Hairpin diversion for additional methods (touch, create_directory, etc.) — only `list_files`, `poll_changes`, `read_range`, `read_file`, `write_file` are diverted today. (Rename is no longer a `Filesystem` verb — it runs as `OperationRequest::Rename`.)

## Generalized remote session transport

Implemented: SSH, pkexec, Docker, Podman, Kubernetes (kubectl exec), and Custom (caller-supplied argv) all share the same `SpawnSpec::Bootstrap` path. Docker and Podman additionally support an opt-in `bootstrapless` (`docker cp` / `podman cp`) path for distroless / sh-less containers. WSL (Windows) is a separate `ConnectionTarget::Wsl` path: `wslapi!WslLaunch` (loaded at runtime) execs the bundled Linux-musl agent directly from its `/mnt/<drive>/…` path — no bootstrap, no upload — with distros enumerated from the `Lxss` registry key. Elevated mode now works on Windows too (`ConnectionTarget::Elevated`): `ShellExecuteEx "runas"` (UAC) launches the native agent, which speaks RPC over a single-instance named pipe (`ShellExecuteEx` can't redirect stdio); the host GUI stays unelevated. Remaining:
- WSL is the one transport still bootstrapped non-login, so its agent never sources `~/.profile` and starts with a barer `PATH` than a `wsl.exe` shell would. It can't just take `-lc` like SSH did: `WslLaunch` execs the agent directly with no handshake, so RPC starts on the first byte of stdout and any profile that prints a banner would corrupt the stream. Give WSL the same `NEWT:` handshake as `SpawnSpec::Bootstrap` (which skips non-`NEWT:` lines precisely to absorb login-shell chatter), then enable `login_shell` for it.
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

Subsystem + git + du enrichers shipped (design: `design_docs/DESIGN_ENRICHERS_AND_RESOURCES.md`): symmetric `newt_common::enrich` (Enrichers registry beside the VfsRegistry, EnricherClient Local/Remote, `API_START_ENRICHMENT`/`API_ENRICHMENT_EVENT`, drop-based cancellation), pane overlay anchored to the history cursor (survives refresh, cleared on navigation), git status via shell-out (row colors, dir rollups, branch badge, `enrichers.git_status` toggle), recursive dir sizes via manual keybinds (streaming running totals, accumulating manual lane, VFS-generic walk). Remaining:
- Agent-VFS pane mounts aren't enriched (enrichment is session-level; the sub-agent's `--serve-vfs` mode has no enricher dispatcher). Needs per-VFS enricher routing, the way `Vfs` verbs remote, if it proves wanted.
- Git status taxonomy nuances to revisit if they bite: deleted-only directories render as Modified rollups; copied (`C`) shows as Renamed; submodule status changes render as Modified.
- Du hardlink dedup is per sized entry (deterministic under the concurrent walker) — two sibling entries sharing hardlinked files each count them, so the status-bar total can exceed a single `du -s` of the parent. Revisit with a run-wide `(device_id, inode)` set if it bites.
- Windows reports no `allocated_size`/`device_id`/`inode`/`hard_links` (all need per-file handles there); du falls back to apparent sizes on Windows.

## Unified VFS recursor + operations improvements (ideas, not started)

- **Unified VFS file recursor.** One shared recursive-walk primitive for everything that traverses trees today with hand-rolled loops: the operations scan/execute phases, the du enricher's `walk_entry`, and (candidate) the search walker. Bakes in the rules each copy has re-derived — never cross registry mount boundaries, don't follow directory symlinks, skip `/proc`, skip unreadable subtrees, optional device-boundary guard — plus streaming output, progress reporting that fits all consumers (operations `Scanning` counters, `EnrichSink` running totals, `VfsProgress`), per-walk concurrency caps, and drop-based cancellation. Key trait addition: an *optional* flat recursive listing verb on `Vfs` (e.g. `list_files_recursive(prefix)` streaming all nested paths directly). Default: unimplemented — the recursor recurses level-by-level at a higher layer, consistent with the trait philosophy. S3 overrides it: a delimiter-less `ListObjectsV2` over a prefix *is* the flat listing, turning du / large prefix copies over S3 from one round-trip per pseudo-directory into a few paginated calls. Descriptor capability flag so the recursor picks the fast path statically.
- **Show the new `File` metadata in the Properties dialog.** `allocated_size` ("size on disk" next to the apparent size — interesting exactly when they diverge: sparse files, compressed volumes), `hard_links` count, `inode`/`device_id`. All unix-only `Option`s — hide rows when `None`. Plumbs through `ModalDataKind::Properties` population in `cmd/dialog.rs`; multi-select fold should sum allocated sizes like it sums apparent ones.
- **Safer copy / recursive delete: `--one-file-system` guard.** `File.device_id` makes it possible: the operations scanner records the root device per source and refuses to descend across a device boundary (delete especially — a mountpoint hiding under a tree being deleted is the classic footgun; `rm -r --one-file-system` semantics). Decide surface: always-on for delete with a per-operation override, or a behavior preference. Unix-only data, so the guard degrades to today's behavior on Windows. Folds naturally into the unified recursor above.
- **Operation framework hardening.** (a) Write-to-temp + atomic rename for overwrites instead of truncate-in-place, on VFSes with `can_rename` — a cancelled/failed copy must never leave a half-written destination; temp naming + orphan cleanup on failure; S3 and friends keep the direct write (PUT is already atomic). (b) Broader metadata preservation on copy (times/mode exist; audit owner/xattrs per VFS pair). (c) Richer conflict handling on the existing issue-resolution channel: keep-if-newer, skip-identical (size+mtime), rename-both, remembered "apply to all" choices per conflict kind.

## Bug fixes and strengthening

- `TerminalHandle` is minted per session (`Local::new()` per `session.rs`), so every window's first terminal is handle 0 — a holdover from when each MainWindow was its own process and handles were therefore process-unique. The cross-window terminal cross-talk this caused is fixed (the `terminal_data` emit is window-scoped now), but the colliding handles remain and will bite the next thing that routes by handle alone. Kill the class rather than the instance: either a process-wide counter, or make `TerminalHandle` carry its session. See "Window-targeted events" in CLAUDE.md.
- Local macOS is the one place the app's own `PATH` is still patched by hand (`[environment] extra_path`) rather than inherited. Everywhere else the environment now arrives ambiently — a login-shell bootstrap for agents, PAM/systemd on a Linux desktop, the registry on Windows — but a Finder-launched `.app` has no login shell above it to inherit from, so there's nothing to hang off. The visible seam: the terminal gets `-l` and so has the user's full `PATH`, while a Newt-spawned command gets launchd's plus `extra_path`, so a tool can work when typed and fail as a command. The cure is what VS Code does — probe once at startup (`$SHELL -ilc`, marker-delimited JSON on stdout so profile chatter can't corrupt it, bounded by a timeout since a profile can block forever on a prompt) and thread the result as a base env rather than `set_var` (edition 2024, and `shell.rs` deliberately doesn't mutate our own environment). Not worth it until the manual patching actually bites.
- Auto-remount VFSes when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.
- Persist column widths across sessions (today they only persist for the lifetime of the session)
- Implement `Vfs::revalidate` for archive VFSes (zip + tar). Trait is wired through to the navigation layer (called when a pane crosses into a VFS that advertises `VfsDescriptor::can_revalidate`); the archive impl should stat the origin file's mtime against the value captured at mount time and rebuild the central directory / entry index in place if it drifted, returning `Refreshed`. Mount identity (`VfsId`, `mount_meta`, `origin`) must be preserved so history entries remain valid. Don't forget to flip `can_revalidate` to true on the descriptors.
