# TODO

## Shell integration (`newt` CLI in built-in terminals)

Design: `design_docs/DESIGN_SHELL_INTEGRATION.md`.

- `behavior.shell_integration = false` gates local sessions only; agents always create the control server (they can't see host preferences). Propagate the flag at agent spawn (env for bootstrap transports like `NEWT_AGENT_MODE`, an arg for direct spawns) if disabling remotely turns out to matter.
- `newt cp --wait` (long-poll an operations endpoint for completion, exit code from the operation result).
- Per-terminal pane affinity (`NEWT_TERMINAL` is already injected), `--json` output, user-command invocation by title.
- Deeper shell integration: a script injected into bash, zsh, fish and PowerShell that emits prompt markers (OSC 133: prompt, command start, command end) and reports the shell's directory, for a two-way working-directory sync between a terminal and its pane. Busy detection rides on it — the foreground process group (`tcgetpgrp` on the PTY) on Unix, the shell's child processes on Windows, both answered on the agent in a remote session — so the Close Window prompt can name what is running (`vim`, `make`) and leave idle shells alone; today it counts every open terminal.

## Remote sessions and agent mounts

Design: `design_docs/DESIGN_AGENT_VFS_MOUNTS.md`, `design_docs/DESIGN_ENRICHERS_AND_RESOURCES.md`.

- Hairpin `get_property_sheet` — the one path-targeted `Filesystem` verb still not diverted. Gated on `RemoteVfsDescriptor::has_extended_properties()` (false today, and `RemoteVfs` doesn't remote the verb), i.e. it needs the "local property sheets in a remote session" feature, not just a hairpin arm.
- WSL is the one transport still bootstrapped non-login, so its agent never sources `~/.profile` and starts with a barer `PATH` than a `wsl.exe` shell would. It can't just take `-lc` like SSH: `WslLaunch` execs the agent directly with no handshake, so RPC starts on the first byte of stdout and any profile that prints a banner would corrupt the stream. Give WSL the same `NEWT:` handshake as `SpawnSpec::Bootstrap` (which skips non-`NEWT:` lines precisely to absorb login-shell chatter), then enable `login_shell` for it.
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

## UI reviews

- A platform token layer (`:root[data-platform=…]`) over the shared design, with components never branching on platform: the system UI font and size, the system accent colour (`AccentColor` in WebKit on macOS, `UISettings` on Windows, the appearance portal's `accent-color` on GNOME/KDE), GNOME's flat borderless buttons (`--color-control-*`), per-platform control heights. Window chrome is the one structural difference: a unified title bar on macOS.
- Rethink how background operations are presented. The panel at the bottom of the main window was the quickest way to get them working, not a considered design: where running, finished and failed operations live, how a prompt waiting on the user gets noticed, and what stays on screen once they're done.

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

- SFTP answers `NotSupported` to `create_new` and a `no_replace` rename, so copies and moves to SFTP still stat every destination. `create_new` is `SSH_FXF_EXCL` (the refusal is a bare `SSH_FX_FAILURE`, classified by an lstat, as `rename` already does). The no-replace rename needs the plain v3 `SSH_FXP_RENAME`, which `openssh-sftp-client`'s `Fs::rename` never sends to a server offering `posix-rename`; its low-level write end is private, so this wants an upstream change.
- Custom S3 endpoints never offer `create_new`. Verify which stores honour `If-None-Match` on PutObject, CompleteMultipartUpload and CopyObject (MinIO, R2, Ceph RGW, …) and allow them.

## Distribution

- Gated on versioned releases rather than nightly snapshots: an AppStream metainfo file (`org.newt-fm.newt.metainfo.xml`, installed beside `org.newt-fm.newt.desktop`), which wants a real `<releases>` history. A security reporting policy belongs to the same milestone.

## Bug fixes and strengthening

- Local macOS is the one place the app's own `PATH` is patched by hand (`[environment] extra_path`) rather than inherited: a Finder-launched `.app` has no login shell above it. The visible seam: the terminal gets `-l` and so has the user's full `PATH`, while a Newt-spawned command gets launchd's plus `extra_path`, so a tool can work when typed and fail as a command. The cure is what VS Code does — probe once at startup (`$SHELL -ilc`, marker-delimited JSON on stdout so profile chatter can't corrupt it, bounded by a timeout since a profile can block forever on a prompt) and thread the result as a base env rather than `set_var` (edition 2024, and `shell.rs` deliberately doesn't mutate our own environment). Also, something similar for WSL on Windows, the terminal shell is not a login shell and may be missing PATH entries. Something holistic and robust is in order for PATH handling (see above)
- Directory watching drops changes between watches: `poll_changes` is one-shot, so `Pane::watch_changes` drops the watcher on the first event, spawns the refresh and only then arms the next watcher (path canonicalisation, a new watcher, one watch per ancestor). A change after the refresh has listed but before the new watch is up raises nothing, and the pane stays stale until the next change — seen once on Windows as a deleted directory lingering after a multi-item delete (the directory goes last, after its contents). The next watch has to be up before the refresh lists: a long-lived change stream rather than one-shot polls, across the agent boundary too. Belongs with a restructuring of the pane's navigation state machine (`navigate_impl`, `refresh_queue`, the navigation mutex, `nav_changes_rx`), which has grown by accretion.
- Auto-remount VFSes (including dead agent mounts) when navigating into a dead history entry. Today such entries render correctly (cached display path, "unmounted" badge, skipped during overlay stepping) but jumping to one fails. Needs mount metadata stored on the history entry so the navigation can transparently re-establish the connection.

- Bump `specta` / `specta-typescript` / `tauri-specta` past rc.22 / 0.0.9 / rc.21 once a release includes the fix for [specta-rs/specta#498](https://github.com/specta-rs/specta/issues/498): rc.25 overflows the stack exporting `serde_json::Value`. The port itself is small — `Type::inline`/`reference` become `definition(&mut Types)` (`specta_as!`, `common::Error`, the wire `PathBuf`), `TypeCollection` becomes `Types` in the `FunctionArg` impls, container `#[specta(rename)]` becomes `#[serde(rename)]`, `FileChunk.data` needs `#[specta(type = Vec<u8>)]` beside its `serde_bytes`, and `BigIntExportBehavior::Number` becomes the builder's `dangerously_cast_bigints_to_number()`.

## Non-UTF-8 names

Non-UTF-8 names (and lone surrogates on Windows) are converted lossily, so they can act on the wrong file: a listing's `a\xff` beside a real `a�` both act on `a�`, and copying, moving or packing a symlink whose target isn't UTF-8 (`operation/walk.rs` takes the `Symlink::Target` string) makes a link to a different path. The lossy sites: `PathBuf::from_native`/`native_segments`, `local.rs` dirent names and `read_symlink`, SFTP names and link targets (`openssh-sftp-client` hands over a `std::path::Path`, lossless on a Unix host), zip and 7z link targets (`from_utf8_lossy` on the entry's bytes), tar member names (read-only). Undecided between:

- **Refuse**: make `from_native`/`native_segments` fallible (`to_str()`), and fail the listing instead of `to_string_lossy`, so panes show an error and operation walks hit the Retry/Skip prompt; a target that won't decode becomes `Symlink::Unknown`, which walks already ask about. Small, but one such name makes its whole directory unbrowsable.
- **Encode losslessly in the `String`** (as WTF-8 / Python's `surrogateescape` do): each byte that isn't valid UTF-8 becomes U+10FF00 + byte (the tail of Supplementary Private Use Area-B), and a literal code point from that range has its own bytes escaped the same way, so the mapping is injective; `to_native` turns them back into bytes (`OsStr::from_bytes`; on Windows a hand-written WTF-8 decode into `OsString::from_wide`, never `from_encoded_bytes_unchecked`, since typed input can hold anything). `Path`, the frontend, bindings, `settings.toml` and the RPC stay as they are; the escapes survive a trip through S3 and back; they are all ≥ 0x80, so `is_valid_name` holds. Display (`format_vfs_path`, the name cell) shows them as `\xNN`, and so should text export (Copy Path, the clipboard's text flavour), or a pasted path misses the file. Raw-byte tests only run on Linux (APFS refuses such names). Switching `Path` to `[u8]` instead is no better: the frontend, TOML and clipboard text would still need this same encoding at every edge.

## 1.0 readiness (audit, October 2026)

Release engineering:

- One version source (`src-tauri/Cargo.toml`, `package.json` and `tauri.conf.json` each say 0.1.0 independently), a tag-driven release workflow, and a mapping onto deb (`1.0.0~rc.1`), rpm (no `-`) and MSI (numeric only) versions. Same-version snapshots don't upgrade under apt/dnf.
- Sign Windows builds (MSI, `newt.exe`, and `newt-agent.exe`, which UAC shows as "Unknown publisher" for elevated sessions).
- CI on `macos-latest` and `windows-latest` (clippy + tests); today only Linux builds on push. Pin the toolchain (`rust-toolchain.toml`).
- macOS: fail the release when signing secrets are missing, verify with `codesign --verify --deep` / `spctl` / `stapler validate`, set `minimumSystemVersion` (the frontend uses `color-mix()`, Safari 16.2, while Vite targets `safari13`). Trash goes through Finder AppleScript (trash-rs `DeleteMethod::Finder`) under the hardened runtime without `NSAppleEventsUsageDescription`; consider `NsFileManager`. Guide users to Full Disk Access on TCC-denied paths.
- Elevated sessions from the AppImage: root can't exec the agent from the FUSE mount. `NEWT_AGENT_DIR` also leaks into terminal shells.
- No CI step test-installs a Linux package.
- `cargo deny` (licences, advisories; bincode 1.3 is unmaintained).
- `cargo tauri build` from source bundles the 0-byte stub agents from `build.rs` without complaint.
- Write supported OS versions and Install/Update/Troubleshooting/Uninstall into FEATURE_DUMP for the manual.

Correctness and UX:

- Linux trash on a root-owned mount falls back to copy-then-delete into `~/.local/share/Trash` with no progress or cancel.
- Huge directories: sync commands do O(n) work on the main thread shared by all windows, and a streaming listing is re-cloned and re-sorted every 100 ms under the view-state lock.
- Properties: owner and group take numeric IDs only (no account names); Windows has no attribute editor.
- Bidi formatting characters show as U+FFFD in the file list, the path bar, Delete, Copy/Move, Properties and operation prompts, but not yet in the window title (set from Rust), Rename, Quick View, the viewer and editor, or palettes.


## Major new features (groom/write design docs first)

- Reveal in Newt / New Newt Window Here from Finder, Explorer, Nautilus, Dolphin; single instance (`design_docs/DESIGN_FILE_MANAGER_INTEGRATION.md`)
- Compare & synchronize directories
- Custom styling / theming
