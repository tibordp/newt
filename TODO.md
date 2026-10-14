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

Design: `design_docs/DESIGN_PLATFORM_LOCATIONS.md`. Doctrine (platform mounts → `LocalVfs`, discovery/enrichment only, no native SMB/NFS/FTP clients) is settled. Volumes are classified and listed in the VFS selector, queried when it opens (no change events).

- Eject/unmount a volume from the VFS selector (×), dispatched by what mounted it: `gio mount -u` (gvfs), `udisksctl unmount` (media), `diskutil unmount` (macOS).
- Volumes of agent mounts (SSH, containers): the sub-agent has no Hot Paths provider.
- Breadcrumb/header enrichment via a pushed location-prefix → label map (open question whether v1).

## Session memory (`sessions.json`)

- Per-profile start directory (`start_directory` on spawn-style connection profiles): explicit, so it beats the remembered location, WinSCP-style.
- Record sessions when the OS ends the process on Windows (`WM_QUERYENDSESSION`/`WM_ENDSESSION` at logoff/shutdown) and Linux (SIGTERM from the session manager). Today only macOS's `applicationShouldTerminate:` path records; elsewhere a logoff loses whatever changed since each window last closed.

## Dialog visual uplift

- `HotPaths.module.scss` deleteBtn hover keeps an `opacity !important`; HistoryNavigator and SortMenu each keep two `!important`s fighting Menu.module's `data-highlighted` styling — all need a structural fix in Menu.module.scss to remove.

## Per-platform look

- A platform token layer (`:root[data-platform=…]`) over the shared design, with components never branching on platform: the system UI font and size, the system accent colour (`AccentColor` in WebKit on macOS, `UISettings` on Windows, the appearance portal's `accent-color` on GNOME/KDE), GNOME's flat borderless buttons (`--color-control-*`), per-platform control heights. Window chrome is the one structural difference: a unified title bar on macOS.

## UI reviews

- Review the file-list icons as a whole. Today a name maps through the seti theme's `mapping.json` (`src/lib/fileIcons.ts`), and anything unmapped gets the plain text-lines glyph — executables, installers and app bundles included. Known gaps: `.exe`/`.msi`/`.com`/`.cmd` are unmapped though seti's `windows` glyph (used for `.bat`) fits; the font's `apple` glyph (U+E002) is unused and suits app bundles; a package (`Annotation::Package`) shows its name's file glyph, which reads oddly for `.app`; an extensionless Unix executable has no icon of its own — seti has none, nerd-symbols has `cod-file_binary` / `cod-terminal` but in an outline style next to seti's filled ones; installers and images (`.dmg`, `.pkg`, `.deb`, `.rpm`, `.apk`) are unmapped. Decide one consistent source (seti only, or seti plus a matching subset of nerd-symbols), whether the lookup takes the mode (execute bits) and package-ness besides the name, and what a non-`.app` package (`.rtfd`, a photo library) shows. The drag ghost (`getFileIconChar` in `Pane.tsx`) should follow whatever the list does.
- Rethink how background operations are presented. The panel at the bottom of the main window was the quickest way to get them working, not a considered design: where running, finished and failed operations live, how a prompt waiting on the user gets noticed, and what stays on screen once they're done.

## Drag and drop

- Drag-out for non-host-local sources (S3/SFTP/remote sessions) needs materialization: either download-to-tempdir before the native drag starts (reuse the `download_and_open` pattern), or per-platform file-promise APIs (NSFilePromiseProvider / CFSTR_FILEDESCRIPTOR / XDS) — no cross-platform crate wraps those today.

## Viewer and editor follow-ups

- Alternate view (Alt+F3, reserved): F3, but always opening in text/hex whatever the file's type. Quick View's Mod+Shift+F3 is provisional.
- Editor (F4) is UTF-8 only (other files open read-only). Reuse the viewer's encoding catalogue and sniffer (`viewer/encoding.rs`) on open, re-encode on save with the same encoding, and give the editor its own Encoding menu.
- `WriteOptions::replace` on local Windows: `ReplaceFileW` keeps attributes, ACLs and alternate data streams, and refuses where it can't (hard links via `GetFileInformationByHandle`'s link count). Editor saves write in place there until then.
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

## Exclusive writes

Design: `design_docs/DESIGN_EXCLUSIVE_WRITES.md`.

- SFTP answers `NotSupported` to `create_new` and `rename_no_replace`, so copies and moves to SFTP still stat every destination. `create_new` is `SSH_FXF_EXCL` (the refusal is a bare `SSH_FX_FAILURE`, classified by an lstat, as `rename` already does). The no-replace rename needs the plain v3 `SSH_FXP_RENAME`, which `openssh-sftp-client`'s `Fs::rename` never sends to a server offering `posix-rename`; its low-level write end is private, so this wants an upstream change.
- Custom S3 endpoints never offer `create_new`. Verify which stores honour `If-None-Match` on PutObject, CompleteMultipartUpload and CopyObject (MinIO, R2, Ceph RGW, …) and allow them.
- Confirm on Windows (derived from MS-FSA, not run): `CREATE_NEW` over a directory fails as access denied, classified to `AlreadyExists` by `create_new_refusal`; `rename_no_replace` over a file and a directory, and a case-only rename; `copy_within` under `create_new` (reservation, then `CopyFileExW` over it).

## Distribution

- Gated on versioned releases rather than nightly snapshots: an AppStream metainfo file (`org.newt-fm.newt.metainfo.xml`, installed beside `newt.desktop`), which wants a real `<releases>` history. A security reporting policy belongs to the same milestone.

## Accessibility

- Live announcements: navigation finishing, operations completing or failing, selection counts, "Loading…" states — one polite live region fed from the existing flows, rather than per-component `aria-live`.
- Screen-reader modes for the terminal (xterm.js `screenReaderMode`) and editor (Monaco `accessibilitySupport`, whose `auto` can't detect a reader inside a webview): detect a running reader natively (`NSWorkspace.isVoiceOverEnabled`, `SPI_GETSCREENREADER`, `org.a11y.Status.ScreenReaderEnabled` on D-Bus), publish it in state, and let a preference override it.
- Keyboard gaps: Settings' category sidebar is click-only (tabs have roles but no key handling); the key-capture field's clear (×) button acts on mousedown only, and Enter/Space on it is captured as a key.
- Regression check: the scene harness can assert ARIA snapshots (e.g. the focused file row is the list's active descendant) and run axe-core over every scene.

## Bug fixes and strengthening

- `TerminalHandle` is minted per session (`Local::new()` per `session.rs`), so every window's first terminal is handle 0. Nothing routes by handle alone today (terminal output travels on each session's own IPC channel), but the next thing that does will cross-talk. Kill the class rather than the instance: either a process-wide counter, or make `TerminalHandle` carry its session. See "Window-targeted events" in CLAUDE.md.
- Local macOS is the one place the app's own `PATH` is patched by hand (`[environment] extra_path`) rather than inherited: a Finder-launched `.app` has no login shell above it. The visible seam: the terminal gets `-l` and so has the user's full `PATH`, while a Newt-spawned command gets launchd's plus `extra_path`, so a tool can work when typed and fail as a command. The cure is what VS Code does — probe once at startup (`$SHELL -ilc`, marker-delimited JSON on stdout so profile chatter can't corrupt it, bounded by a timeout since a profile can block forever on a prompt) and thread the result as a base env rather than `set_var` (edition 2024, and `shell.rs` deliberately doesn't mutate our own environment). Not worth it until the manual patching actually bites.
- A pane's `file_window.items` is diffed by array index, so when entries sort in above the window (a streaming search, a refresh) every row reads as changed, the patch overflows `MAX_PATCH_OPS`, and the whole state is re-sent and every row re-rendered — about every 100 ms during a large search. Diff the window by entry key instead (keyed array moves in the patch, or the window sent outside the generic treediff), keeping `deepUpdate`'s reference preservation per row.
- Scroll anchoring (`PaneViewState::keep_cursor_row`) decides whether the cursor was on screen from the viewport the pane last reported, which it only reports near the window's edges, so near the screen's top and bottom the call can go either way. Exact needs the first visible row on every row scrolled, without recomputing the window each time.
- Auto-remount VFSes (including dead agent mounts) when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.

- Bump `specta` / `specta-typescript` / `tauri-specta` past rc.22 / 0.0.9 / rc.21 once a release includes the fix for [specta-rs/specta#498](https://github.com/specta-rs/specta/issues/498): rc.25 overflows the stack exporting `serde_json::Value`. The port itself is small — `Type::inline`/`reference` become `definition(&mut Types)` (`specta_as!`, `common::Error`, the wire `PathBuf`), `TypeCollection` becomes `Types` in the `FunctionArg` impls, container `#[specta(rename)]` becomes `#[serde(rename)]`, `FileChunk.data` needs `#[specta(type = Vec<u8>)]` beside its `serde_bytes`, and `BigIntExportBehavior::Number` becomes the builder's `dangerously_cast_bigints_to_number()`.

## 1.0 readiness (audit, October 2026)

Release engineering:

- One version source (`src-tauri/Cargo.toml`, `package.json` and `tauri.conf.json` each say 0.1.0 independently), a tag-driven release workflow, and a mapping onto deb (`1.0.0~rc.1`), rpm (no `-`) and MSI (numeric only) versions. Same-version snapshots don't upgrade under apt/dnf.
- Sign Windows builds (MSI, `newt.exe`, and `newt-agent.exe`, which UAC shows as "Unknown publisher" for elevated sessions).
- An update check against the `downloads/latest.json` CI already publishes.
- `publish` rewrites `latest.json` from the current run alone, so a lean run drops the Intel Mac, ARM Windows and ARM Linux downloads a full run published. Merge instead.
- CI on `macos-latest` and `windows-latest` (clippy + tests); today only Linux builds on push. Pin the toolchain (`rust-toolchain.toml`).
- macOS: fail the release when signing secrets are missing, verify with `codesign --verify --deep` / `spctl` / `stapler validate`, set `minimumSystemVersion` (the frontend uses `color-mix()`, Safari 16.2, while Vite targets `safari13`). Trash goes through Finder AppleScript (trash-rs `DeleteMethod::Finder`) under the hardened runtime without `NSAppleEventsUsageDescription`; consider `NsFileManager`. Guide users to Full Disk Access on TCC-denied paths.
- The remote agent cache (`scripts/bootstrap.sh`, `newt-agent-${NEWT_HASH}`) isn't keyed by triple, so a home shared between architectures execs the wrong agent forever. The cleanup loop also deletes other versions' agents, and the `/tmp/newt-$(id -u)` fallback (and `/tmp/newt` in `agent_cache_dir()`) accepts a directory another user created first. A raw-upload `head -c` short read leaves a truncated agent that is reused.
- Elevated sessions from the AppImage: root can't exec the agent from the FUSE mount. `NEWT_AGENT_DIR` also leaks into terminal shells.
- Settle identifiers before 1.0: keychain service `com.newt.credentials` vs `org.newt-fm.newt`, `newt.desktop` vs reverse-DNS.
- Linux packages depend on libappindicator, which the binary doesn't link; no CI step test-installs a package.
- `cargo deny` (licences, advisories; bincode 1.3 is unmaintained), and WiX 6 → 7 wording in `newt.wxs` / `build-msi.ps1`.
- On Windows, `--help`/`--version` print nothing (GUI subsystem), the MSI doesn't add to PATH and is per-machine only.
- `cargo tauri build` from source bundles the 0-byte stub agents from `build.rs` without complaint; `agent_hash` reads ~100 MB of agents with blocking I/O on every connect.
- Write supported OS versions and Install/Update/Troubleshooting/Uninstall into FEATURE_DUMP for the manual.

Correctness and UX:

- File clipboard: Mod+C/Mod+V are path-text only, and paste after copying in Explorer/Finder errors or navigates to a bare name.
- Files opened externally from remote/archive VFSes are temp copies never uploaded back (edits lost silently) and never deleted; a failed launch only logs.
- Case-insensitive destinations: `Makefile` and `makefile` from a case-sensitive source collide mid-operation (likewise NFC/NFD and Windows trailing dots); detect before writing.
- Linux trash on a root-owned mount falls back to copy-then-delete into `~/.local/share/Trash` with no progress or cancel.
- Non-UTF-8 file names can't be operated on (lossy wire paths).
- Huge directories: sync commands do O(n) work on the main thread shared by all windows, and a streaming listing is re-cloned and re-sorted every 100 ms under the view-state lock.
- The remote connection log (stderr of an agent run at `RUST_LOG=info`, including full operation-request Debug dumps) grows without bound and is re-serialised on every publish.
- macOS file watching roots an FSEvents stream at `/` per pane per wait.
- Two instances clobber each other's `state.json`; `UpdatePublisher::publish` can emit out of order (the frontend sees a version gap and re-requests full state).
- Properties: owner/group by name silently does nothing; Windows has no attribute editor.
- Terminal: the keys that escape xterm are hardcoded, so rebinding `focus_panes`/`toggle_terminal_panel` strands focus; Close/Clear/Select All are menu-only; font, size and scrollback aren't settings.
- Window close/quit kills busy terminals without asking; `Mod+Q` is Ctrl+Q off macOS.
- Copy/Move can't take a relative destination (`../backup/`) and Create Directory doesn't create `a/b/c`; errors there are native popups over the dialog.
- Errors carry no path ("Permission denied (os error 13)"); file names aren't bidi-isolated (RLO spoofing).
- Light theme `--color-fg-muted` (#929292, 3.1:1 on white) is below WCAG AA; about #6a6a6a would clear 4.5:1 on both the background and the chrome.
- No undo for file operations; no OS notification when a background operation finishes or stops on a prompt; no minimum window size.
- Bookmarks have no Settings tab (the dialog-is-the-file rule); Mod+H doesn't persist.
- Alt+F5 is taken by GNOME; F11 is Show Desktop on macOS.
- The Connect dialog lists no profiles when `connections.toml` doesn't parse, with only a log line; show the parse error there instead.
- Files materialised from downloads/archives get no Mark-of-the-Web or quarantine; the file server sends `Access-Control-Allow-Origin: *` (it needs the webview's origin, which is `tauri://localhost`, `http://tauri.localhost` on Windows, or the dev server).
- Windows named pipes (elevation, shell control, askpass) use the default DACL, readable by Everyone; `/tmp` socket names are predictable.
- Host-side RPC dispatch trusts a compromised remote: clipboard read/write via shell control, askpass prompts with arbitrary text, `API_HOST_FETCH_AGENT` triple path joins, a 4 GiB frame reservation.
- FEATURE_DUMP drift: Open Config File (says built-in editor), Escape (says clears selection), Rename (says full name), terminal paste, Settings categories, `[environment]`, hidden files, ownership.

## Major new features (groom/write design docs first)

- Reveal in Newt / New Newt Window Here from Finder, Explorer, Nautilus, Dolphin; single instance (`design_docs/DESIGN_FILE_MANAGER_INTEGRATION.md`)
- Compare & synchronize directories
- Custom styling / theming
