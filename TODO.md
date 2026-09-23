# TODO

## Shell integration (`newt` CLI in built-in terminals)

Design: `design_docs/DESIGN_SHELL_INTEGRATION.md`.

- `behavior.shell_integration = false` gates local sessions only; agents always create the control server (they can't see host preferences). Propagate the flag at agent spawn (env for bootstrap transports like `NEWT_AGENT_MODE`, an arg for direct spawns) if disabling remotely turns out to matter.
- `newt cp --wait` (long-poll an operations endpoint for completion, exit code from the operation result).
- Per-terminal pane affinity (`NEWT_TERMINAL` is already injected), `--json` output, user-command invocation by title.

## Remote sessions and agent mounts

Design: `design_docs/DESIGN_AGENT_VFS_MOUNTS.md`, `design_docs/DESIGN_ENRICHERS_AND_RESOURCES.md`.

- Hairpin `get_property_sheet` — the one path-targeted `Filesystem` verb still not diverted. Gated on `RemoteVfsDescriptor::has_extended_properties()` (false today, and `RemoteVfs` doesn't remote the verb), i.e. it needs the "local property sheets in a remote session" feature, not just a hairpin arm.
- WSL is the one transport still bootstrapped non-login, so its agent never sources `~/.profile` and starts with a barer `PATH` than a `wsl.exe` shell would. It can't just take `-lc` like SSH: `WslLaunch` execs the agent directly with no handshake, so RPC starts on the first byte of stdout and any profile that prints a banner would corrupt the stream. Give WSL the same `NEWT:` handshake as `SpawnSpec::Bootstrap` (which skips non-`NEWT:` lines precisely to absorb login-shell chatter), then enable `login_shell` for it.
- Host-key prompts and "are you sure you want to add fingerprint" flows for SSH ride the existing askpass channel; verify the same UX with `StrictHostKeyChecking=accept-new` setups.
- Agent-VFS pane mounts aren't enriched (enrichment is session-level; the sub-agent's `--serve-vfs` mode has no enricher dispatcher). Needs per-VFS enricher routing, the way `Vfs` verbs remote, if it proves wanted.
- `Vfs`-level remoting of the property-sheet verbs (`API_VFS_*` constants + `RemoteVfs`/`VfsDispatcher` arms) — deferred until `LocalVfs` grows a sheet (xattrs); nothing crosses that layer today. (Design: `design_docs/DESIGN_VFS_PROPERTY_SHEETS.md`.)

## Platform locations (hot paths, promoted)

Design: `design_docs/DESIGN_PLATFORM_LOCATIONS.md`. **Not yet decided — awaiting a go/no-go on the design**; doctrine (platform mounts → `LocalVfs`, discovery/enrichment only, no native SMB/NFS/FTP clients) is settled, surfaces and scope are the open questions.

- Add `volume: Option<VolumeInfo>` to `HotPathEntry` (Windows collector already has it via `local_roots()` and discards it; Linux/macOS collectors gain the existing `volume.rs` probes).
- Broaden the Linux Mount collector: include by fs type (cifs/nfs/fuse.sshfs/…) regardless of prefix; enumerate + name-parse `/run/user/$UID/gvfs/`; keep the pseudo-fs blocklist.
- Classify macOS `/Volumes` entries via the statfs probe; dedupe the boot volume.
- VFS selector "Locations" section (mounts slice, volume icon/label/target treatment, eject on ×) + mount-table change events on Unix (`mountinfo` is pollable; macOS focus-sweep or DiskArbitration) feeding the existing refresh path.
- Breadcrumb/header enrichment via a pushed location-prefix → label map (open question whether v1).

## Persisted UI state (runtime-state / `state.json`)

- Persist window geometry (main + viewer/editor size/position/maximized) via `tauri-plugin-window-state`. Must handle the pre-warmed hidden viewer/editor windows (`PrewarmedWindow`, keyed per main-window label) so restore lands on the window that actually shows the file.

## Dialog visual uplift

- `HotPaths.module.scss` deleteBtn hover keeps an `opacity !important`; HistoryNavigator and SortMenu each keep two `!important`s fighting Menu.module's `data-highlighted` styling — all need a structural fix in Menu.module.scss to remove.

## Drag and drop

- Drag-out for non-host-local sources (S3/SFTP/remote sessions) needs materialization: either download-to-tempdir before the native drag starts (reuse the `download_and_open` pattern), or per-platform file-promise APIs (NSFilePromiseProvider / CFSTR_FILEDESCRIPTOR / XDS) — no cross-platform crate wraps those today.

## Viewer and editor follow-ups

- Editor (F4) is UTF-8 only. Reuse the viewer's encoding catalogue and sniffer (`viewer/encoding.rs`) on open, re-encode on save with the same encoding, and give the editor its own Encoding menu.
- Prev/next file navigation from the viewer window (`viewer_next_file`/`viewer_prev_file`, default `n`/`p`, arrows navigating at fit zoom in image mode). The keybinding side is ready (viewer commands live in the central registry); what remains is the session side — ask MainWindowState for the pane-order neighbor of the same class and re-target the window, generic across viewer modes.
- Decode-in-Rust fallback for formats the webview can't render (TIFF on Windows, HEIC off macOS, RAW via embedded-preview extraction). Big surface — deliberately deferred.
- SVG in image mode is degenerate (no natural size → transform-based zoom rasterizes blurry); a vector-aware path would size the `<img>` element instead of transforming it.
- Downscale quality: composited CSS transforms sample bilinearly with no mip chain on both WebKit and WebView2. If real-world images shimmer at fit zoom, swap in a pre-downscaled rendition below 100% (static images only).

## Archive follow-ups

Design: `design_docs/DESIGN_7Z_VFS.md` (newt) and
`~/src/iluvatar/design_docs/DESIGN_STREAM_ENGINE.md` (iluvatar).

- Implement `Vfs::revalidate` for the archive VFSes (zip, tar, 7z, compressed files). The trait is wired through to the navigation layer (called when a pane crosses into a VFS that advertises `VfsDescriptor::can_revalidate`); the archive impl should stat the origin file's mtime against the value captured at mount time and rebuild the index in place if it drifted, returning `Refreshed`. Mount identity (`VfsId`, `mount_meta`, `origin`) must be preserved so history entries remain valid; flip `can_revalidate` to true on the descriptors.
- Test 7z archives from other writers. The corpus is written by 7-Zip 26.03 on macOS (`sevenz/fixtures/regenerate.py`; zstd via py7zr, which 7-Zip cannot write); 7-Zip on Windows (attribute conventions, `\` names, NTFS times without the unix extension), p7zip, Keka and WinRAR's 7z are untested.
- PPMd folders: port ppmd-rust's Ppmd7 decoder (CC0/MIT-0) to the push model; no checkpoints (the model is the state), so a PPMd folder decodes from its start like ZIP's cursor path.
- Check that the pane surfaces the "Decoding · folder n of m" progress during a read, not only during mounting: a cold read deep into a huge solid block decodes forward once (about 1.5 s per 100 MB of packed data in a release build) before the first byte comes back.
- Split volumes (`.7z.001`) and SFX stubs: volumes need sibling reads through the upstream VFS.
- 7z writer: a BCJ x86 stage for executables (liblzma's filter chain has it; 7-Zip puts `.exe`/`.dll` through BCJ2, which we cannot write), and header compression/encryption (`-mhe`).
- Spool quota and memory threshold as preferences; today `SpoolConfig::default()` (32 MiB in memory, unlimited disk) is wired at session start.
- Folder index memory is bounded per folder (256 MiB budget), not per mount; a mount over many huge solid folders can add up. An LRU across folders would cap it.

## Disc image VFS follow-ups

- VAT/virtual and sparable partition maps (packet-written CD-RW/DVD±RW dumps) — currently a clean "unsupported" error.
- `.img` support via content sniffing: the extension is ambiguous (raw disk images with partition tables vs raw ISO9660/UDF), so claiming it needs a cheap probe before mount rather than an extension match.
- El Torito boot catalog: expose boot images as synthetic entries at the mount root.

## Unified VFS recursor + operations improvements (ideas, not started)

- **Unified VFS file recursor.** One shared recursive-walk primitive for everything that traverses trees today with hand-rolled loops: the operations scan/execute phases, the du enricher's `walk_entry`, and (candidate) the search walker. Bakes in the rules each copy has re-derived — never cross registry mount boundaries, don't follow directory symlinks, skip `/proc`, skip unreadable subtrees, the mount-point rule (device comparison, per-operation default, the never-rmdir-a-mount-point bookkeeping) — plus streaming output, progress reporting that fits all consumers (operations `Scanning` counters, `EnrichSink` running totals, `VfsProgress`), per-walk concurrency caps, and drop-based cancellation. Key trait addition: an *optional* flat recursive listing verb on `Vfs` (e.g. `list_files_recursive(prefix)` streaming all nested paths directly). Default: unimplemented — the recursor recurses level-by-level at a higher layer, consistent with the trait philosophy. S3 overrides it: a delimiter-less `ListObjectsV2` over a prefix *is* the flat listing, turning du / large prefix copies over S3 from one round-trip per pseudo-directory into a few paginated calls. Descriptor capability flag so the recursor picks the fast path statically.
- **Operation framework hardening.** (a) Write-to-temp + atomic rename for overwrites instead of truncate-in-place, on VFSes with `can_rename` — a cancelled/failed copy must never leave a half-written destination; temp naming + orphan cleanup on failure; S3 and friends keep the direct write (PUT is already atomic). (b) Richer conflict handling on the existing issue-resolution channel: keep-if-newer, skip-identical (size+mtime), rename-both.

## Distribution

- Gated on versioned releases rather than nightly snapshots: an AppStream metainfo file (`org.newt-fm.newt.metainfo.xml`, installed beside `newt.desktop`), which wants a real `<releases>` history. A security reporting policy belongs to the same milestone.

## Bug fixes and strengthening

- `TerminalHandle` is minted per session (`Local::new()` per `session.rs`), so every window's first terminal is handle 0. Nothing routes by handle alone today (`terminal_data` emits are window-scoped), but the next thing that does will cross-talk. Kill the class rather than the instance: either a process-wide counter, or make `TerminalHandle` carry its session. See "Window-targeted events" in CLAUDE.md.
- Local macOS is the one place the app's own `PATH` is patched by hand (`[environment] extra_path`) rather than inherited: a Finder-launched `.app` has no login shell above it. The visible seam: the terminal gets `-l` and so has the user's full `PATH`, while a Newt-spawned command gets launchd's plus `extra_path`, so a tool can work when typed and fail as a command. The cure is what VS Code does — probe once at startup (`$SHELL -ilc`, marker-delimited JSON on stdout so profile chatter can't corrupt it, bounded by a timeout since a profile can block forever on a prompt) and thread the result as a base env rather than `set_var` (edition 2024, and `shell.rs` deliberately doesn't mutate our own environment). Not worth it until the manual patching actually bites.
- Auto-remount VFSes (including dead agent mounts) when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.

## Major new features (groom/write design docs first)

- Batch rename (probably with enrichers preview)
- Compare & synchronize directories
- Custom styling / theming
