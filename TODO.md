# TODO

## Shell integration (`newt` CLI in built-in terminals)

Shipped (design: `design_docs/DESIGN_SHELL_INTEGRATION.md`). Follow-ups:
- `behavior.shell_integration = false` currently gates local sessions only; agents always create the control server (they can't see host preferences). Propagate the flag at agent spawn (env for bootstrap transports like `NEWT_AGENT_MODE`, an arg for direct spawns) if disabling remotely turns out to matter.
- `newt cp --wait` (long-poll an operations endpoint for completion, exit code from the operation result).
- Per-terminal pane affinity (`NEWT_TERMINAL` is already injected), `--json` output, `newt select <glob>`, user-command invocation by title.
- Windows is compiled but untested end-to-end: named-pipe HTTP server/client, `newt.cmd` shim (`NEWT_CLI` marker), ConPTY env merge.

## Remote VFS (local ↔ remote bridge)

Basic Remote VFS is implemented. Remaining work:
- Every path-targeted `Filesystem`/`FileReader` verb is now hairpinned (`list_files`, `poll_changes`, `touch`, `create_directory`, `file_details`, `read_range`, `read_file`, `write_file`, `find_in_file`; `revalidate` is trivially fresh). The one holdout is `get_property_sheet` — diverting it is gated on `RemoteVfsDescriptor::has_extended_properties()` (false today, and `RemoteVfs` doesn't remote the verb), i.e. it needs the "local property sheets in a remote session" feature, not just a hairpin arm. (Rename is no longer a `Filesystem` verb — it runs as `OperationRequest::Rename`.)

## Generalized remote session transport

Shipped: SSH, pkexec, Docker, Podman (both with an opt-in bootstrapless `cp` path), Kubernetes, Custom argv, WSL (`WslLaunch`, no bootstrap), and Windows elevated (UAC + named-pipe RPC). Remaining:
- WSL is the one transport still bootstrapped non-login, so its agent never sources `~/.profile` and starts with a barer `PATH` than a `wsl.exe` shell would. It can't just take `-lc` like SSH did: `WslLaunch` execs the agent directly with no handshake, so RPC starts on the first byte of stdout and any profile that prints a banner would corrupt the stream. Give WSL the same `NEWT:` handshake as `SpawnSpec::Bootstrap` (which skips non-`NEWT:` lines precisely to absorb login-shell chatter), then enable `login_shell` for it.
- Host-key prompts and "are you sure you want to add fingerprint" flows for SSH still ride the existing askpass channel; verify the same UX with `StrictHostKeyChecking=accept-new` setups.

## Pane-mounted agent connections (agent-as-VFS)

Shipped (design: `design_docs/DESIGN_AGENT_VFS_MOUNTS.md`). Follow-ups:
- Quick Connect affordance to override a profile's `open_in` at activation time (modifier key, submenu, or two entries).
- Auto-reconnect for dead agent mounts — folds into the dead-history-entry remount item below.
- Real-world pass over double-hop askpass and foreign-arch provisioning (unit/e2e covered; not yet exercised against a live remote+container).

## VFS property sheets (S3 ACLs / metadata)

Shipped (design: `design_docs/DESIGN_VFS_PROPERTY_SHEETS.md`). Follow-ups:
- `Vfs`-level remoting of the two verbs (`API_VFS_*` constants + `RemoteVfs`/`VfsDispatcher` arms) — deferred until `LocalVfs` grows a sheet (xattrs); nothing crosses that layer today.

## Dialog visual uplift

Shipped (shared dialog primitives in `modals/primitives/`, all ~26 dialogs migrated). Remaining:
- `HotPaths.module.scss` deleteBtn hover keeps an `opacity !important`; HistoryNavigator keeps two `!important`s fighting Menu.module's `data-highlighted` styling — both need a structural fix in Menu.module.scss to remove.

## Drag and drop

Drag-out shipped (escalation at the window edge → native OS drag via the `drag` crate, copy-only, host-local files only). Follow-ups:
- Drag-out for non-host-local sources (S3/SFTP/remote sessions) needs materialization: either download-to-tempdir before the native drag starts (reuse the `download_and_open` pattern), or per-platform file-promise APIs (NSFilePromiseProvider / CFSTR_FILEDESCRIPTOR / XDS) — no cross-platform crate wraps those today.

## Archive unpacking

A dedicated extract operation with conflict handling, not a VFS — packing shipped as Pack to Archive / Alt+F5; today unpacking means copying out of a mounted archive VFS.

## Sans-IO zip reader

Replace the `zip` crate in `ZipArchiveVfs` with an in-tree sans-IO reader on the `newt-disc` model (batched range requests, async-native — no `RangeReadAdapter`/`spawn_blocking` bridge). Fixes the structural inefficiencies the crate forces: every `read_range` re-opens the archive and decompresses the whole entry (the F3 viewer's chunked fan-out makes that quadratic), and central-directory indexing issues a hail of tiny upstream reads instead of a few coalesced ones. Natural home for a shared block cache and a decompressed-entry LRU; `newt-archive` already owns the write side.

## Disc image VFS follow-ups

ISO 9660 (+Joliet/Rock Ridge) and UDF through 2.60 shipped as the `disc` VFS (`newt-disc` sans-IO parser + `vfs/disc.rs` driver). Remaining ideas:
- VAT/virtual and sparable partition maps (packet-written CD-RW/DVD±RW dumps) — currently a clean "unsupported" error.
- `.img` support via content sniffing: the extension is ambiguous (raw disk images with partition tables vs raw ISO9660/UDF), so claiming it needs a cheap probe before mount rather than an extension match.
- El Torito boot catalog: expose boot images as synthetic entries at the mount root.

## Enrichers

Subsystem + git + du enrichers shipped (design: `design_docs/DESIGN_ENRICHERS_AND_RESOURCES.md`). Remaining:
- Agent-VFS pane mounts aren't enriched (enrichment is session-level; the sub-agent's `--serve-vfs` mode has no enricher dispatcher). Needs per-VFS enricher routing, the way `Vfs` verbs remote, if it proves wanted.

## Unified VFS recursor + operations improvements (ideas, not started)

- **Unified VFS file recursor.** One shared recursive-walk primitive for everything that traverses trees today with hand-rolled loops: the operations scan/execute phases, the du enricher's `walk_entry`, and (candidate) the search walker. Bakes in the rules each copy has re-derived — never cross registry mount boundaries, don't follow directory symlinks, skip `/proc`, skip unreadable subtrees, optional device-boundary guard — plus streaming output, progress reporting that fits all consumers (operations `Scanning` counters, `EnrichSink` running totals, `VfsProgress`), per-walk concurrency caps, and drop-based cancellation. Key trait addition: an *optional* flat recursive listing verb on `Vfs` (e.g. `list_files_recursive(prefix)` streaming all nested paths directly). Default: unimplemented — the recursor recurses level-by-level at a higher layer, consistent with the trait philosophy. S3 overrides it: a delimiter-less `ListObjectsV2` over a prefix *is* the flat listing, turning du / large prefix copies over S3 from one round-trip per pseudo-directory into a few paginated calls. Descriptor capability flag so the recursor picks the fast path statically.
- **Safer copy / recursive delete: `--one-file-system` guard.** `File.device_id` makes it possible: the operations scanner records the root device per source and refuses to descend across a device boundary (delete especially — a mountpoint hiding under a tree being deleted is the classic footgun; `rm -r --one-file-system` semantics). Decide surface: always-on for delete with a per-operation override, or a behavior preference. Unix-only data, so the guard degrades to today's behavior on Windows. Folds naturally into the unified recursor above.
- **Operation framework hardening.** (a) Write-to-temp + atomic rename for overwrites instead of truncate-in-place, on VFSes with `can_rename` — a cancelled/failed copy must never leave a half-written destination; temp naming + orphan cleanup on failure; S3 and friends keep the direct write (PUT is already atomic). (b) Broader metadata preservation on copy (times/mode exist; audit owner/xattrs per VFS pair). (c) Richer conflict handling on the existing issue-resolution channel: keep-if-newer, skip-identical (size+mtime), rename-both, remembered "apply to all" choices per conflict kind.

## Bug fixes and strengthening

- `TerminalHandle` is minted per session (`Local::new()` per `session.rs`), so every window's first terminal is handle 0 — a holdover from when each MainWindow was its own process and handles were therefore process-unique. The cross-window terminal cross-talk this caused is fixed (the `terminal_data` emit is window-scoped now), but the colliding handles remain and will bite the next thing that routes by handle alone. Kill the class rather than the instance: either a process-wide counter, or make `TerminalHandle` carry its session. See "Window-targeted events" in CLAUDE.md.
- Local macOS is the one place the app's own `PATH` is still patched by hand (`[environment] extra_path`) rather than inherited. Everywhere else the environment now arrives ambiently — a login-shell bootstrap for agents, PAM/systemd on a Linux desktop, the registry on Windows — but a Finder-launched `.app` has no login shell above it to inherit from, so there's nothing to hang off. The visible seam: the terminal gets `-l` and so has the user's full `PATH`, while a Newt-spawned command gets launchd's plus `extra_path`, so a tool can work when typed and fail as a command. The cure is what VS Code does — probe once at startup (`$SHELL -ilc`, marker-delimited JSON on stdout so profile chatter can't corrupt it, bounded by a timeout since a profile can block forever on a prompt) and thread the result as a base env rather than `set_var` (edition 2024, and `shell.rs` deliberately doesn't mutate our own environment). Not worth it until the manual patching actually bites.
- Auto-remount VFSes when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.
- Implement `Vfs::revalidate` for archive VFSes (zip + tar). Trait is wired through to the navigation layer (called when a pane crosses into a VFS that advertises `VfsDescriptor::can_revalidate`); the archive impl should stat the origin file's mtime against the value captured at mount time and rebuild the central directory / entry index in place if it drifted, returning `Refreshed`. Mount identity (`VfsId`, `mount_meta`, `origin`) must be preserved so history entries remain valid. Don't forget to flip `can_revalidate` to true on the descriptors.
