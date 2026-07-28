# Newt Feature Dump

Exhaustive inventory of every user-facing feature in Newt, a keyboard-centric dual-pane file manager built with Tauri 2.

---

## 1. Application Layout

The main window is divided into two zones by a resizable vertical splitter:

- **Top zone**: Two file panes side by side (horizontal splitter), each showing a directory listing.
- **Bottom zone**: Terminal panel (collapsible) with a tab bar for multiple terminals.

Both splitters are user-resizable by dragging the divider. Clicking splitter dividers does not steal focus from the file list or terminal.

Additional overlay elements:
- Operations panel (shows background file operations and progress).
- Askpass dialog (SSH/sudo password prompts, overlays the main window).
- Connection status indicator (during connect/disconnect/reconnect).
- Modal dialogs (all driven by Rust state — never by React `useState`).

### Dialog system

All dialogs share a common visual language and a set of frontend primitives (`src/main_window/modals/primitives/`):

- **DialogShell / DialogHeader / DialogBody / DialogFooter**: structural skeleton. The body scrolls while the header and footer stay pinned (matters for tall content like Properties and the Connection Log). The footer is a chrome-tinted strip with a hairline top border; it hosts the right-aligned Cancel/primary buttons plus an optional left `start` slot for secondary controls (e.g. Copy's "Pack into archive…", Properties' "Apply recursively").
- **Field / FieldGroup / CheckboxField / FieldRow**: form layout primitives (stacked label+control with optional hint, tight checkbox clusters, inline label+control rows).
- **DialogTabs**: shared tab strip (Connect transports, archive formats, Settings sections).
- **DialogSubmitButton**: submit with spinner + pending label; `variant="destructive"` renders the red primary used by delete confirmations. **DialogError**: inline error banner. Both pair with `useAsyncAction` for single-flight async submits.
- No backdrop scrim — dialogs separate from the panes via a deep elevation shadow (`--shadow-dialog`) plus a strong border, keeping open/close instant and the panes fully readable (deliberate: dimming reads web-modal, and macOS/KDE/Win32 dialogs don't dim).
- Floating containers (centered dialogs, top-anchored palettes, settings editor, progress modal, askpass) share elevation/border/radius via Sass mixins in `src/styles/_dialog-mixins.scss`.
- Type sizes come from `--font-size-xs/sm/md/lg` tokens; a `--font-mono` token covers log/transcript surfaces.

### Theming

`appearance.theme` (`system` / `light` / `dark`) resolves to a `tauri::Theme` and reaches the UI by three separate routes, which is worth knowing because they fail independently:

- **Page colors** come from CSS reacting to `prefers-color-scheme`. Nothing sets `data-theme` today — the attribute is an escape hatch the cascade honours if something ever does. The media query reflects the *app* theme rather than the OS because the native theme is pushed into the webview (below).
- **Title bar / window decorations** follow `WebviewWindow::set_theme`, kept live by `spawn_theme_sync` subscribing to preference changes.
- **UA-rendered chrome — scrollbars, native form controls** — comes from the CSS `color-scheme` property, declared on `:root` in `_reset.scss`. Chromium takes these from the *used* color scheme, which defaults to light no matter what `prefers-color-scheme` says, so WebView2 rendered light scrollbars against a dark page until the property was declared. WebKit (macOS, webkitgtk) infers them from the system appearance, so this only ever showed on Windows.

Platform mechanics behind route 1: on macOS the window's `NSAppearance` propagates to the `WKWebView`; on Linux the GTK theme variant does the same for webkitgtk. On **Windows** the webview color scheme is `ICoreWebView2Profile::SetPreferredColorScheme` — **process-wide**, and set only at webview *creation* (`WebviewWindowBuilder::theme`), since `set_theme` after the fact reaches only the tao window. Two consequences: every window must be built with the resolved app theme or it stamps the shared profile back (see `build_child_window` — a prewarmed F3/F4 window otherwise reset every window's webview), and `detect_theme()` is Linux-only, so `theme = "system"` on Windows passes `None` and leaves the profile on `AUTO`, which follows the OS by itself.

### Locale

Numbers, dates and times are formatted with an explicitly resolved locale rather than whatever the webview would pick: `appearance.locale` when set, else the system's **regional format** — `GetUserDefaultLocaleName` on Windows, `CFLocaleCopyCurrent` on macOS, `LC_ALL`/`LC_NUMERIC`/`LANG` on Linux, normalised to a BCP-47 tag (`de_DE.UTF-8@euro` → `de-DE`). `C`/`POSIX` resolve to nothing, leaving the runtime default. It arrives as `ResolvedPreferences.locale` and is passed to every `Intl`/`toLocale*` call.

The explicitness is load-bearing on Windows, which separates the *display language* from the *regional format*: wry initialises WebView2 from `GetUserDefaultUILanguage` (the display language), so a bare `toLocaleString()` formats US-style for anyone running an English UI in a non-US region. macOS and Linux don't split the two, which is why it only ever showed there.

`date_format` / `time_format` pick the date *layout*; the locale still supplies month and weekday names, so `%B` is `Januar` under `de-DE`.

Prefixed sizes are localised the same way, so the decimal separator agrees with the exact byte counts beside them — `1,5 GB` under `de-DE`.

The tag is checked for well-formedness against `Intl` itself (`usableLocale`) before any formatting call, and falls back to the runtime default when it doesn't pass. That guard is load-bearing rather than defensive: settings are written on every keystroke, so typing `sl-SI` puts `s`, `sl` and `sl-` through the file list's formatting in turn, and `Intl` answers a malformed tag with a `RangeError` instead of degrading. It equally covers a system locale the engine won't take. Validation is delegated to the engine that consumes the value rather than hand-rolled from the BCP-47 grammar, so the resolved value in `ResolvedPreferences.locale` is deliberately *not* pre-validated on the Rust side.

### Size Units

Every displayed byte count goes through one formatter (`lib/size.ts`), so base and prefixes always agree: `appearance.size_units = "decimal"` (default) gives powers of 1000 with SI prefixes — `kB`, `MB`, `GB` — and `"binary"` gives powers of 1024 with IEC prefixes — `KiB`, `MiB`, `GiB`. Values carry up to two decimals with trailing zeros dropped, in the resolved locale.

It applies wherever a size appears: the Size column (when `si_size_prefixes` is on), free space in the pane header and VFS selector, operation progress and transfer speed, the Properties dialog, and the viewer and editor status bars. The Properties dialog additionally shows the exact byte count in parentheses, and the Size column shows exact counts by default.

### Zoom

- **Mod+=** (or **Mod++**): Zoom in.
- **Mod+-**: Zoom out.
- **Mod+0**: Reset zoom to default.

Zoom is applied via the webview zoom factor and persisted app-wide in the runtime-state file (`state.json`, `zoom` key): it survives reloads, new windows (including viewers/editors) start at it, and changing it in one window follows through to all others via the runtime-state broadcast.

### Window Management

- **New Window** (Mod+N): Opens a fresh Newt window (new session).
- **Close Window** (Mod+Shift+W): Closes the current window. Shifted (gnome-terminal convention) so a session window can't be closed on a stray Mod+W. Viewer/editor children can't outlive the session they were spawned from, so closing a main window takes them along: a Rust-side `CloseRequested` interceptor (covers the command, the menu item, and the OS close button alike) closes viewer children outright and sweeps editor children — dirty ones prompt, the window close resumes once the last child editor is gone, and refusing a prompt aborts the close.
- **Quit Newt** (Mod+Q): Closes all main windows, exiting the app. Open editor windows are swept first so their unsaved-changes prompts fire: the quit waits (event-driven, via the `Destroyed` handler) for the last editor to close before the main windows go, and refusing any editor's prompt aborts the quit (`cancel_quit`), leaving all remaining windows open. Pre-warmed hidden editors are exempt from the sweep. OS-initiated termination (macOS Dock quit, logout) goes through the same sweep via an `applicationShouldTerminate:` method injected into tao's app delegate at runtime (`terminate_guard` in `main.rs` — tao doesn't implement the selector and `RunEvent::ExitRequested` never fires for OS termination, see tauri#9198): a dirty editor cancels the termination and runs the sweep, a clean app terminates immediately so it never blocks logout. Editor dirty state is mirrored to Rust for this via `set_editor_dirty`. The `ExitRequested { code: None }` handler remains as a safety net for the all-windows-closed path.
- **Reload Window**: Available in the Debug dialog (debug builds only).
- **Refresh File List** (Mod+R): Force-refreshes the active pane's directory listing.

Multiple main windows coexist in the same process: "New Window", remote connections, and elevated sessions all create additional windows within the running app. Each window has its own independent session, panes, terminals, and operations; closing the last main window exits the app.

### Main Window Menu Bar (macOS only)

Main windows carry a minimal native menu on macOS (Windows/Linux main windows have no menu bar). Menu items dispatch window-scoped `menu-command` events to the frontend, which routes them through the same `executeCommandById` path as keybindings, and item accelerators are derived from the *resolved* keybindings at build time — a task subscribed to the preferences watcher rebuilds all main-window menus on preference changes, so rebinds are reflected live.

- **App menu**: About Newt, Settings… (Mod+,), Quit Newt (Mod+Q). Deliberately no Hide/Hide Others — their fixed Cmd+H would shadow Toggle Hidden Files.
- **File**: New Window, Connect to Remote Host…, Close Window.
- **Edit**: predefined Cut/Copy/Paste/Select All (required so macOS routes Cmd+C/V/X/A to the webview — previously the sole content of the main-window menu).
- **Window**: predefined Minimize, Zoom.

Viewer and editor windows prepend an app submenu with Quit Newt (Cmd+Q) on macOS, so quit works whichever window is focused.

---

## 2. Dual-Pane File Browser

Each pane is an independent file browser with its own path, selection, filter, sort order, and navigation history.

### Pane Header

- **VFS selector dropdown**: Shows the current filesystem type (Local, S3, SFTP, Archive name). Click to open a dropdown listing all mounted VFSes and available mount options. Mounted VFSes sort first and show an unmount (×) button; below a separator, unmounted types (S3, SFTP, Remote) appear as ellipsis entries ("S3…") that open the respective mount dialog. "Remote…" opens the full Connect dialog with "Open as a new session" defaulted off (pane mount) regardless of session mode.
- **Breadcrumb path**: Current directory path displayed as clickable breadcrumb segments. Clicking any segment navigates to that directory. Clicking the *last* segment opens the Navigate (Go To) dialog instead of navigating. Breadcrumb format varies by VFS type:
  - Local: `/home/user/documents`
  - S3: `s3://bucket/prefix/key`
  - SFTP: `sftp://hostname/path`
  - Archives: origin path + inner path, e.g., `/home/user/file.tar.gz/dir/subdir`
- **Git branch badge**: When the pane's directory is inside a git repository, a quiet muted-text badge appears between the breadcrumbs and the free-space indicator: seti git glyph + branch name (short commit id when detached), a `*` suffix when the repo has uncommitted changes, and `↑N`/`↓M` ahead/behind counts when an upstream exists. Produced by the git enricher (see "Enrichers" below).
- **Free space indicator**: Shows available disk space (e.g., "123.4 GB free") when the filesystem reports stats. Link-styled; clicking opens the volume-root Properties dialog (`root_properties`), whose Volume section carries the full detail (volume classification — local disk / removable / optical / network / RAM disk / subst — filesystem name, label, target, capacity/used/free). The dialog targets the volume the stats actually describe (`VolumeInfo.mount_point` — on Unix the containing mount point, e.g. `/proc`, not `/`; on Windows the drive/share root). Volume info is probed on the FS-owning side per listing (`VolumeInfo` on `FsStats`): Win32 volume APIs on Windows, `/proc/self/mountinfo` + `/dev/disk/by-label` on Linux, `statfs` on macOS. Only visible for VFS types that support `fs_stats` (local filesystem). Directory Properties invoked *at* a volume root shows the same Volume section.

The whole header can be hidden via the `appearance.show_pane_header` preference (default: on). When hidden, the VFS selector trigger stays mounted in an off-screen anchor so its keyboard shortcut still opens the dropdown anchored to the top of the pane.

### Pane Status Bar

Bottom strip on each pane showing loading state, the current file's full display path, file/directory counts, total size of selection or directory, and a "(partial)" marker when the listing is windowed. Toggled by the `appearance.show_pane_status` preference (default: on).

### File List

Server-side windowed list with 22px fixed row height. Rust sends only a ~150-item window around the current viewport; the frontend renders all window items directly with simple spacer divs. Enables smooth performance with directories of 100k+ files.

**Default columns** (all sortable by clicking the header):

| Column | Width | Alignment | Content |
|--------|-------|-----------|---------|
| Name | 250px | Left | File type icon (color-coded, VSCode icon set) + filename |
| Size | 100px | Right | Locale-formatted byte count — or SI-prefixed ("1.5 GB") when `appearance.si_size_prefixes` is on (default off) — "DIR" for directories, "???" if unknown |
| Modified Date | 80px | Right | Date of last modification |
| Modified Time | 80px | Right | Time of last modification |
| User | 70px | Left | Owner name (or numeric UID if name unavailable) |
| Group | 70px | Left | Group name (or numeric GID) |
| Mode | 70px | Left | Unix permissions string, e.g., `drwxr-xr-x` |

**Additional columns** (available via settings, not shown by default):

| Column | Content |
|--------|---------|
| Extension | File extension only |
| Modified / Accessed / Created | Compound date + time in one column (145px) |
| Accessed/Created Date and Time | Access / creation timestamp, date-only or split |
| Attr | Windows `FILE_ATTRIBUTE_*` letters: `R`eadonly `H`idden `S`ystem `A`rchive + `L` (reparse point) `C`ompressed `E`ncrypted; sortable by raw bits |
| Link Target | Symlink target path |

**VFS-aware columns**: the `appearance.columns` preference defines one global superset + order, but each pane filters it by the metadata its VFS actually populates (`VfsDescriptor::metadata_traits`, surfaced per pane as `PaneViewState::metadata_traits`): User/Group/Mode render only on `unix_owner` VFSes (Unix-shaped local/remote/agent mounts, SFTP, tar and zip archives, disc images), Attr only on `windows_attributes` ones (Windows-shaped local/remote mounts — incl. the client-local mount in remote sessions; the raw attribute bits ride `File::attributes` across RPC). S3 and search results show neither. The header context menu hides inapplicable choices for that pane; the settings widget still edits the full global list; header drag-reorder weaves the reordered visible keys back through the config so hidden-on-this-pane columns keep their global positions.

Compound-column swaps: when the Extension column is visible, the Name column automatically shows just the file stem (name without extension). The timestamp columns follow the same pattern — a compound column (`modified` etc.) shows date + time in one cell and swaps down to date-only when the paired Time column is also in the list. Each timestamp thus has four presentations: compound date & time, date only (`modified_date`), separate date and time columns (`modified_date` + `modified_time`, or equivalently `modified` + `modified_time` via the swap — the default for Modified), and hidden.

Column visibility and order are configurable via the `appearance.columns` preference, edited from three places that all write the same list:

- **Header context menu** (mouse-only): right-clicking the column header row opens a menu where simple columns are checkboxes and each timestamp (Modified/Accessed/Created) is a submenu with radio choices — Date & time / Date only / Separate columns / Hidden — with the current state shown on the submenu trigger. Newly enabled columns are inserted at their canonical position relative to the currently visible ones (timestamp rewrites happen in place). The Name column cannot be removed; the menu stays open so several columns can be flipped in one visit.
- **Header drag-to-reorder**: dragging a column header horizontally (5px threshold, so plain clicks still sort) shows an accent insertion marker and dims the dragged header; dropping persists the new order to the preference. Dragging onto the column's own position is a no-op (marker hidden).
- **Settings dialog widget**: a full-width row (title/description on top) with two side-by-side panels below — Visible columns in display order (each with a drag handle: mouse drag with live preview, or focus it and use ArrowUp/Down), Hidden columns greyed out beside them. Simple columns are checkboxes; timestamps are a checkbox plus a presentation dropdown (Date & time / Date only / Separate columns). Checking a hidden timestamp enables the compound presentation.

Timestamp columns honor the `appearance.date_format` / `appearance.time_format` preferences — strftime-style format strings (supported specifiers: `%Y %y %m %d %e %j %b %B %a %A %H %I %M %S %p %%`; month/weekday names are locale-aware). Empty (the default) falls back to the system locale rendering. Compound columns render as "date time" (`date_format` + `time_format`, or the locale's combined rendering when both are unset); split Date/Time columns each use their own format. The same formats apply to the timestamps in the Properties dialog.

Column widths are resizable by dragging the grip between column headers (minimum 10px during the drag). Widths persist per pane and per column key in the runtime-state file (`state.json`, see Configuration) — not in `settings.toml` — and are restored on window reload / new windows; a zero-movement click on the grip writes nothing. Double-clicking a grip auto-sizes its column Excel-style: the width fits the widest currently rendered cell (the list is virtualized, so this is the visible window plus overscan, not the whole directory) with the header's own labels as a floor, clamped to ≥30px, then persists. Widths are applied via per-pane CSS custom properties; runtime-state updates broadcast app-wide, so the same pane slot in other windows follows (last write wins).

Click a column header to sort ascending by that key; click the same header again to toggle descending. A triangle indicator (▲/▼) shows the active sort column and direction.

**Sort menu (keyboard, Mod+Shift+S)**: opens a compact menu anchored under the pane header listing every sort key, the current one marked with its ▲/▼. Each row has an underlined accelerator letter (and 1–9 by position); pressing it sorts by that key and closes the menu — a poor-man's chord, so `Mod+Shift+S` then `m` sorts by Modified. Pressing the same key again reverses direction (same toggle as a header click), so `Mod+Shift+S m m` flips to descending. **Holding Shift arms reverse**: `Shift+<key>` sorts by that key *descending* outright (`Mod+Shift+S`, then Shift+M → Modified descending), and while Shift is held the Reverse row lights up (with a `Shift` hint badge) to show it's in effect. Arrow keys + Enter also work; the highlight seeds on the current key so a bare Enter reverses it. `R` reverses without changing the key; `F` toggles "Folders first" (mutating the `appearance.folders_first` preference, like column reordering mutates prefs). Esc dismisses; focus returns to the pane. The menu is per-pane and keyboard-launched only (no click target). The command (`sort`) is rebindable and appears once in the command palette.

**Available sort keys**: Name, Extension, Size, User, Group, Mode, Modified, Accessed, Created.

**Sort behavior**:
- The `..` parent directory entry *always* sorts first, regardless of sort key or direction.
- When "Folders first" is enabled (preference), directories sort before files (but after `..`).
- Extension sort treats directories as having no extension.
- Name and extension sorting is **case-insensitive** (Unicode `to_lowercase`), with a stable byte-order tiebreaker for entries differing only by case.

**Visual indicators**:
- Selected files: distinct background highlight.
- Focused (cursor) file: different highlight from selection.
- Hidden files: dimmed styling. Hidden-ness is platform-native — the leading-`.` convention on Unix, the filesystem `HIDDEN`/`SYSTEM` attributes on Windows.
- Symlinks: special styling (CSS class).
- Git status: filename color per working-tree status (VSCode-style palette, theme-aware) — modified/deleted-under = amber, untracked/added/renamed = green, conflicted = red, ignored = dimmed muted. Directories carry rollups of everything beneath them. See "Enrichers" below.
- `..` parent directory: always shown at the top, even when hidden files are hidden or a filter is active. Cannot be selected, and is never an operation target: it is a navigation affordance only. It can still be *focused*, so Delete / Copy / Move / Pack / Properties with `..` under the cursor and nothing selected do nothing at all — no confirmation, no dialog. Two accessors enforce this, and actions must read their targets through one of them: `Pane::effective_keys` (selection, else focus) for the bulk operations, and `PaneViewState::actionable_focus` for the ones that deliberately ignore the selection and act on the cursor (Rename). Navigation is the deliberate exception — Enter and open-in-other-pane read `focused` / `get_focused_*` and see `..`, which is the entire point of it. View/Edit no-op on `..` already, via their `is_focused_dir` guard.

**Status bar** (bottom of each pane) — content changes dynamically:

| State | Display |
|-------|---------|
| Loading (first 200ms) | (nothing — grace period to avoid flicker) |
| Loading (after 200ms) | "Loading... (X items so far)" |
| Loaded, no selection | "X files, Y directories", plus ", N hidden" when hidden files exist but Show Hidden is off |
| Loaded, with selection | "X files, Y directories selected, Z bytes total" — Z includes computed recursive sizes of selected directories, so ⌘A after Calculate All Sizes totals the whole directory |
| Filter active | "(showing X of Y)" appended |
| Partial results | "(partial)" appended when directory listing was truncated |

### Directory Loading

Large directories are loaded incrementally via streaming:
- First batch clears old state (filter, selection, focus).
- Intermediate batches update the visible file list and statistics in real time — the user can browse and interact while loading continues.
- Navigation to a new directory auto-cancels any in-progress load for the same pane.
- A 200ms grace period suppresses the loading spinner to avoid flicker on fast directories.

### Enrichers

Background annotation of directory listings (design: `design_docs/DESIGN_ENRICHERS_AND_RESOURCES.md`). An enricher computes extra per-entry information (`Annotation`s keyed by entry key) and per-location `ContextBadge`s, streamed into a pane-side overlay that is merged into rows at window projection time. The pane layer is fully generic — annotations are opaque to it (`FileView.annotations`); only the frontend interprets the kinds it knows how to render, and the preference↔enricher-id gating lives with the toggles in the preferences schema (`EnricherPreferences::disabled_enrichers`).

**Lifecycle — anchored to the history cursor**: automatic enrichers (git) start when a navigation lands and restart on every refresh (recompute-per-refresh replaces cache invalidation); annotations survive refreshes (re-keying onto surviving rows) but are cleared by any navigation. History navigation (Alt+arrows, history-dialog jumps) restores them stale-while-revalidate: each history entry captures the overlay as it stood when that view was left, the restore happens at landing before the automatic rerun is signalled, so git supersedes its part deterministically while computed du sizes simply reappear as they were (returning to a past view is an explicit acceptance of that view's snapshot). Fresh navigation to the same path does not resurrect anything; snapshot lifetime rides history retention. An automatic rerun supersedes the previous generation wholesale (first batch carries a `reset` flag), so entries that stopped matching (e.g. a committed file) don't linger. Manual enrichers (du) run on their own lane: triggered by keybinds, never restarted by refreshes, and their runs *accumulate* within a visit (sizing one directory doesn't clear a previously sized sibling). Esc on a loaded pane and navigating away both cancel the runs in flight reliably; already-applied annotations stay.

**In-flight visibility**: while an enricher runs, the pane status bar shows its activity label ("git status… (Esc cancels)"), with a 200ms appearance delay so fast runs don't flash it.

**Architecture**: symmetric across the host↔agent boundary on the operations template — an `Enrichers` registry (`newt_common::enrich`) lives next to the `VfsRegistry` (host-side in local sessions, agent-side in remote ones), fronted by an `EnricherClient` with Local/Remote impls. Static, inventory-collected `EnricherDescriptor`s (the `VfsDescriptor` analogue: id, activity label, automatic, `applies_to_vfs`) are linked by both sides, so the host selects which enrichers to run for a pane (descriptor gate against the pane's VFS × preference gate) and the request names them explicitly — an empty selection sends no request at all, so S3/search/archive/agent-mount panes cost nothing. Data-dependent applicability (is this a repo?) lives inside the enricher; a run that finds nothing still sends an empty reset batch, clearing stale annotations. The remote path streams events via `Notify(API_ENRICHMENT_EVENT)` correlated by `EnrichmentId`; cancellation is by dropping the request future (transport-level `InvokeCancel` aborts the agent-side run). Producer-side batching at 100ms.

**Du enricher (recursive directory sizes)**: manual-only, two commands — **Calculate Size** (Mod+Shift+Enter; sizes the selected/focused directories) and **Calculate All Sizes** (Shift+Alt+Enter; sizes every entry). There is deliberately no directory-total badge — selection totals include computed sizes, so ⌘A reads the directory total off the existing status line. Walks via the `Vfs` trait on the side that owns the filesystem, so it works on any VFS (S3, archives, SFTP, remote sessions) and never crosses registry mount boundaries; up to 16 entry walks run concurrently, each a serial DFS. Running totals stream into the size column ncdu-style (in-progress values dimmed with a trailing `+`, flipping to plain when a subtree completes); Esc freezes whatever was computed as marked partials. Computed sizes participate in sort-by-size (a live walk re-sorts in place while the order is active). Directory symlinks are not followed, `/proc` is never descended into, unreadable subtrees are skipped. Sizing matches `du`: allocated bytes (`File.allocated_size`, `st_blocks`-based) when the filesystem reports them — sparse files (VM disk images, Docker.raw) count what they occupy, not their apparent size — with apparent-size fallback on VFSes without block metadata (S3/SFTP/archives, where the two coincide). Hardlinked files are counted once per sized entry (`(device_id, inode)` dedup gated on `hard_links > 1`), and the walk never crosses filesystem boundaries (`du -x` semantics; a mountpoint entry reports the mounted filesystem's device, so sizing the mountpoint itself works and stops at further nested mounts). All four fields are unix-only `File` metadata (`None` on Windows and non-local VFSes).

**Git enricher**: shells out to the `git` binary where the files live — the remote host's git in remote sessions; no gitoxide/libgit2 dependency. A cheap `.git` walk-up (directory or file, covering worktrees) guards the spawn, so non-repo directories never pay for a git process (their run just emits the empty reset batch). One `git --no-optional-locks status --porcelain=v2 -z --branch --ignored=matching` run at the repo root yields branch/ahead-behind/dirty for the badge and repo-wide per-file statuses for row coloring, with directory rollups (precedence: conflicted > modified > renamed > added > untracked; ignored never rolls up). `--no-optional-locks` keeps status from writing `.git/index` and re-triggering the pane watcher. The status taxonomy is deliberately coarse: copied (`C`) renders as renamed, submodule status changes as modified, and deleted-only directories roll up as Modified. Toggled by `enrichers.git_status` (default on); toggling takes effect immediately (preference changes re-run automatic enrichers).

### Keyboard Navigation

| Key | Action |
|-----|--------|
| Arrow Up/Down | Move focus one item |
| Shift+Arrow Up/Down | Move focus and extend selection |
| Page Up/Down | Jump one viewport height |
| Shift+Page Up/Down | Jump one viewport height with selection |
| Home | Jump to first item |
| End | Jump to last item |
| Enter | Open file or enter directory (see "Enter behavior" below) |
| Backspace | Navigate to parent directory (`..`) |
| Tab | Switch active pane |
| Insert | Toggle selection on current file and advance focus to next |
| Mod+A | Select all files (except `..`) |
| Mod+D | Deselect all |
| (unbound) | Invert Selection (command palette) — toggles every visible entry; filtered-out selections are left alone |
| Escape | Clear filter text, or clear selection if no filter active |
| Shift+Enter | Follow symlink (navigate to its target) |
| Shift+\<drive letter\> | Jump to that drive's root. Offered whenever the session has a split-root (Windows drive-lettered) mount — `MainWindowState.mount_summary.has_split_root_vfs`, refreshed on mount/unmount — not gated on the host OS. With no such mount the keystroke types into the quick filter as usual. |

**Enter behavior** depends on what's focused:
- **Directory**: Navigate into it.
- **Archive file** (`.tar.gz`, `.zip`, etc.) or **disc image** (`.iso`, `.udf`): Mount as VFS and navigate into its root.
- **Symlink to directory**: Enter it logically — the pane keeps the link path, listing goes through the link (both Unix and healthy Windows links, `mklink /D`/`/J`). On Windows-shaped FSes, if the logical entry fails, Enter falls back to the resolved link *target*: the app-compat junctions (`C:\Users\<user>\Cookies` & co.) carry an `Everyone:(DENY)(RD)` ACE and are unlistable by design, so resolving is the only useful move (TC resolves; Salamander errors). Deliberately shallow: direct navigation to a link path (Ctrl+L, breadcrumb) is unchanged and may error.
- **Regular file**: Open with system default application. For host-local files this opens directly via the OS opener; for files on a non-host-local VFS (S3, SFTP, archives, remote), the file is first downloaded to a temp directory on the host using the standard Copy operation, and the system handler is launched on completion.
- **`..`**: Navigate to parent directory.

### Mouse Interactions

| Action | Behavior |
|--------|----------|
| Left click | Focus the clicked file |
| Mod+Click (Ctrl; ⌘ on macOS) | Toggle selection for that file (keeps other selections) |
| Shift+Click | Range select from focused file to clicked file |
| Double-click | Open/enter (same as Enter key) |
| Right-click | If clicked file is NOT selected: focus it (clears selection), show context menu. If clicked file IS selected: keep selection, show context menu. |
| Drag on empty area | Rectangle (marquee) selection — see below |
| Drag on file icon/name | Initiate drag-and-drop to other pane — see below |

**Rectangle (marquee) selection**:
- Must drag at least 5px before the rectangle appears (prevents accidental activation on click).
- A blue selection rectangle is drawn; files overlapping the rectangle are selected.
- Auto-scrolls when dragging near the top or bottom edges of the pane (44px zone). Speed increases closer to the edge.
- **Mod+Drag** (Ctrl; ⌘ on macOS): Adds rectangle selection to existing selection.
- **Shift+Drag**: Selects range from focused file to drag endpoint.
- **Normal drag**: Replaces entire selection with rectangle contents.

**Pane activation**: Clicking anywhere in a pane makes it the active pane for keyboard commands.

### Context Menu

The default browser context menu is suppressed in the main window (but not in the viewer/editor). Text inputs retain their native context menus.

**Right-click a file** or press Shift+F10 / Menu key:

| Item | Shortcut |
|------|----------|
| Open | |
| View | F3 |
| Edit | F4 |
| Copy Path | Mod+C |
| Rename | F2 |
| Delete | F8 |
| Delete Permanently | Shift+Delete |
| Open in Terminal | Mod+Enter |
| Properties | Alt+Enter |
| Windows Menu | Shift+RClick (Windows host + host-local files only) |

**Right-click empty space** in the file list:

| Item | Shortcut |
|------|----------|
| Open in Default App | Shift+F3 (host-local VFS only) |
| New Directory | F7 |
| New File | |
| Directory Properties | |
| Windows Menu | Shift+RClick (Windows host + host-local files only) |

**Windows shell context menu** (Windows host, host-local files): the trailing "Windows Menu" item — or Shift+right-click to skip our menu entirely — pops the classic `IContextMenu` shell menu (not the Windows 11 abbreviated one) for the effective selection; on empty space (or the `..` row) it targets the current directory itself. Implementation (`main_window/shell_menu.rs`, `shell_context_menu` command): runs synchronously on the main thread (`TrackPopupMenuEx` pumps its own modal loop), `SHParseDisplayName` on de-verbatimed paths (`launch_cwd`) → parent `IShellFolder::GetUIObjectOf` → `QueryContextMenu` → `TrackPopupMenuEx(TPM_RETURNCMD)` → `InvokeCommand`, with the Tauri window temporarily subclassed to forward `WM_INITMENUPOPUP`/`WM_DRAWITEM`/`WM_MEASUREITEM`/`WM_MENUCHAR` to `IContextMenu2/3` so dynamic submenus ("Open with", "Send to") populate. User-cancelled verbs (`ERROR_CANCELLED`) are not errors; shell-side mutations (delete, rename, …) reach the pane through the directory watcher. In **elevated sessions** the menu is still built and invoked by the non-elevated UI process — items the desktop user can't read (e.g. `C:\Windows\System32\config\*`) fail at `SHParseDisplayName` with an error, and any verb that did run would run non-elevated. Deliberate: an agent-side menu (feasible for the UAC transport, which shares the interactive desktop) would invoke *every* verb elevated — "Open"/"Open with" silently spawning admin-token processes — which is why Explorer refuses to run elevated too. Power users who need shell verbs on admin-only files can just run `newt.exe` itself elevated.

**Right-click a breadcrumb** in the path bar:

| Item | Description |
|------|-------------|
| Copy Path | Copies the display path up to that breadcrumb segment |

### Drag and Drop

- Drag one or more files from one pane to the other by clicking and dragging the file icon or name.
- **Multi-file drag**: If multiple files are selected, dragging any selected file drags them all. A ghost preview shows "N items" at the cursor.
- **Drop targets**: Drop on a folder to copy/move into it. Drop on the pane background to copy/move to the pane's current directory.
- **Modifier keys**: Normal drop = copy. Shift+drop = move.
- **Visual feedback**: Drop target pane/folder highlights on hover.
- **Same-pane restrictions**: Cannot drop a folder onto itself.

**External drag-and-drop IN** (from the OS file manager):
- Drag one or more files from the OS file manager (Nautilus, Dolphin, Finder, etc.) and drop onto any pane.
- **Drop on pane background**: Copies files to the pane's current directory.
- **Drop on a folder row**: Copies files into that folder.
- **Visual feedback**: Pane background or folder row highlights as the cursor moves, same styling as internal drag-and-drop.
- **Always copies**: External drops always create copies (no modifier key for move).
- **Requires host-local VFS**: Only works when a local filesystem VFS is mounted (to resolve source paths).

**External drag-and-drop OUT** (to other applications):
- Dragging past the window edge hands the internal drag off to a native OS drag session (`drag` crate; copy-only). Drop into Finder/Explorer/Nautilus, other apps, or **another Newt window** (arrives there via the normal external-drop path).
- **Host-local files only**: every dragged file must resolve to a host-local path. Search panes escalate when the underlying files are local (per-file check after source deref). S3/SFTP/remote-session drags stay internal — leaving the window behaves as before (release outside cancels, re-entering resumes the ghost).
- **Preview**: the drag carries a rendered pill matching the internal ghost (file icon + name, or "N items"); falls back to the app icon.
- **Drop back into the same window**: routed through internal DnD semantics — copy, dropping into the source directory is a no-op, dropping onto a dragged folder degrades to the pane background.
- Known platform gaps: Windows paths >260 chars crash upstream (drag-rs #76); Linux X11 can silently fail on some GTK3 setups (#84); Wayland behavior varies by compositor.

### Focus Preservation

- When navigating to a new directory, the first file is focused by default.
- When navigating **back** (Alt+Left, mouse back button), the previously focused file is restored from the popped history entry.
- When exiting an archive VFS via `..`, the archive file itself is focused in the parent directory.
- On refresh (e.g., after file system changes), existing selection and focus are preserved if the files still exist.
- Selection state survives filter changes in Filter mode (but not in Quick Search mode).

---

## 3. Filtering and Search

Two filter modes for narrowing the visible file list within a pane. The default mode when typing is controlled by the `quick_search` preference (default: true). When disabled, typing goes directly to Filter mode.

### Quick Search Mode

- **Activation**: Start typing any printable character (not modified with Ctrl/Shift/Alt) while the pane is focused. Requires `quick_search = true` (default).
- **Matching**: Case-insensitive **prefix** matching on filenames. Wraps around the file list (searches downward from cursor, then wraps to top).
- **Live updates**: Results update as you type. The cursor moves to the first match.
- **Arrow Left/Right**: Adjusts the search string based on the focused file's name. Right extends the search to include more of the focused filename; Left trims it.
- **Press the filter key** (`/` by default): Switches to full Filter mode, keeping the current search text.
- **Cleared by**: Escape, any selection action, or navigating to a different directory.

### Filter Mode (Visual Regex)

While rows are hidden, the file list carries a faint accent tint (`--color-bg-filtered`) as a standing reminder that the view is partial. Applies to Filter mode with a non-empty pattern, the only state that hides entries.

- **Activation**: Press the filter key (`/` by default), switch from Quick Search with the same key, or start typing when `quick_search = false`.

  Both routes run the one `start_filter` command — "switch this pane to filter mode, keeping whatever is typed" — so it is rebindable in the Keybindings tab and runnable from the command palette like anything else. That matters for non-US layouts: `/` is Shift+7 on QWERTZ, and the binding is matched as an exact normalized string, so those layouts capture `shift+/` instead. Unlike other commands it is matched inside the pane rather than dispatched from the window-level handler — the pane's own keydown runs first and routes every unmodified printable character into quick-search, so a key left to the dispatcher would be swallowed before reaching it.
- **UI**: A filter input bar appears at the bottom of the pane.
- **Matching**: Full **regex** pattern matching (case-insensitive). Files that don't match are hidden entirely.
- **`..` always visible**: The parent directory entry is never hidden by a filter.
- **Focus follows the matches**: a non-empty pattern moves the cursor to the first match whenever it would otherwise sit on `..` or nowhere — including when the entry it was on stops matching. A cursor already on a surviving entry stays put, so refining a pattern never yanks it away, and an empty pattern hides nothing and moves nothing.
- **Status bar**: Shows "(showing X of Y)" when filtering.
- **Selection persists**: Selection is retained even for files hidden by the filter. However, operations only act on *visible* selected files (`get_effective_selection()`).
- **Cleared by**: Escape clears the filter text and shows all files. Navigating to a different directory clears the filter.

### Differences Between Modes

| Behavior | Quick Search | Filter Mode |
|----------|-------------|-------------|
| Matching | Prefix, case-insensitive | Regex, case-insensitive |
| Non-matching files | Still visible | Hidden |
| Selection clears filter | Yes | No |
| Visual indicator | No bar | Filter bar at pane bottom |
| Navigation clears | Yes | Yes |

---

## 4. File Operations

### Create Directory (F7)

Modal dialog with a single name input field (auto-focused). Creates a new directory in the active pane's current path.

### Create File (from command palette)

Modal dialog with a name input. Creates an empty file in the current directory.

### Create and Edit (Shift+F4)

Creates a new file (same dialog as Create File) and immediately opens it in the built-in editor.

### Rename (F2)

Modal dialog with the current filename pre-filled and fully selected (so you can type a new name immediately). Operates on the **focused** entry, ignoring the selection — and does nothing at all when that is `..` (see the `..` note under File List). Runs as an operation (`OperationRequest::Rename`) with the same two-step execution as Move: native `Vfs::rename` when the VFS supports it, else copy+delete — so S3 objects and prefixes can be renamed (server-side CopyObject, no data through the app). Conflicts raise the standard Skip / Overwrite prompt; the fallback shows real progress and is cancellable. Renaming to the byte-identical name is a no-op, and renaming to a *different spelling of the same name* — `Foo` → `foo` on a case-insensitive volume, NFC → NFD on HFS+ — is a rename rather than a conflict: the destination stats successfully as the source, so `Vfs::same_file` is consulted before the conflict probe and suppresses it. Renaming one **hard link** onto another takes the same exemption and is then a no-op — POSIX has `rename` succeed without acting when both names are links to one file, so both survive and nothing is reported. That is BSD `mv`'s behaviour (GNU `mv` refuses instead); it's rare, and inaction is the safe end of the trade. Copying one hard link onto another is still refused outright, since that one would destroy the file. The pane refreshes and re-focuses the new name when the operation completes.

### Delete (F8 / Del / Cmd+Backspace) and Delete Permanently (Shift+Delete / Opt+Cmd+Backspace)

Deletes all selected files and directories (recursive for directories).

**Symlinks and Windows junctions are removed as links, never followed.** `File::is_dir` reports the *target's* type for a link — that's what lets a pane enter one — so every recursive operation (delete, recursive chmod, recursive property apply) classifies its top-level path with `is_dir && !is_symlink` instead, matching what the directory walk has always done for children. On Windows a directory symlink or junction counts as a directory to the Win32 API, so `DeleteFileW` refuses it; `LocalVfs::remove_file` falls back — only once the plain delete has already failed, so ordinary deletes pay nothing — to `RemoveDirectoryW` for a directory-shaped reparse point, which removes the link and not its target.

- If `behavior.delete_to_trash` is enabled (default: yes), plain Delete moves items to the OS trash instead of deleting them. Only real local filesystems have a trash: the local FS, the remote host's FS in SSH/elevated sessions (freedesktop `~/.local/share/Trash` on the remote machine), and agent mounts — always the trash of the machine that owns the files. S3/SFTP/archive/search VFSes have no trash.
- **Delete Permanently** (`delete_permanent`, Shift+Delete, ⌥⌘⌫ on macOS, also in the context menu) always bypasses the trash.
- If `behavior.confirm_delete` is enabled (default: yes), a confirmation dialog appears first. For a trash delete it offers three choices: **Move to Trash** (default, focused), **Delete Permanently**, and Cancel.
- If the trash preference is on but the selection isn't trashable (e.g. on S3), a dialog explains the items will be deleted permanently and offers **Delete Permanently** / Cancel — this dialog is shown even when `confirm_delete` is off, since the recoverability expectation would otherwise be silently violated.
- If nothing is selected, the focused file is the target (unless it's `..`).

**Trash execution**: each top-level item is trashed wholesale (`Vfs::trash_item`) and counts as one progress item — no scan walk. Failures raise the standard Skip/Retry prompt. The operations panel shows the kind as `trash` ("Moving N item(s) to Trash").

**Permanent delete strategies** (tried in order):
1. **Fast tree removal**: If the VFS supports atomic `remove_tree()`, deletes the entire tree in one call.
2. **Manual tree walk**: Depth-first traversal — deletes files first, then directories bottom-up.

### Copy (F5)

Opens a modal dialog with:

- **Destination path**: static label showing the other pane's directory (not an input — the destination is always the other pane).
- **Summary**: Shows the filename (single file) or "N items" (multiple selection).
- **New name** field (single item only, copy and move): pre-filled with the source's leaf name; edit it to land under a different name in the destination (`rename_to` on the operation — same-VFS moves rename directly to the new name, and the copy fallback plans the tree under it). Left unchanged it sends nothing. Also names the symlink when "Create symbolic link" is checked. Empty or separator-containing input greys out the primary button, since the value becomes a single leaf under the destination. *Which* characters separate comes from the destination's `PathStyle` (`name_separators` on the modal payload), not from the frontend: `\` is rejected on a Windows-styled destination and accepted on a Unix one, where it is an ordinary filename character.
- **Options** (checkboxes):
  - **Create symbolic link** — only available for single-file copies. Creates a symlink at the destination pointing to the source. Disables the other options when checked.
  - **Preserve timestamps** — maintains file modification and access times.
  - **Preserve owner** — maintains UID.
  - **Preserve group** — maintains GID.
  - The three preserve toggles are **sticky**: the last-used values are remembered in `state.json` (`copy_move.*`) and seed the next Copy/Move. "Create symbolic link" is deliberately not sticky — a remembered value would silently change what Copy does.
- **Pack into archive…** button (copy only): swaps the dialog for Pack to Archive over the same selection.

**Copy execution**:

0. **Self-destination check**: before any scanning, an operation whose destination *is* one of its sources fails outright — no conflict prompt, since there is no sane resolution (Overwrite would open the destination truncating while the source read was still pending, emptying the file). Byte comparison can't answer this — `/a/Foo` and `/a/foo` are one file on a case-insensitive volume, as are NFC and NFD spellings on HFS+ — so the filesystem is asked via `Vfs::same_file`. Copy refuses every spelling of a self-copy, matching `cp`'s "'x' and 'x' are the same file". Costs one filesystem question per distinct source directory, since a source can only land on itself when its own directory is the destination.
1. **Planning phase**: Recursively traverses all source directories to build a complete file list. The UI shows "Scanning..." with a live count of items and bytes discovered so far. Subdirectory scan errors raise a skip/retry prompt rather than aborting the whole operation.
2. **Conflict detection**: For each file, checks if the destination already exists:
   - File → File: Offers Skip/Overwrite.
   - Directory → Directory: Merges (copies contents into existing directory without error).
   - File → Directory or Directory → File (type mismatch): Error, offers Skip.
3. **Copy strategies**:
   - Same-VFS `copy_within` (fastest, if available — kernel-assisted: FICLONE/`copy_file_range`/`CopyFileEx`, S3 server-side CopyObject).
   - Streaming copy otherwise: async read → async write in 64 KB chunks, drop-cancellable at every await.
4. **Metadata preservation**: After copying, optionally sets permissions, timestamps, owner, and group on the destination. Silently skipped if the destination VFS doesn't support metadata operations. The source side reads `Vfs::get_metadata`, whose default derives from the listing entry — so any VFS that surfaces mode/owner/timestamps in listings (tar, Rock Ridge/UDF disc images, S3 mtimes) feeds preservation without its own implementation; local and SFTP override it with a real stat.
5. **Progress**: Reports every 100ms with bytes done, items done, and current filename.

**Symlink handling**: With "Create symbolic link" checked, creates a symlink directly (no file content copied). Only available for single files.

**Cross-VFS copies**: Fully supported. You can copy files between any combination of Local, S3, SFTP, and Archive VFS types — every VFS speaks the same async read/write surface, so any readable source streams into any writable destination.

### Move (F6)

Same dialog and options as Copy (except "Create symbolic link" is not available).

**Move execution**:
0. **Self-destination check**: as for Copy, but Move refuses only the true no-op — same file *and* a byte-identical leaf name. A differing leaf (`Foo` → `foo` in place on a case-insensitive volume) is a re-spelling, which is legitimate and falls through to the rename below. This is the `mv Foo foo` works / `cp Foo foo` fails split that coreutils has.
1. **Try fast rename** (same VFS only): Attempts atomic rename for each source. Instant if it works. The rename path also performs conflict detection — if the destination already exists, the same Skip / Overwrite prompt as Copy is shown rather than silently overwriting. A destination that resolves to the *source itself* is exempt: on a case-insensitive volume `Foo` → `foo` stats successfully as the source, and that is the point of the move rather than an obstacle to it. `Vfs::same_file` is consulted only once something is actually in the way, so bulk moves stay at one probe apiece. An approved overwrite still goes through the plain rename (atomic replace on POSIX and posix-rename SFTP servers); only if the backend refuses with "already exists" is the destination cleared and the rename retried. Directory-onto-existing-directory goes straight to the copy machinery, which merges.
2. **Fallback to copy+delete**: Only a `NotSupported` rename — the VFS has no rename, or this particular pair can't be renamed (cross-device inside the root VFS, cross-VFS) — falls back to copying each file and immediately deleting the source after successful copy. Real rename failures (permissions, connection) raise a Skip/Retry issue instead of silently degrading. After all files are copied, empty source directories are removed in reverse order (deepest first). Directories that still contain files (because some copies were skipped) are left intact. The same rule governs the same-VFS server-side copy fast path: `copy_within` falling over with `NotSupported` (e.g. S3 CopyObject's 5 GiB cap) cascades to streaming; real errors surface as issues.

### Pack to Archive (Alt+F5)

Packs the active pane's selection into a new archive in the other pane's directory. Fully streaming through the VFS layer — archive bytes are produced chunk-at-a-time and written straight to the destination, so there are **no temp files and no whole-archive buffering**, regardless of which side is remote (local→S3, S3→local, remote-session sources, etc. all stream end-to-end).

Opens a modal dialog with:

- **Format tab bar**: `zip`, `tar`, `tar.gz`, `tar.xz`, `tar.zst`. Switching formats swaps the extension on the name field.
- **Archive name** (auto-focused, stem pre-selected): defaults to the single selection's stem, or the containing directory's name for multi-selections. Empty or separator-containing input greys out the primary button — the archive is a single leaf in the destination, so to write it into a subdirectory you navigate the other pane there. Which characters separate comes from the destination's `PathStyle` (`name_separators`, shared with the Copy/Move dialog), so `\` blocks on a Windows-styled destination and is a legal name character on a Unix one.
- **Destination** display (read-only, the other pane's directory).
- **Compression level** (per-format, seeded from the `[archives]` preferences): gzip/xz/deflate 0–9, zstd 1–22; zip level 0 stores entries uncompressed. Hidden for plain tar. Each format remembers its own level while the dialog is open.
- **Preserve symlinks** (default on, seeded from preferences): stores symlinks as symlink entries. When off, symlinks are followed — symlinked files are stored as regular files, symlinked directories are descended into (with cycle detection; a cycle raises a skip prompt).
- **Password** (zip only, optional, with confirm field): WinZip AES-256 (AE-2) encryption. Opens in 7-Zip/WinRAR/Keka and Newt's own archive VFS (lazy askpass); not in Windows Explorer or macOS Archive Utility.

**Writers** (in-tree `newt-archive` crate, sans-IO streaming state machines):

- **tar**: ustar with pax extended headers when needed (long/unsplittable paths, long link targets, files ≥ 8 GiB, large uid/gid, pre-epoch or sub-second mtimes). Preserves mode, uid/gid (or uname/gname), and mtime from the source dirent; sensible defaults (0644/0755, archive-creation time) when the source VFS has no such metadata (e.g. S3).
- **zip**: streaming data-descriptor mode (no seeking — this is what makes append-only sinks like S3 multipart possible), UTF-8 names, per-entry zip64 committed up front from the scanned size, zip64 EOCD for >65k entries or >4 GiB offsets, unix modes in external attributes, symlink entries, extended-timestamp extra field. DOS times are written as UTC.

**Execution**:

1. **Planning phase**: same recursive scan as Copy (live "Scanning…" counts, skip/retry on unreadable subdirectories). The destination archive itself is excluded from the walk (overwriting an archive that sits inside the selection doesn't pack its stale self). Duplicate top-level names across sources fail up front rather than silently colliding inside the archive.
2. **Conflict detection**: if the destination file exists, offers Skip (cancels the operation — single artifact) / Overwrite.
3. **Per-entry streaming**: sources are opened *before* their header is committed, so open failures offer Skip/Retry cleanly. A read error mid-entry finalizes the entry as truncated (tar zero-pads to the declared size) and offers Skip only — the stream can't rewind. Files that grow or shrink between scan and pack are truncated/padded with a logged warning, matching GNU tar's "file changed as we read it" spirit.
4. **Failure/cancel cleanup**: the partial archive is removed best-effort; an S3 multipart upload is aborted (also on drop — writers discarded mid-stream no longer leak uploads).
5. **Progress**: bytes count source bytes read, so the bar tracks the scanned totals regardless of compression ratio.

The Copy (F5) dialog has a **"Pack into archive…"** button that swaps it for this dialog over the same selection.

Hardlinks are not detectable through the VFS surface and are archived as independent file copies.

### Properties (Alt+Enter)

Modal dialog showing file metadata. Supports single files and multi-file selections. The Unix permission/ownership editor is read-only on VFS types that don't support metadata changes (S3, archives); VFSes with extended properties (S3) still get their own editable sheet groups (see below).

**Information displayed**:
- Name
- Size (human-readable + exact byte count)
- Size on disk (allocated bytes, `st_blocks`-based) — shown when the VFS reports it (local Unix files); diverges from Size for sparse files. Multi-select sums it the same way as Size.
- Type (file / directory / symlink)
- Symlink target path (if applicable)
- Owner (name and numeric ID when available, e.g., "root (0)")
- Group (name and numeric ID)
- Mode (Unix permissions)
- Modified, Accessed, Created timestamps (locale-formatted)
- Hard links, Inode, Device (single selection, Unix filesystems only — rows hidden when the VFS doesn't report them)

**Permission editor** (when VFS supports metadata changes):
- **Tri-state checkboxes**: 3×3 grid (Owner/Group/Other × Read/Write/Execute). For multi-file selections with mixed permissions, differing bits show as indeterminate. Click cycles: checked → unchecked → indeterminate (leave unchanged) → checked.
- Special bits row: Set UID, Set GID, Sticky (also tri-state).
- Octal notation display — shows "?" for indeterminate digit positions.
- Mask-based application: only explicitly set/cleared bits are modified; indeterminate bits are preserved per-file.

**Ownership editor**:
- Separate checkboxes to enable owner/group editing.
- Text input accepts numeric ID. Name resolution planned for future.

**Recursive** checkbox (for directories): Applies permissions and ownership changes to all contents.

**Extended properties (property sheets)**: VFSes that advertise `has_extended_properties` contribute extra editable groups below the generic metadata. The sheet is schema-driven — the backend describes fields (text, choice, key-value map, grant list) and one generic renderer edits them all; no per-VFS frontend code. Sheets load after the dialog opens (loading placeholder → filled in place), so Alt+Enter never stalls on network calls. Multi-select folds per-field: equal values show, differing ones show as mixed/indeterminate and are left untouched unless edited; grant lists fold whole (differing lists offer an explicit "replace on all"). Applying goes through the operations engine (progress, per-item retry/skip, cancel) as an `ApplyProperties` operation; the **Recursive** checkbox extends to sheet edits (per-prefix apply on S3, skipping synthetic directory entries).

Today only S3 implements a sheet:
- **S3 metadata** group: user metadata (`x-amz-meta-*`) as an editable key-value map (add/remove/edit keys), storage class, Content-Type, Cache-Control. Edits rewrite the object in place (CopyObject with metadata replacement) — untouched system headers and any non-default ACL are preserved across the rewrite; the dialog shows a hint that this can be slow for large objects. The rewrite fails on objects over 5 GiB (CopyObject cap — would need multipart copy) and on unrestored Glacier-class objects; both surface as per-item operation issues.
- **S3 access control** group: the grant list (grantee user ID / group URI / email × permission) and a write-only canned ACL selector (S3 reads back grants, not the canned value). Omitted gracefully when `s3:GetObjectAcl` is denied — the metadata group still loads.

**Directory Properties**: Available from the pane context menu (right-click empty space). Shows metadata for the current directory itself.

### Clipboard Operations

- **Copy Path** (Mod+C): Copies the paths of all selected files (or the focused file if none selected) to the system clipboard.
- **Paste** (Mod+V): Pastes file paths from the system clipboard into the current pane.

### Operation Progress and Issue Resolution

When a copy, move, rename, delete, or trash operation runs, it's tracked in the **Operations Panel**:

**Foreground modal** (default for new operations):
- Large overlay showing operation kind, description, progress bar, percentage, the current file being processed (relative path, not full destination path), live transfer speed, and estimated time remaining (ETA).
- **Cancel** button (rightmost): Stops the operation. Partially copied files are left as-is.
- **Background** button: Minimizes the operation to the compact panel, freeing the UI for other work.
- **Esc** maps to Cancel, **click-outside** maps to Background. This is a deliberate asymmetry from the rest of the app where Esc and click-out are symmetric: the progress modal isn't a form being canceled — it's destructive work in flight, and Esc is the panic-cancel reflex (an accidental click-out cancelling a long copy is annoying-but-redoable; an accidental Esc backgrounding a runaway delete is silent destruction).
- **Show Next Operation** (F10): cycles foreground through all ops by id — backgrounds the current and surfaces the next, wrapping. Useful when multiple ops are running simultaneously.

**Background panel** (compact list):
- Shows all backgrounded operations as a compact list.
- Each operation shows: kind, description, progress bar, percentage.
- Cancel and Dismiss buttons per operation.
- **Click a backgrounded operation** to foreground it again (re-opens the modal).

**Operation states**: Scanning → Running → Completed / Failed / Cancelled.
- By default, Completed and Cancelled operations are automatically removed from the panel. Set the `behavior.keep_finished_operations` preference to keep them visible until dismissed.
- Failed operations persist with an error message until dismissed.
- The Close button in the foreground modal is available for all finished states (completed, cancelled, failed).

**Issue resolution** (file conflicts):
When an operation encounters a conflict:
- The foreground modal shows the issue (e.g., "File 'readme.txt' already exists").
- Available actions depend on the issue type:

| Issue Type | Available Actions |
|-----------|------------------|
| File already exists | Skip, Overwrite |
| Permission denied | Skip, Retry |
| Other I/O error | Skip, Retry |

- **"Apply to all" checkbox**: When checked, the chosen action is automatically applied to all subsequent issues of the same type within the same operation. No further prompts for that issue type.

---

## 5. File Viewer (F3)

Opens in a separate window (1100×800 pixels). Automatically detects the file's MIME type and selects the appropriate viewing mode.

### Pre-warmed Windows

Viewer windows are **pre-warmed** for instant opening. A hidden window is created in the background with all web content and static UI already loaded. When F3 is pressed, the pre-warmed window receives the file path via the existing `UpdatePublisher` state mechanism, attaches its menu bar, and is made visible — avoiding the WebKit startup and JavaScript initialization latency. A replacement pre-warmed window is spawned immediately in the background. Falls back to direct window creation if no pre-warmed window is available.

Pre-warms are created only from the `init` command — i.e. after the main window's frontend has demonstrably booted — never during setup. Creating additional WebView2 windows while the main webview is still in its early composition phase intermittently wedged it into a permanently blank (white) window on Windows, at up to ~50% of Explorer launches on slow machines. Do not move prewarm creation back into the startup path.

### Mode Selection

The viewer has a **View** menu with radio buttons to manually switch between modes: Text, Hex, Image, Audio, Video, PDF. The initial mode is chosen automatically:

| Detected Type | Mode |
|---------------|------|
| `text/*`, `application/json`, `application/xml`, `application/javascript`, `application/typescript`, `application/x-sh`, `application/x-python`, `application/sql`, `application/x-yaml`, `application/toml`, `application/graphql`, `image/svg+xml`, anything ending in `+xml` or `+json` | Text |
| `image/*` | Image |
| `audio/*` | Audio |
| `video/*` | Video |
| `application/pdf` | PDF |
| Everything else | Hex |

### Mode Toggle

The status bar includes mode toggle buttons on the right side. The auto-detected mode and Hex are always available as quick-switch options. Pressing **F3** toggles between the auto-detected mode and Hex (e.g., auto=Image, current=Image → F3 → Hex; auto=Image, current=Hex → F3 → Image).

### Text Mode

- Line-numbered display with a non-selectable gutter. Gutter width adjusts to fit the number of digits in the total line count.
- **Chunked loading**: Loads files in 128 KB chunks on demand. Large files don't need to be fully loaded before viewing. LRU cache holds up to 32 chunks (4 MB); older chunks are evicted as new ones load.
- **UTF-8 aware**: Detects incomplete UTF-8 sequences at chunk boundaries and handles them gracefully.
- **Virtual scrolling**: Only renders visible lines plus 5-line overscan for smooth scrolling. Scroll scaling for files exceeding browser's max element height (16M px).
- **Incremental line index**: Line positions are built by scanning for `0x0A` in chunks as they load. The `+` after the line count in the status bar indicates more lines may exist in unscanned chunks.

**Selection**:
- **Mouse drag**: Click and drag to select character ranges. Selection is character-granular (uses `caretRangeFromPoint`).
- **Double-click**: Selects the word under the cursor.
- **Shift+Click**: Extend selection from anchor to clicked position.
- **Ctrl+A**: Select entire file.
- **Auto-scroll**: Dragging near top/bottom edges (20px margin) auto-scrolls.
- **Escape**: Clears the selection. If there is no selection, closes the viewer.

**Copy** (Ctrl+C): Copies selected text to clipboard via the Rust backend (`copy_viewer_range`). 10 MB copy size limit.

**Search** (Ctrl+F): Opens a search bar at the bottom of the viewer.
- **Literal text search** (default) or **regex** (toggle with `.*` button).
- **Enter**: Find next match from current position. Wraps around to start if at end of file.
- **Shift+Enter**: Find from start of file.
- Match is selected and scrolled into view (including horizontal scroll if needed).
- Status indicator shows "Not found", "Wrapped", or error messages.
- Search executes on the backend via `find_in_file` on the `Filesystem` trait — works for remote files too.

**Go to Line** (Ctrl+G): Opens a bar with a line number input (1-based). Enter to jump, Escape to cancel.

**Context menu** (right-click): Copy, Select All, Go to Line...

**Keyboard**:
| Key | Action |
|-----|--------|
| Arrow Up/Down | Scroll one line |
| Page Up/Down | Scroll one page |
| Home | Jump to start of file |
| End | Jump to end of file |
| Ctrl+A | Select all |
| Ctrl+C | Copy selection |
| Ctrl+F | Open search |
| Ctrl+G | Go to line |
| Escape | Clear selection / close search / close viewer |
| F3 | Toggle mode |

**Status bar**: `path/to/file.txt | Text | Line 42 / 1250+ | Sel: L10 C5–C20 (0x00A5–0x00B4, 15) | 125.4 KB`

The `+` after the line count indicates the file is still loading. Selection info shows line/column range with byte offsets and size.

### Hex Mode

- Classic hex dump layout: offset column (8 hex digits) | 16 hex bytes (grouped 8+8 with a gap) | ASCII representation.
- Non-printable bytes shown as `.` in the ASCII column. Printable range: 0x20–0x7E.
- **Virtual scrolling** with max scroll height clamping (prevents browser rendering issues with very tall elements).
- **On-demand chunk loading**: 128 KB chunks with LRU cache (32 chunks). Preloads chunks for the visible viewport and overscan area.
- **Mouse wheel**: Handles both pixel-mode and line-mode scroll deltas. Accumulates sub-row pixel deltas across events to snap to row boundaries.

**Selection**:
- **Byte-granular**: Click in the hex or ASCII column to select a byte. The clicked column becomes the "active" column (blue highlight); the other column shows the same selection in grey.
- **Mouse drag**: Drag across bytes to select a range.
- **Shift+Click**: Extend selection to the clicked byte.
- **Ctrl+A**: Select all bytes.
- **Auto-scroll**: Dragging near edges auto-scrolls.
- **Escape**: Clears selection.

**Copy** (Ctrl+C): Copies selection using the active column's format.

**Context menu** (right-click): Copy as Hex (space-separated uppercase, e.g., `4D 5A 90 00`), Copy as Text (UTF-8), Select All, Go to Offset...

**Search** (Ctrl+F): Same search bar as Text mode, plus a **Hex toggle** button.
- **Hex mode**: Input parsed as hex bytes (e.g., `4D 5A` or `4d5a`).
- **Literal text** and **regex** modes also available.
- Matches are selected as byte ranges in the hex view.

**Go to Offset** (Ctrl+G): Opens a bar with label "Go to offset (hex)". Input is parsed as hexadecimal (e.g., `1A0` jumps to byte 416).

**Keyboard**: Same as Text mode (Ctrl+A, Ctrl+C, Ctrl+F, Ctrl+G, arrows, Page Up/Down, Home, End, Escape, F3).

**Status bar**: `path/to/file.bin | Hex | Offset 00000A20 / 000FFFFF | Sel: 00000A20–00000A2F (16) | 1.0 MB`

### Image Mode

- Displays the image centered, initially fit to the window (aspect ratio preserved). Zoom percentages are HiDPI-aware: 100% means one image pixel per physical display pixel (`devicePixelRatio` is tracked live, so moving the window between monitors keeps the indicator and limits correct). Nearest-neighbor (`image-rendering: pixelated`) kicks in only above 100%; at or below, the image is smoothly resampled.
- **Toolbar**: zoom out / zoom-percent preset dropdown (Fit, 25–800%) / zoom in, Fit, 1:1, rotate CCW/CW, flip H/V (toggle state shown), background cycle, info panel toggle. Tooltips carry the keyboard shortcuts.
- **Zoom**: Mouse wheel (×1.11 per tick) and macOS trackpad pinch zoom centered on the cursor; `+`/`-` (×1.25 per press) zoom centered on the window. Min zoom = fit-to-window (or 100% if image is smaller). Max zoom = 50×. `0` resets to fit, `1` goes to actual size (100%), double-click toggles fit ↔ 100% around the clicked point.
- **Selection**: Plain left-drag draws a marching-ants selection rectangle, snapped to integer image pixels (in display orientation — rotation/flips are baked into what's selected). Mod+C copies the selection to the clipboard as PNG (no selection: copies the whole image, transforms applied); Mod+A selects all, Escape or a plain click clears (Escape only closes the viewer once there's no selection). Rotating or flipping clears the selection. Copying renders through an offscreen canvas (`crossOrigin=anonymous`; the file server serves `Access-Control-Allow-Origin: *`) and writes via `navigator.clipboard` — the `ClipboardItem` is constructed synchronously inside the gesture, as WebKit requires.
- **Pan**: Middle-click drag or Shift/Alt+left drag, or arrow keys. Pan is clamped to keep the image visible (no empty edges). Cursor is crosshair (selection is the default drag), grabbing while panning.
- **Rotate/flip** (view-only, never touches the file): `r` rotates 90° clockwise, `Shift+R` counter-clockwise, `h`/`v` flip horizontally/vertically. Rotating while fitted re-fits to the new orientation; the status bar shows the active rotation/flips.
- **Background**: `b` cycles the backdrop — transparency checkerboard (default, theme-aware via the `--color-checker-*` tokens), dark gray, light gray. Persisted as the `viewer.image_background` preference (app-wide, like all preferences).
- **Info panel**: `i` (or toolbar/context menu) toggles a right-side panel with dimensions, megapixels, MIME type, file size, and a curated EXIF summary (camera make/model merged, exposure-time · aperture · ISO on one row, focal length with 35mm equivalent, flash fired-state only, zero exposure bias omitted, GPS as decimal coordinates). EXIF is parsed backend-side (`kamadak-exif`) from a bounded 4 MiB prefix read over the VFS read path, so it works on S3/archives/remote without downloading the file; files with no (or late-offset) EXIF show "No EXIF metadata". Values (not labels) are selectable for copy-out; a Mod+C with an active text selection copies the text, not the image. On macOS, non-text/hex viewer modes carry the predefined Edit menu items so clipboard keys route into the webview at all (same trap as `main_window::menu`).
- **Context menu**: Copy Selection/Copy Image, Fit/Actual Size, rotate/flip, background radio submenu, info panel toggle — same Radix menu styling as the main window, with shortcut hints.
- **Live Text**: the `<img>` carries `pointer-events: none` so macOS WebKit's VisionKit overlay (Live Text OCR / "Look Up" data detectors) never activates; all interaction is handled by the container.
- **Escape**: Close viewer (when no selection is active).
- **Cached image handling**: Correctly detects already-cached images (`img.complete`) to avoid missed load events.

**Status bar**: `path/to/image.png | Image | 1920×1080 | 150% | 90° flip H | Sel: 640 × 480 | 2.4 MB`

**Error handling**: Shows "Unable to display image preview" if the image fails to load.

### Audio Mode

- Native HTML5 `<audio>` player with browser controls (play, pause, seek, volume).
- Centered in the window with a dark background. Max width: 500px, player width: 80% of container.
- **Escape**: Close viewer.

**Status bar**: `path/to/audio.mp3 | Audio | 5.2 MB`

**Error handling**: Shows "Unable to play audio preview" with error details. Logs network/ready state to console.

### Video Mode

- Native HTML5 `<video>` player with browser controls (play, pause, seek, volume, fullscreen).
- Scales to fit container (max 100% width and height).
- **Escape**: Close viewer.

**Status bar**: `path/to/video.mp4 | Video | 150.5 MB`

### PDF Mode

- Rendered via PDF.js with a custom toolbar (not a browser iframe).
- **Toolbar**: Previous/Next page buttons, page display ("1 / 42"), zoom in/out/fit buttons, zoom percentage.
- **Keyboard**: `=` zoom in, `-` zoom out, `0` reset to fit (the shared, rebindable `viewer_zoom_*` commands — Mod+= and friends stay webview zoom).
- **Escape**: Close viewer (via window-level menu accelerator).

**Status bar**: `path/to/document.pdf | PDF | 2.3 MB`

### Viewer Menu Bar

- **App menu** (macOS only): Quit Newt (Cmd+Q).
- **File**: Close (Escape)
- **Edit** (Text/Hex modes only): Copy, Select All, separator, Go to Line / Go to Offset
- **View**: Text / Hex / Image / Audio / Video / PDF (radio buttons — one always checked)

### File Serving

The viewer and editor access files through an internal HTTP server on localhost (random port, token-protected). This supports:
- Range requests (HTTP 206) for chunked loading.
- 1 MB streaming chunks to avoid buffering entire files.
- MIME type detection for proper content-type headers.

---

## 6. File Editor (F4)

Opens in a separate window (900×700 pixels) using Monaco Editor (the editor core from VS Code).

### Pre-warmed Windows

Like the viewer, editor windows are **pre-warmed** — a hidden window with Monaco Editor fully initialized runs in the background. When F4 is pressed, the file path is sent via state, the menu is attached, and the window is shown instantly. Monaco's heavy JavaScript initialization happens during the pre-warm phase, so the editor is ready to type immediately. A replacement is spawned after each use.

### Language Detection

**By file extension** (prioritized):

| Extensions | Language |
|-----------|----------|
| `.js`, `.mjs`, `.cjs`, `.jsx` | JavaScript |
| `.ts`, `.tsx` | TypeScript |
| `.py` | Python |
| `.rs` | Rust |
| `.go` | Go |
| `.java` | Java |
| `.kt`, `.kts` | Kotlin |
| `.c`, `.h` | C |
| `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx` | C++ |
| `.cs` | C# |
| `.rb` | Ruby |
| `.php` | PHP |
| `.swift` | Swift |
| `.lua` | Lua |
| `.pl`, `.pm` | Perl |
| `.r` | R |
| `.m` | Objective-C |
| `.sh`, `.bash`, `.zsh`, `.fish` | Shell |
| `.ps1` | PowerShell |
| `.bat`, `.cmd` | Batch |
| `.html`, `.htm` | HTML |
| `.css` | CSS |
| `.scss`, `.less` | SCSS/Less |
| `.json`, `.jsonc` | JSON |
| `.yaml`, `.yml` | YAML |
| `.toml`, `.ini` | INI/TOML |
| `.xml`, `.svg` | XML |
| `.md`, `.mdx` | Markdown |
| `.sql` | SQL |
| `.graphql`, `.gql` | GraphQL |
| `.dockerfile` | Dockerfile |
| `.tf` | HCL (Terraform) |
| `.diff`, `.patch` | Diff |

**By filename** (case-insensitive): `Dockerfile` → Dockerfile, `Makefile`/`GNUmakefile` → Makefile.

**By MIME type** (fallback): `application/json` → JSON, `application/xml` → XML, `text/x-python` → Python, etc.

**Fallback**: Plain text.

### Editor Features

- **Syntax highlighting** for all detected languages.
- **Word wrap**: Toggle via View menu. Setting persists in Rust state.
- **Line numbers**: Always visible.
- **Find and replace**: Ctrl+F (find), Ctrl+H (replace).
- **Undo/redo**: Ctrl+Z / Ctrl+Y.
- **Font**: 13px, 18px line height.
- **Minimap**: Disabled.
- **Render whitespace**: Only in selection.
- **Theme**: Follows system dark mode preference. Updates dynamically if the OS theme changes.
- **File size limit**: 5 MB (enforced on load). Larger files show an error and the window closes.

### Saving

- **Ctrl+S / Cmd+S**: Save the file.
- Encodes content to UTF-8 and writes atomically.
- Status bar briefly shows "Saving..." during the write.
- After save, the dirty indicator clears and the file size updates.

### Dirty State and Closing

- **Dirty indicator**: Window title shows `* filename - Editor` when unsaved.
- **Escape**: Closes the editor window (confirmation first if unsaved changes) — but only when Monaco doesn't consume it: dismissing intellisense/find/palette widgets or canceling a selection spends the Escape, and only an unclaimed press closes. Save dispatches through the rebindable `editor_save` command; the resolved key also rides Monaco's Save action (re-registered on rebind — `addAction` commands are editor-scoped, so global keybinding rules can't target them) so the key works natively in-editor and Monaco's palette shows the real shortcut.
- **Close with unsaved changes**: A warning confirmation dialog appears: "You have unsaved changes. Close without saving?" User must confirm to discard changes.
- **Close without unsaved changes**: Window closes immediately.

### Editor Menu Bar

- **App menu** (macOS only): Quit Newt (Cmd+Q).
- **File**: Save, Close (Ctrl+W). On macOS the Save accelerator mirrors the resolved `editor_save` binding and menus rebuild on preference changes (the menu bar claims the key ahead of the webview, so a stale accelerator would shadow a rebind); other platforms register no Save accelerator — the webview handles the key.
- **View**: Word Wrap (checkbox toggle). Each newly opened file starts from the `editor.word_wrap` preference (default off); toggling wrap on an open file overrides it for that file only.
- **Language**: Radio buttons for all supported languages (Plain Text, C, C++, C#, CSS, Dockerfile, Go, HTML, INI/TOML, Java, JavaScript, JSON, Kotlin, Lua, Markdown, Perl, PHP, Python, Ruby, Rust, SCSS, Shell, SQL, Swift, TypeScript, XML, YAML)

### Status Bar

`path/to/script.py [Modified] | python | Ln 25, Col 14 | 12.3 KB | Saving...`

- Path with `[Modified]` suffix if dirty.
- Language ID.
- Cursor position (line and column, 1-based).
- File size (human-readable).
- "Saving..." briefly during save.

---

## 7. Terminal Integration

### Terminal Panel

- Collapsible panel at the bottom of the main window. Its height is resizable by dragging the splitter and persists app-wide in `state.json` (`layout.terminal_height`), restored when the panel is next shown; the file-pane split above it always opens 50/50.
- **Tab bar**: Lists all terminal tabs ("Terminal 1", "Terminal 2", etc.). Active tab has a distinct style. Defunct terminals show "(exited)" suffix.
- **"+" button**: Creates a new terminal.
- **"×" button** on each tab: Closes that terminal.
- **Tab click**: Activates that terminal (switches visible terminal).

All terminals are always mounted in the DOM but only the active one is visible. This preserves terminal state (scrollback, running processes) when switching tabs.

### Terminal Emulation

- **xterm.js**: Full VT100/ANSI terminal emulation with 256-color and truecolor support.
- **Scrollback**: 1000 lines.
- **Font**: Menlo, Monaco, Courier New (fallback chain), 12px, 1.2 line height.
- **Cursor**: Blinking bar, 2px wide.
- **Working directory**: New terminals inherit the current directory of the active pane. On Windows the native path is de-verbatimised for the spawn (`\\?\C:\…` → `C:\…`) so cmd.exe actually cd's there; a genuine network location stays UNC (`\\server\share\…`) so the shell shows its own "UNC not supported" notice rather than us hiding it.
- **Shell**: Unix — system default shell (passwd database or `$SHELL`). Windows — `%COMSPEC%` (cmd.exe). On macOS the shell is spawned as a **login shell** (`-l`, for `bash`/`zsh`/`fish`/`sh`; shells with no login flag are left alone): launchd gives a GUI process a bare `PATH`, and everything `path_helper` contributes (`/etc/paths.d`, the cryptex dirs, `/Library/Apple/usr/bin`) only arrives via `/etc/profile`, which non-login shells never read. Terminal.app, iTerm2, Ghostty and VS Code all do the same on macOS — and, like VS Code, Newt deliberately does *not* pass `-l` elsewhere: a Linux desktop session gets its environment from PAM/systemd, and an agent gets one from its login-shell bootstrap.
- **Backend**: Unix uses a real PTY (`pty-process`). Windows uses a ConPTY (`CreatePseudoConsole`) driven directly via `windows-sys` — no third-party PTY wrapper. I/O is fully async over tokio overlapped named pipes (IOCP reactor, no dedicated reader threads); child exit is observed via an OS thread-pool wait. Because the ConPTY output pipe (owned by conhost, not the child) never EOFs on its own, end-of-stream is deterministic: on child exit the console is closed, which makes conhost flush its entire buffer and then break the pipe (no timers, no teardown latency).
- **Environment**: Unix sets `TERM=xterm-256color`, `COLORTERM=truecolor` (ConPTY emits its own VT, so these are not set on Windows). On macOS, `LANG` is exported process-wide at startup (`newt_common::locale::ensure_locale`) when the environment carries no locale at all, which is what launchd hands a GUI process — without it bash fails `setlocale` and prints a warning per category into every terminal. The value comes from `CFLocaleCopyCurrent`, probed against libc and falling back to `en_US.UTF-8`. Only ever `LANG`, never `LC_ALL`: ssh forwards `LC_*` by default and `LC_ALL` outranks everything on the far side, so exporting it would push our locale onto every remote and warn there whenever it isn't generated. Nothing to do on Linux (pam_env supplies `LANG`) or Windows.
- **Responsive**: Automatically resizes when the panel is resized (via ResizeObserver + FitAddon).

### Theming

Terminal colors follow the system/app theme:
- Separate light and dark color palettes (VSCode-inspired).
- Theme updates reactively when the OS color scheme changes.
- Checks `document.documentElement.dataset.theme` first (explicit app override), then falls back to `prefers-color-scheme` media query.

### Copy/Paste

- **Copy**: Ctrl+Shift+C (or Cmd+C on macOS) copies the terminal selection to the system clipboard.
- **Paste**: Ctrl+Shift+V on Linux/Windows, Cmd+V on macOS. The terminal reads the clipboard via `navigator.clipboard.readText()` and writes it into the PTY (an explicit handler is needed on macOS because Cmd+V is not delivered to the webview without an Edit menu — see below).
- **macOS Edit menu**: Main, viewer, and editor windows include a native Edit submenu with Undo / Redo / Cut / Copy / Paste / Select All entries. Without this menu, macOS silently swallows Cmd+V/C/X/A before they reach the webview, so this is required for clipboard shortcuts to work in any text input.
- **Input assist disabled globally**: `index.html` sets `autocorrect="off" autocapitalize="off" spellcheck="false"` on `<html>`, which inherits to every `<input>` and `<textarea>`. macOS WebKit otherwise applies these by default and silently mangles typed paths, regex patterns, and shell commands; Linux WebKit doesn't, so this normalises behaviour across platforms.
- **Selection**: Highlight text with the mouse to select. Text is selectable by default.

### Terminal Lifecycle

- **Running**: Full interactivity. Input goes to the shell, output is displayed.
- **Defunct/Exited**: When the shell process exits:
  - If `behavior.keep_terminal_open` is **true** (default): The tab stays open, showing a dimmed message: `[Process exited with code X. Press Enter to close.]` (or signal name if killed). User presses Enter to close the tab.
  - If `behavior.keep_terminal_open` is **false**: The tab is automatically removed. If it was the active terminal, the next terminal becomes active. If no terminals remain, the terminal panel hides.

### Terminal in Remote/Elevated Mode

Terminals in remote sessions run on the remote host. The PTY is allocated by the agent process. Terminal I/O is forwarded over the RPC protocol. From the user's perspective, the terminal behaves identically to local mode.

### Working Directory Resolution

When a new terminal is created (Mod+Enter, Ctrl+Shift+~, panel toggle, focus terminal, send-to-terminal), its initial cwd is resolved from the active pane's path:

- **Path on the terminal's filesystem** (the local FS in local mode, the agent's FS in remote/elevated mode): used as cwd directly.
- **Archive VFS**: walks to the enclosing directory of the archive's origin file (e.g., browsing `/home/user/foo.tar.gz/inner` opens the terminal in `/home/user`). Nested archives walk the chain back to a host path.
- **VFSes with no origin** (S3, SFTP, Remote): the spawning process's inherited cwd is used (no `working_dir` is set), since there is no host path that meaningfully corresponds to the pane location.

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| Ctrl+\` | Toggle terminal panel visibility |
| Ctrl+Shift+~ | Create new terminal |
| Ctrl+PageDown | Next terminal tab |
| Ctrl+PageUp | Previous terminal tab |
| Alt+Up | Switch focus from terminal to file panes |
| Alt+Down | Switch focus from file panes to terminal |
| Mod+Enter | Open focused file/directory in a new terminal (sets working directory) |
| Enter (in defunct terminal) | Close the terminal tab |

These shortcuts are handled by the terminal's custom key event handler and bubble through to the main window handler where appropriate.

### Shell Integration (`newt` CLI)

Every spawned terminal (and every user command, both terminal- and operation-mode) gets a `newt` CLI that remote-controls the owning session (design: `design_docs/DESIGN_SHELL_INTEGRATION.md`). Gated by `behavior.shell_integration` (default on; currently gates local sessions — remote agents always provide it).

**Plumbing**: the PTY-owning side (host in local sessions, agent in remote/elevated) creates one fresh 0700 temp dir per session holding the control endpoint and the shim, and injects `NEWT_SHELL_SOCK` (socket path / pipe name), `NEWT_TERMINAL` (handle), and a PATH prepend of that dir into every spawned child. Unix: a Unix-domain socket plus a `newt` symlink to the agent binary; Windows: a named pipe (`\\.\pipe\newt-shell-…`) and a generated `newt.cmd` (which sets `NEWT_CLI=1`, since a .cmd shim can't control argv[0]). The agent path written into that shim has its `\\?\` verbatim prefix undone, because cmd.exe's parser rejects verbatim paths: Tauri canonicalises its own exe to resolve symlinks and `fs::canonicalize` on Windows always returns the `\\?\` form, so the agent located relative to the resource dir inherits it. Verbatim UNC is unwrapped to a real share (`\\?\UNC\server\share` → `\\server\share`) rather than trimmed, which would leave a relative path. Every other consumer of that path is a Win32 API and takes the verbatim form happily — only the shell needs this, as `wsl_launch::to_wsl_path` already found. The agent binary enters CLI mode only when invoked *as* `newt` (argv[0] / `NEWT_CLI`) with the env var set — `newt-agent` invoked by its own name always behaves as the agent. The main `newt` executable answers the same modality when `NEWT_SHELL_SOCK` is set and argv[1] is a known verb, so an already-on-PATH `newt` works on Linux without the shim.

**Protocol**: HTTP/1.1 over the socket/pipe (hyper connection-level; no axum in the shared code), deliberately version-tolerant (unknown route → 404, never a panic) because shells outlive app upgrades. On Unix, `curl --unix-socket "$NEWT_SHELL_SOCK" http://newt/v1/panes/active/cwd` works. In remote sessions the agent forwards control-plane verbs to the host over `API_HOST_SHELL_CONTROL`; `cat` bytes stream from the agent-side VfsRegistry without a host round-trip.

**Verbs** (`--pane active|other|left|right`, default active; exit codes 0 ok / 1 error / 2 no session):

| Verb | Behavior |
|------|----------|
| `newt pwd` | Print the pane's directory (display path — native on the root VFS, `s3://…` etc. otherwise). |
| `newt cd [path]` | Navigate the pane (non-strict: a leaf path lands on the parent with the entry focused). Bare `newt cd` syncs the pane to the shell's cwd. Relative paths resolve against the shell's cwd. |
| `newt focus <path>` | Alias for the leaf-focus form of `cd`. |
| `newt cat <path>` | Stream a file to stdout through the session VFS. Relative paths resolve against the *pane* — works inside archives/S3 mounts. |
| `newt open <path>` / `newt edit <path>` | Open the built-in viewer / editor (pane-relative resolution like `cat`). |
| `newt cp <src>… <dest>` / `newt mv` | Enqueue a copy/move through the operations framework (fire-and-forget; prints the operation id). Multiple sources need an existing dir; single source to a non-existent leaf copies/moves under the new name (a same-directory `mv` is a plain rename). Trailing slash asserts directory-ness; existence checks go through the session VFS. |
| `newt cmd [id]` | Tier-1 mechanical dispatch of any command-registry id (same ids as `[[bind]]`/palette): closes any open modal first, exactly like a keybinding. Bare `newt cmd` lists ids + names (including user commands). Excluded: `new_window`, `quit`, `open_elevated`, `connect_wsl` (non-uniform signatures). |

Path arguments resolve exactly like the Go To dialog (`resolve_display_path`: mounted-VFS URLs, native absolutes, `~` expansion on the session side), so `newt cd "$(newt pwd)"` round-trips on any pane. A URL matching no mounted VFS is an error (no auto-mounting).

---

## 8. VFS (Virtual Filesystem) Support

All filesystem access goes through trait abstractions. Multiple VFS types can be mounted simultaneously and accessed independently from either pane.

**File identity** (`Vfs::same_file`): whether two paths denote the same file is a question only the filesystem can answer — case folding, Unicode normalization, Windows short names and trailing-dot stripping, hardlinks and bind mounts all let distinct strings name one file, and the folding rules can't be reproduced on our side (NTFS's uppercase table is frozen per volume, APFS has its own, ext4 casefold is per *directory*). So there is no `is_case_sensitive` flag anywhere; the verb asks instead. Local compares `(st_dev, st_ino)` on Unix and volume serial + 128-bit `FILE_ID_INFO` on Windows (falling back to the legacy file index on network redirectors); SFTP compares the server's own `realpath` of each side, which also folds the way *that* server's filesystem does. The default is exact path equality, which is correct rather than merely conservative for byte-keyed namespaces (S3, archives, search results). Consumed by the copy/move self-destination check and the rename re-spelling exemption — see File Operations.

**Read primitives**: three shapes on the `Vfs` trait — sequential streams (`open_read_async`), stateless one-shot ranges (`read_range`), and positioned-read handles (`open_read_at` → `VfsRandomReader::read_at`), held open for a whole read session instead of paying an open per chunk. The split follows chunk count: a caller that will issue a single chunk uses `read_range` (one round trip, nothing to reap); a multi-chunk loop opens a handle. Handle consumers: the archive/disc engine loops, `find_in_file`, `newt cat` streaming, media streaming through the file server (ranges longer than one chunk), and viewer range-copy for selections past one chunk. The F3 viewer's own text/hex chunk reads deliberately stay on `read_range` — the viewer should observe live changes to the file and must not hold the file open (locking it on Windows) just because a viewer window is up. The handle pins the file's identity where the backend can, so a file replaced mid-session fails or keeps serving the original rather than mixing bytes across generations: an open fd on local (pread), an SFTP file handle (protocol-native offset reads, size stat'd once at open), a pinned ETag on S3 (reads after the first carry `If-Match`; a concurrent overwrite fails the read with "object changed while reading"). The same surface is exposed on the shell-facing `Filesystem` trait (`VfsPath`-addressed, hairpin-diverted like `read_range`), which also carries the file-content verbs (`read_range`/`read_file`/`write_file`/`find_in_file`/details/property sheet) — there is no separate reader trait. Over RPC a handle is a server-held session (`OPEN_READ_AT` mints a `StreamId`, `READ_AT` is request/response per chunk, `READ_AT_CLOSE` on proxy drop reaps it) — the same open/read/close triple exists at both the VFS verb level and the Filesystem verb level.

**Auto-refresh**: Panes auto-refresh on window focus for local and remote VFS types. Auto-refresh is disabled for S3, SFTP, and archive VFS types (where listing is expensive). Manual refresh is always available via Mod+R.

### Local Filesystem (always mounted, VFS ID 0)

- Full read/write support.
- File watching: Panes automatically refresh when the underlying directory changes on disk.
- All operations supported: rename, hard link, symlink creation, metadata (permissions, timestamps, owner, group), filesystem stats.

### S3 (Amazon S3 / S3-Compatible)

**Mounting**: Via command palette ("Mount S3"), VFS selector dropdown, or Quick Connect. Opens a dialog with:

- **Region** (optional): AWS region (e.g., `us-east-1`). When given, it pins every bucket — no `GetBucketLocation` discovery calls (IAM policies frequently don't grant it). When empty, per-bucket regions are discovered and cached via `GetBucketLocation`.
- **Bucket** (optional): Scope the mount to a specific bucket instead of listing all buckets.
- **Endpoint URL** (optional): Custom S3-compatible endpoint (Minio, Ceph, etc.).
- **Credentials** dropdown with four modes:
  - **Default** (environment / instance metadata): Uses the AWS default credential chain.
  - **AWS Profile**: Specify a named AWS profile.
  - **IAM User (access key)**: Enter Access Key ID and Secret Access Key (masked). Secrets stored in system keychain.
  - **Assume Role**: Enter Role ARN and optional External ID for cross-account access.
- **Profile name** row: revealed on save-intent (Save… / Mod+S), auto-generated from bucket/endpoint/region until manually edited. Used by the **Save** action (see Connection Profiles — Save and Connect are independent footer actions).

While the mount is in flight the dialog streams the mount log (VFS progress stages), and a failed mount leaves the dialog open with everything editable for a retry. The SDK client is lazy, so the mount performs one probe request mirroring the first navigation (`ListObjectsV2` with `max_keys=1` on a scoped bucket — including region discovery — else `ListBuckets` with `max_buckets=1`): bad credentials/endpoint/bucket fail the mount in the dialog instead of surfacing later at listing time. Errors from a dialog-submitted action that settle only after the modal has closed (e.g. mount succeeded but the follow-up navigation failed) fall back to a popup instead of being swallowed with the dialog.

**Browsing**:
- Root (`/`) lists all buckets.
- Bucket contents are listed using `ListObjectsV2` with delimiter, simulating a directory structure via common prefixes.
- "Directories" in S3 are virtual (based on `/` separators in object keys). Created directories are 0-byte objects with trailing `/`.

**Operations supported**: Read, write (multipart upload with 10 MB chunks), create directory, delete, copy within the same S3 bucket, touch, rename (via the operation's copy+delete fallback — server-side CopyObject per object, works on prefixes too), extended properties (user metadata, storage class, Content-Type/Cache-Control, ACL grants + canned ACL — see Properties dialog). Server-side copies (copy/move/rename within S3) carry over user metadata and system headers (CopyObject default), and explicitly re-apply the source's storage class and any non-default ACL — a failed ACL restore is logged and the copy still succeeds, since the streaming fallback couldn't restore it either.

**Operations NOT supported**: Hard link, symlink, Unix permissions, filesystem stats, trash (plain Delete prompts for permanent deletion).

**Display path**: `s3://bucket/prefix/key`

**Breadcrumbs**: `s3:// → bucket → prefix → key`

**In remote sessions**: S3 connections originate from the remote host, using the remote host's AWS credentials and network.

### SFTP

**Mounting**: Via dialog (Mod+Shift+L → SFTP, or "Mount SFTP" in command palette) with `user@hostname` input plus a profile-name row revealed on save-intent (auto-named from the host; used by the **Save** footer action — see Connection Profiles). While the mount is in flight the dialog streams the mount log; a failed mount leaves it open for a retry.

**Connection**: Spawns an SSH process (`ssh <host> -s sftp`) with stdin/stdout piped. SFTP handshake happens over the SSH connection. 30-second timeout on connection. In remote sessions the `ssh` is spawned by the agent on the remote host, so the SFTP connection originates from there.

**Authentication**: Relies on the SSH client's configuration:
- Public key (SSH agent, key files).
- Password (via askpass dialog — see Connection Management). Prompts originating from agent-side `ssh` (i.e. when SFTP is mounted inside a remote session) round-trip back to the host UI via reverse RPC, so the dialog appears in the host window regardless of where the `ssh` process actually runs.
- Keyboard-interactive.
- SSH config file (`~/.ssh/config`) is respected.
- Host key verification prompts appear as in-app dialogs.

**Operations supported**: Read, write, rename, create directory, delete, symlink creation, hard link, metadata (permissions, timestamps, owner, group), file watching.

**Operations NOT supported**: Copy within SFTP (cross-file copy goes through the host), filesystem stats, trash (plain Delete prompts for permanent deletion).

**Symlink handling**: Reads symlink targets for display, stats targets to determine if they're directories.

**Display path**: `sftp://hostname/path/to/file`

**MIME detection**: Reads the first 8 KB of a file via `read_range()` and uses MIME type detection.

**Lifecycle**: SSH process is killed when the SFTP VFS is unmounted.

### Archives (Read-Only)

Mount and browse archive files as virtual read-only filesystems.

**Supported formats**:

| Format | Extensions |
|--------|-----------|
| TAR (uncompressed) | `.tar` |
| TAR + gzip | `.tar.gz`, `.tgz` |
| TAR + bzip2 | `.tar.bz2`, `.tbz2`, `.tbz` |
| TAR + xz | `.tar.xz`, `.txz` |
| TAR + zstd | `.tar.zst`, `.tzst`, `.tar.zstd` |
| CPIO | `.cpio` |
| CPIO + compression | `.cpio.gz`, `.cpio.bz2`, `.cpio.xz`, `.cpio.zst` |
| ZIP | `.zip`, `.jar`, `.war`, `.ear`, `.apk`, `.ipa` |

**Auto-detection**: Pressing Enter on a file with a recognized archive extension mounts it automatically and navigates into the archive root (instead of opening the file).

**TAR indexing** (streaming/incremental):
- Index is built by scanning the archive stream. Files appear incrementally in the UI as indexing progresses — you can browse partial results while the rest of the archive is still being indexed.
- Periodic snapshots every 200ms update the file list.
- If you navigate away before indexing completes, the indexing is cancelled.

**ZIP indexing** (one-shot, sans-IO): the in-tree `newt_archive::zip` reader (disc-image architecture, not the tar one — no external zip crate, no `spawn_blocking`/`block_on` anywhere) fetches the central directory at EOF in bounded 1 MiB slices with determinate progress, yielding the complete entry table; listings and `file_details` afterwards cost zero upstream reads. Reads are random-access: stored entries map 1:1 onto upstream range reads (ISO-extent style), compressed entries stream through a resumable decrypt→decompress cursor that gets parked per archive and resumed by the next sequential range read — the viewer's chunk fan-out costs one decompression pass total, not one per chunk. Dropping a read future drops the in-flight upstream read (async-native cancellation).

- **Methods**: stored, deflate, deflate64, bzip2, lzma, zstd, xz. PPMd and pre-1993 methods (shrink/implode/reduce) list fine and report a clean unsupported error on read.
- **Container gamut**: zip64 (large files, many entries), self-extracting stubs and prepended data (base-offset skew derived from the EOCD position), data-descriptor entries, implicit directories, duplicate entries (last wins), archive comments, split archives detected and refused cleanly.
- **Names**: UTF-8 flag honored, unicode-path extra field (0x7075) with CRC staleness guard, CP437 fallback for legacy names, `\`-separated paths normalized.
- **Metadata**: unix mode/symlinks from external attributes, uid/gid from the Info-ZIP extra (0x7875), timestamps from the extended-timestamp (0x5455) and NTFS (0x000a) extras at full precision with DOS-time fallback, DOS hidden attribute. Feeds copy-out metadata preservation via the standard `get_metadata` listing derivation.
- **Integrity**: full sequential reads verify CRC32 (and the WinZip AES HMAC) at end of stream; range reads can't see the whole stream and skip verification by construction.

**Encrypted ZIP archives**: The ZIP central directory is always cleartext, so mount and listing always succeed even for encrypted archives — you can browse the entry tree without unlocking anything. The password prompt happens lazily, the first time an encrypted entry is read: the standard askpass UI (same dialog used for SSH/SFTP) is shown, and a working password is cached for subsequent reads. ZipCrypto and WinZip AES (AE-1 and AE-2, all three strengths: AES-128/192/256) are supported; PKWARE SES "strong encryption" is refused cleanly. Passwords are validated against the entry's cheap verifier (AES PBKDF2 verifier bytes / ZipCrypto check byte) — no trial decompression. Wrong passwords re-prompt with an "Incorrect password" hint; dismissing the prompt returns a Cancelled error for that read but does not lock you out — the next read re-prompts. Cleartext entries are always readable without prompting, even in mixed-encryption archives. If individual entries use different keys, the cached password is replaced when a later entry needs a new one. AES-CTR is seekable, so range reads inside encrypted entries stay random-access.

**Navigation out of archives**:
- Pressing `..` at the archive root exits the archive and returns to the parent directory containing the archive file.
- The archive filename itself is focused after exiting.
- Breadcrumbs show the full path including the origin: clicking archive-level breadcrumbs exits back to the origin filesystem.

**Nested archives**: Archives can contain other archives. Opening an inner archive creates a new VFS mount with the outer archive as its origin. The cleanup system prevents unmounting a parent archive while a child archive is still open.

**Stale mount cleanup**: Archive mounts are *ephemeral* — automatically removed when no pane's current path or back/forward history references them (or any origin-derived children — nested archives, searches over them — transitively). The same cleanup machinery handles other ephemeral VFS types (currently SearchVfs) via a shared `is_ephemeral()` descriptor flag.

**Symlink and hard link resolution**: Symlinks (TAR/CPIO/ZIP) and hard links (TAR/CPIO) inside the archive are resolved internally. Directory listings show the *target's* size and `is_dir` for symlinks, and reading or viewing a file through a symlink or hard link transparently fetches the target's contents. ZIP symlink targets are entry content, so they are read eagerly (concurrently, tiny) at index time.

**Limitations**: Read-only. No create, modify, delete, rename, or metadata changes inside archives. (Creating new archives is a separate operation — see Pack to Archive under File Operations.)

### Disc Images (ISO 9660 / UDF, Read-Only)

Mount and browse optical disc images (`.iso`, `.udf`) as virtual read-only filesystems, exactly like archives: Enter on a disc image mounts it and navigates into its root; `..` at the root exits back to the image file. Same ephemeral-mount lifecycle, origin-styled breadcrumbs, and read-only capability surface as archive mounts.

**Formats**: ISO 9660 with Joliet and Rock Ridge extensions (Rock Ridge preferred over Joliet when both exist — real POSIX names, permissions, uid/gid, timestamps, symlinks), and UDF 1.02–2.60 including the Metadata Partition used by Blu-ray-era images (Type-1 and metadata partition maps; logical sector sizes 512–4096, so both optical `.iso` dumps and hard-disk-profile UDF images work). Hybrid/bridge images carrying both filesystems use the UDF view (the authoritative one, and the only correct one for >4 GB files). Multi-extent ISO files (the >4 GB mechanism), inline UDF data, sparse UDF extents, and UDF/Rock Ridge symlinks (resolved internally like archive symlinks) are all supported.

**Fully range-read native — no downloading, no indexing pass**: unlike archives, disc-image file data is stored as raw contiguous extents, so every read inside the image translates directly into a range read on the image file. Combined with S3's HTTP-Range reads, a Blu-ray-sized ISO on S3 can be browsed and its files viewed/copied out without the image ever being downloaded. Directory metadata is parsed *lazily per-directory* (no upfront tree walk; entering a 100 GB image costs a handful of small reads) through a 16 MiB block cache that coalesces the structure walk into few upstream GETs; listings are cached permanently since the image is immutable. The in-tree sans-IO `newt-disc` parser crate hands the VFS layer byte ranges to fetch, which are issued concurrently per round — high-latency backends pay round-trips, not per-structure reads.

**Limitations**: Read-only. VAT/virtual and sparable partition maps (packet-written CD-RW/DVD±RW media), El Torito boot image exposure, zisofs compression, interleaved ISO files, and multi-session images are not supported (clean errors where detectable). Raw-sector formats (`.bin`/`.cue`, `.nrg`) are out of scope.

### Remote VFS (client-local filesystem in SSH sessions)

In remote (SSH) sessions, the client-local filesystem can be mounted as a VFS on the remote agent, allowing the user to browse local files alongside remote ones. The root VFS label shows "Remote" in SSH sessions to distinguish it from the client-local VFS which shows "Local".

**Gated by preference**: `behavior.expose_local_fs` (default: false). When disabled, the remote VFS is not available and no local filesystem access is exposed to the remote host.

**Architecture**: The agent mounts a `RemoteVfs` that proxies all Vfs trait calls back to the Tauri host over bidirectional RPC. The host runs a `VfsDispatcher` that dispatches these calls to a real `LocalVfs`. Streaming reads run inside the invoke handler, pumping the VFS's async reader into chunk notifications: dropping the consumer aborts the invoke, which drops the reader mid-chunk. Mid-stream read errors are encoded into the invoke response and surfaced by the consumer's next read — a failed stream errors out instead of waiting for chunks that will never arrive. Streaming writes have a distinct high-priority abort signal: once the ordered RPC receiver processes it, dropping a writer aborts the host-side writer task without calling `finish` and removes both session and task-handle state. A receiver already backpressured on an earlier write chunk processes the abort after that handler resumes.

**Hairpin diversion**: For performance, the most latency-sensitive methods (`list_files`, `poll_changes`, `read_range`, `read_file`, `write_file`) are diverted at the Tauri backend — they execute against the local filesystem directly without round-tripping through the remote agent. This is transparent: the wrapper rewrites VFS IDs so callers see consistent paths.

**Operations supported**: Full read/write, browsing, file watching — same as local filesystem.

**VFS ID rewriting**: Batch streaming results from `list_files` have their VFS IDs rewritten from the local root to the remote VFS ID before being forwarded to the UI.

### Agent Mounts (remote connection in a pane)

Any spawn-style connection (SSH, Docker, Podman, Kubernetes-exec, Custom) can be mounted as a VFS in a pane instead of remoting a whole session — pick **Open in: Active pane** in the Connect dialog, or save a profile with `open_in = "pane"`.

**Architecture**: The spawner launches the agent with `--serve-vfs`: an FS-only mode that serves just the VFS API over the target's local filesystem (plus askpass forwarding) — no terminals, operations, or nested mounts exist on that connection, structurally. The proxy side is the same `Vfs`-over-RPC implementation as the client-local Remote VFS, under a distinct `agent` descriptor. The VFS selector shows the transport kind plus target — e.g. "Docker (web-1)", "SSH (user@host)" — not a bare "Remote".

**Where the connection originates follows the session**, like every mount: in a local session the host spawns the sub-agent; in a remote session the *agent* does — so a Docker profile mounted from an SSH session execs `docker` on the SSH host, against its daemon and network.

**Agent binary provisioning**: The spawner uploads its own executable when the target's triple matches (the common case). Foreign triples are streamed on demand from the host's bundled agents over RPC (pre-gzipped, spliced straight into the bootstrap upload), or downloaded into `~/.cache/newt/` for `docker cp`-style transports. Cache keys use the host's agent hash throughout.

**Lifecycle**: The sub-agent process is owned by the mount — unmounting (× in the VFS selector) or closing the last referencing history entry kills it; a sub-agent that dies on its own is reaped immediately and subsequent operations surface connection errors. Not ephemeral: agent mounts appear in the VFS selector like S3/SFTP.

**Startup probe**: after connecting, the mount verifies the agent actually responds (raced against connection close) before registering the VFS — an agent that dies on exec (wrong arch, missing binary) fails the mount with a diagnostic instead of producing a VFS that fails every operation.

**Connection log**: the spawn/bootstrap transcript (including the sub-agent process's stderr, where failures like "exec format error" surface) streams live into the Connect dialog while the mount is in flight, and a failed mount attaches the full transcript to its error message.

**Askpass**: SSH password / host-key prompts during the spawn ride the standard askpass channel — in a remote session they hop agent → host and land in the same UI dialog.

### Recursive Search (Find in Folder, Mod+F)

A search becomes a mounted VFS — results show up as a flat directory the user can browse, select, open, copy, delete using every existing pane affordance.

**Opening**: From any pane, press Mod+F (or run "Find in Folder…" from the command palette). The dialog is rooted at the current pane's directory and offers:

- **Name**: substring by default — typing `Cargo` matches anything containing `Cargo`. Switches to glob semantics (must match the whole basename) as soon as the pattern contains any of `*`, `?`, `[` — so `*.rs`, `Cargo.*`, etc. behave as expected. Empty = match every entry. Matches both files *and* directories.
- **Content**: optional substring (or regex, when the checkbox is set). Files larger than 10 MiB are skipped from content matching but still surface on name match. Directories are skipped when a content filter is set (they have no bytes to scan).
- **Case-sensitive**: applies to both name glob and content search.
- **Follow symlinks**: off by default (avoids loops and double-counting).

The content-is-regex / case-sensitive / follow-symlinks toggles are **sticky** — the last-used values are remembered in `state.json` (`search.*`) and seed the next fresh search. Refining an existing search (below) instead restores that search's own params, which takes precedence.

Submitting mounts a `SearchVfs` and navigates the active pane to its root. The walker runs in the background; matches stream into the pane as they're found, with the secondary "where from" hint inline next to each filename (formatted through the source VFS's descriptor — so an archive entry shows `/path/to/foo.zip/inner/dir`, not a raw inner-archive path).

**Liveness while nothing is found.** A search over a large tree can run for a long time before its first hit, so the walker reports what it is doing through the VFS progress channel — at most 5×/sec, independent of hits — and the pane status bar renders it as `Searching · 48,120 items · src/main_window`. The count is entries *scanned*, not matched (matches are already the pane's own item count), and the directory is shown relative to the search root, which the pane header already names. The pane lands on the (empty) search VFS immediately rather than waiting for the first match, so the line appears at once and the old directory's listing is not left on screen.

**Display & navigation**:
- The pane's path label and breadcrumb show `<root> [<params summary>]`, e.g. `/home/foo/projects [*.rs · "TODO"]`. No `Search:` prefix — the VFS selector already conveys that.
- `try_parse_display_path` returns nothing for SearchVfs paths, so the Navigate dialog will never accidentally drop the user back into a search.
- **Reveal source**: Shift+Enter on a result navigates the pane out of the search to the result's real parent directory in the source VFS, with the file focused. (Same key as Follow Symlink; the alias takes priority when the entry has one.)

**Behavior**:
- **Flat list, with paths shown.** Identically-named matches sort/select independently — entries are keyed by their relative path under the search root, not basename.
- **Rooted at the searched folder.** The SearchVfs's origin is the search root, so `..`/Backspace (and the synthetic `..` row) back out of the results into the directory the search ran over — like exiting an archive, except the origin is a directory so the escape lands *in* it rather than beside it (`OriginKind::Directory` vs. the archives' `Entry`, per `VfsDescriptor::origin_kind`). History (Alt+Left) and Shift+Enter on a hit still work as exits too. A terminal opened on a search pane gets the search root as its cwd.
- **Mod+F inside a search refines it.** The dialog reopens pre-filled with the current search's params (name field focused and selected), rooted at the original root; submitting mounts a fresh search that *replaces* the pane's current history entry, so refinements don't pile up in Alt+Left history and the superseded mount is auto-unmounted. Nested search stays impossible — it would produce duplicate-keyed aliases and break operation routing, so SearchVfs opts out via `VfsDescriptor::can_search` and exposes its params via `VfsDescriptor::search_params` instead. On any other VFS that opts out of search without offering params, Mod+F transparently falls back to the in-pane quick filter (`/`).
- **Invalid patterns fail the mount.** A malformed glob or regex errors in the search dialog on submit (validated with the same engines the walker uses) instead of mounting a search that silently finds nothing.
- **Operations are transparent.** Open, view, edit, rename, delete, copy/move, drag-out — every action targets the underlying real file. The display still shows the basename + source-path hint, but the bytes the operation touches are the source file's bytes.
- **Walker boundaries.** Walks within a single VFS; mounted child VFSes (archives, etc.) are *not* descended into. OS-level mounts (bind mounts, autofs, network shares) look like ordinary directories and are traversed.
- **Lifecycle.** SearchVfs is *ephemeral* (see below) — it auto-unmounts as soon as it's no longer reachable from any pane's current path or back/forward history, and it does not show up in the VFS selector dropdown. Its origin link also keeps the source mount (e.g. an archive the search ran over) alive while the search is reachable.
- **Deferred for v1**: tree-view toggle, native search inside archives / S3 / SFTP, `.gitignore` honoring.

### VFS Selector Dialog (Mod+Shift+L)

- Lists all currently **mounted** VFS instances (with VFS ID, type, and mount label).
- Lists **available** VFS types to mount, as a trailing "connect" section (separator + ellipsis-suffixed entries):
  - S3: Mounts immediately on selection (uses ambient credentials).
  - SFTP: Opens hostname input dialog.
  - Remote: Opens the Connect dialog (`DialogKind::MountRemote`) with "Open as a new session" defaulted off, i.e. pre-scoped to a pane mount whatever the session mode. Always offered, even inside a remote session.
- **Unmount button** (×) on mounted VFSes (except Local).
- Mount labels: S3 shows nothing extra, SFTP shows hostname, Archives show the source file path.
- **Drive classification** (split-root FSes, i.e. Windows drives): each per-drive entry carries a `VolumeInfo` probed at mount time on the FS-owning side (so a remote Windows host's drives classify too) — a kind-specific icon (fixed / removable / optical / network / RAM disk / subst), the volume label after the drive letter, and a dimmed target for drives that point elsewhere (`\\server\share` for mapped network drives, the aliased directory for subst). Recorded in `mount_meta` alongside the roots.
- **Live drive refresh** (Windows hosts): drive-letter changes (USB plug/eject, `net use`, `subst`) refresh the roots + classification of every mount describing this machine's drives via a logical remount (`VfsManager::remount` — revalidate + fresh `mount_meta` for ROOT in local/elevated sessions; owner-supplied meta pushed into the agent-side `RemoteVfs` for the client-local mount in remote/WSL sessions, which also carries the meta in the initial `MountRequest::Remote`). Triggers: a hidden `WM_DEVICECHANGE` listener window (volume arrival/removal broadcasts, burst-coalesced across all session windows) plus a window-focus-regain sweep as the catch-all for changes that don't broadcast (`subst`). No-op signals are free (old/new meta byte-compare); an open VFS selector is rebuilt in place. A pane sitting on a removed drive degrades through the normal NotFound handling on its next listing.
- **Map / Unmap network drive** (Windows only, `@cfg(windows)` — absent from the palette elsewhere): `map_network_drive` (F11) opens the system Map Network Drive wizard (`WNetConnectionDialog1W`, owned by the session window, run on the main thread) and, on success, navigates the invoking pane to the freshly mapped drive (`dwDevNum` → drive letter; deviceless connections stay put). `unmap_network_drive` (Alt+F11, pane-scoped) checks that the pane's current drive is a mapped network drive per the mount's recorded `VolumeInfo` (errors otherwise), confirms via a `ConfirmUnmapDrive` modal showing the drive and its `\\server\share` target, then disconnects with `WNetCancelConnection2W` *without* force (open handles surface as an error rather than yanking the drive). After either operation the drive roots refresh immediately (on top of the WM_DEVICECHANGE watcher), and panes parked on a disconnected drive are relocated to the mount's initial path.
- **Free space per target**: the dropdown opens instantly with free bytes blank, then a background fetch fills in "N GB free" (right-aligned, muted) per mounted target as results land — one entry per drive on split-root FSes, the single root on unified-root mounts whose VFS supports `fs_stats` (via the session `Filesystem::fs_stats` / `API_FS_STATS`, so it works identically in remote sessions). A dead network drive delays only its own entry, never the dropdown.
- **Ephemeral VFSes** (archives, search results) are hidden from the dropdown: they're reachable via navigation history, auto-unmount when no pane references them, and listing them as switch targets would just be noise.

---

## 9. Session and Connection Management

### Local Mode (Default)

All operations run directly in the Tauri process. No agent subprocess, no serialization, no network. This is the default when launching Newt without arguments.

### Connection Profiles and Quick Connect

**Connection profiles** are saved connection configurations stored in `connections.toml` under Tauri's platform-specific application configuration directory for `org.newt-fm.newt`: `~/Library/Application Support/org.newt-fm.newt/` on macOS, `$XDG_CONFIG_HOME/org.newt-fm.newt/` (falling back to `~/.config/org.newt-fm.newt/`) on Linux, and `%APPDATA%\org.newt-fm.newt\` on Windows. Secrets (e.g., AWS access keys) are stored in the system keychain (macOS Keychain, Linux Secret Service via `keyring` crate) under the service name `com.newt.credentials`.

**Profile types**:
- **S3**: Region, bucket, endpoint URL, credential mode (default/profile/IAM user/assume role), and associated secrets.
- **SFTP**: Host (`user@hostname`).
- **SSH**: Host (`user@hostname`) + optional `forward_agent` flag (`-A`) + `login_shell` (defaults true). Connecting opens a new window.
- **Docker** / **Podman**: Container name + optional user + `bootstrapless` flag (defaults to true: `docker cp` / `podman cp` + direct exec; disable to use the sh-bootstrap path with hash-keyed caching).
- **Kube**: kubectl context, namespace, pod, container.
- **Custom**: Caller-supplied shell command run locally via the platform shell (`sh -c` on Unix, `cmd.exe /C` on Windows). The bootstrap script is exposed as `$NEWT_BOOTSTRAP` for the user to interpolate (so anything from `ssh foo@bar "$NEWT_BOOTSTRAP"` to `bash -c "$NEWT_BOOTSTRAP"` to elaborate nsenter / firejail recipes works).

Spawn-style profiles additionally carry **`open_in`** (`window` default, or `pane`): whether activating the profile opens a full session window or mounts the target as an agent VFS in the active pane.

**Save and Connect are independent actions.** Every connect/mount dialog (Connect, Mount SFTP, Mount S3) has the same footer — `[Cancel] [Save] [Connect/Mount]` — with Connect as the primary (Enter) action and Save also bound to Mod+S. Connect uses the current form values and never writes a profile (ad-hoc use lands in Quick Connect's Recents automatically); Save persists the form as a profile and never connects. Whether Save creates or updates is decided by provenance: a cold dialog creates (id slugged from the name), a dialog opened from a saved profile — whether via F4-edit or activation — updates in place with the stable id, so renames don't fork a new profile, and a cold dialog's *second* Save updates the profile minted by the first. Save keeps the dialog open (the button briefly reads "Saved"), so save-then-connect is one flow. The profile-name row is progressively disclosed: hidden until save-intent — the button reads "Save…" and the first press (or Mod+S) reveals the row (muted, separated by a rule, auto-named from the target, focused and preselected) without saving; Enter in the row or a second Save persists, and Escape in the row backs out of the reveal instead of closing the dialog. Opened from a saved profile, the row is pinned visible from the start (it *is* the thing being edited) with the same subordinate styling. For IAM-user S3, Connect uses the typed keys and drops them; Save writes them to the keychain.

**Quick Connect** (Ctrl+R): A fuzzy-searchable palette listing recent ad-hoc connections and all saved connection profiles.

- **Search**: Searches across name, ID, bucket, host, region, and endpoint URL.
- **Each entry shows**: Connection name, type badge, and relevant details (bucket, host, region, etc.).
- **Recent section**: Ad-hoc connections — ones connected without saving a profile — are remembered in a bounded MRU (`recent_connections` in `state.json`, most-recent first, capped at 12) and shown in a separate "Recent" section above the saved profiles. Only the secret-free target is stored (SSH auth, S3 keys, etc. never land in `state.json`); all kinds are recorded, S3 as its secret-free config (region/bucket/endpoint/credential mode — IAM-user keys are used for the mount and dropped). A recent that now matches a saved profile is filtered out — the profile already covers it. **Delete** (or the ×) forgets a recent.
- **Enter**: Activates the selected connection (recent or saved). Spawn targets with `open_in: window` open a new session window directly — it has its own connection screen. Everything VFS-bound (S3, SFTP, pane-scoped agent mounts; pane-scoped spawn profiles are marked "pane mount") activates *through* its dialog: the matching Connect / Mount SFTP / Mount S3 dialog opens prefilled and submits itself, doubling as the connection-progress surface — the mount/bootstrap log streams while connecting, and on failure the dialog stays open with everything editable for a retry. The one case that doesn't auto-submit is an IAM-user S3 *recent*: its keys were never persisted, so the dialog waits for re-entry (a saved IAM-user profile fetches its keys from the keychain and auto-connects; if the keychain entry is gone it too stays open).
- **Delete**: Removes the selected saved profile (also its keychain secrets) or forgets the selected recent — with inline Yes/No confirmation.
- **F4** (or the ✎ pencil): Opens the selected entry in the matching dialog (Connect, Mount SFTP, or Mount S3) prefilled with its fields, without auto-connecting. For a saved profile the title carries the profile name, Save updates in place (stable id across renames; keychain secrets included — an IAM-user S3 profile prefills its key pair from the keychain), and Connect is a one-off with the current form values that leaves the profile untouched — which also serves as the per-activation `open_in` override (flip "Open as a new session", Connect, don't Save). For a recent, Save *promotes it to a saved profile* (after which the identity filter hides the recent), and Connect is plain edit-before-connect.
- **Escape**: Closes the palette.
- Empty state: "No saved connections. Use the connect or mount dialogs to save one."

### Remote Sessions

Newt opens an agent session over any of these transports. The frontend / IPC layer is identical regardless of transport — only the spawn step differs.

| Transport | CLI | Notes |
|---|---|---|
| Local | (default) | No subprocess; services run in-process. |
| SSH | `--target=ssh:user@host` | Uses `~/.ssh/config`, askpass for passwords / host keys. Login-shell bootstrap (see below). |
| SSH (agent forwarding) | `--target=ssh-agent:user@host` | Adds `-A`. Lets the remote agent's SSH/SFTP invocations reuse host keys. |
| pkexec | `--target=pkexec` | Linux only. Elevated agent via Polkit. |
| Elevated | `--target=elevated` / `--elevated` | Linux: pkexec. Windows: UAC (`ShellExecuteEx "runas"`) + named-pipe agent. |
| Docker | `--target=docker:[user@]<container>` | Default: bootstrapless (`docker cp` + direct exec). Local engine, fast transfer, works for sh-less images. |
| Docker (bootstrap) | `--target=docker-bootstrap:[user@]<container>` | Opt back into the sh bootstrap (hash-keyed agent cache; avoids re-upload on reconnect). |
| Podman | `--target=podman:[user@]<container>` | Same shape / default as docker. |
| Podman (bootstrap) | `--target=podman-bootstrap:[user@]<container>` | Same shape as docker-bootstrap. |
| Kubernetes | `--target=kube:[context/][namespace/]pod[:container]` | `kubectl exec -i`. Bootstrap-only (kubectl cp itself needs tar). |
| Custom | `--target='custom:<shell command>'` | Runs locally via the platform shell (`sh -c` / `cmd.exe /C`); bootstrap exposed as `$NEWT_BOOTSTRAP` for the user to splice in (e.g. `ssh host "$NEWT_BOOTSTRAP"`, `bash -c "$NEWT_BOOTSTRAP"`). |
| WSL | `--wsl` / `--wsl <NAME>` | Windows only. Bare `--wsl` uses the default distro. No bootstrap (the bundled musl agent is exec'd directly via its `/mnt/<drive>/…` path). Not a `--target` scheme and has no saved profiles. |

The Connect dialog (Mod+Shift+R) exposes the same set as a transport-picker form. For Docker/Podman/Kube the dialog populates a combo-box with live targets (`docker ps`, `podman ps`, `kubectl get pods`), and for SSH it parses `~/.ssh/config` for host aliases. Discovery is per-dialog ephemeral state — no persistent caching.

**Open as a new session** (checkbox in the Connect dialog's button row): checked opens a full remote session in a new window; unchecked mounts the target's filesystem as an agent VFS in the current pane instead (see "Agent Mounts" in the VFS section). The default follows the session: checked in local sessions, unchecked in remote ones — connecting from inside a remote session usually means peeking into one of *its* containers. The choice is saved on connection profiles (`open_in`, default `window`) and honored by Quick Connect. A pane mount is established by the *session's* agent — the target is reached with the remote host's ssh/docker/kubectl, credentials, and network. Discovery follows the same side (a remote session lists the remote's targets), and exited/dead containers are filtered out — they can't be exec'd into.

**Login-shell bootstrap** (SSH, on by default; per-profile `login_shell`): the agent is started under `exec "$SHELL" -lc '<script>'` so it inherits the target's real login environment — the same one a bare `ssh host` would have given you, since sshd only execs a login shell when handed *no* command. `$SHELL` rather than `sh` so a bash user gets `~/.bash_profile`. This is ambient rather than resolved: everything downstream of the agent — terminals, `sh -c` commands, git, discovery — inherits it, so no `PATH` probing or manual patching is needed on the remote side. It is safe because the handshake already skips any non-`NEWT:` line, tolerating profiles that print banners, and because `-lc` takes the script from argv and leaves stdin free for RPC.

Off for `docker exec`-style transports: those are non-login by design and already carry the image's `ENV`, so a login shell would only add risk on images with a minimal `sh` and no `/etc/profile`. WSL is also non-login for now — it execs the agent directly with no handshake, so profile chatter would land in the RPC stream (see TODO.md).

**Bootstrap protocol** (SSH / Docker / Podman / Kube / Custom):
1. Newt spawns the transport process and sends a bootstrap shell script (`scripts/bootstrap.sh`) to it on stdin.
2. The script detects platform and architecture (`uname -s`, `uname -m`).
3. It checks a cache directory (`~/.cache/newt/`) for a matching `newt-agent` binary (keyed by a blake3 hash of the local agent binary).
4. If cached: Executes immediately (`NEWT:READY`).
5. If missing: Requests upload (`NEWT:NEED:triple:caps`). Newt gzip-compresses the agent binary if the remote supports it and uploads it. The script caches it for future use, cleans up old versions, and confirms with a second `NEWT:READY` — the host holds off RPC traffic until then, because some `head -c` implementations (BSD/macOS) read ahead and would swallow bytes sent while the upload is still being consumed.
6. The agent enters RPC mode; all further communication is bincode over stdin/stdout.

**Bootstrapless (direct-copy) protocol** (Docker / Podman only):
1. Newt runs `<engine> inspect --format='{{.Os}}/{{.Architecture}}' <container>` and maps the result to an agent target triple.
2. It runs `<engine> cp <local-agent> <container>:/tmp/newt-agent-<hash>`.
3. It execs `<engine> exec -i <container> /tmp/newt-agent-<hash>` and uses that pipe as the RPC channel.
No shell or coreutils in the container required, but every connect re-uploads (no cache).

**After connection**: All filesystem operations, terminal PTYs, file operations, and VFS mounts execute on the remote side. The UI is identical to local mode. If `behavior.expose_local_fs` is enabled, the client-local filesystem is automatically mounted as a Remote VFS (see VFS section).

**Askpass** is only wired for SSH; daemon-mediated transports (docker / kubectl / podman) skip it since the daemon handles auth out of band.

**Connection logging**: Every step (transport launch, bootstrap progress, agent startup) is logged. The Connection Log dialog shows it in real-time. Transport stderr is captured in a background task and appended.

**Process safety** (Linux): Spawned transports run with `PR_SET_PDEATHSIG=SIGTERM`, so if the Tauri process crashes the agent is killed too. Prevents zombies on the remote host.

### Elevated Mode (Linux pkexec / Windows UAC)

**Connecting**: Via command palette ("Open Elevated"), `--elevated`, or `--target=elevated`. Available on Linux and Windows (macOS has no equivalent with usable IPC). Same session UX as any remote: connection overlay, reconnect (re-prompts), child watcher.

**Linux**: spawns `pkexec <agent-binary-path>`; the Polkit dialog prompts for the password; RPC runs over the agent's stdin/stdout. The agent runs as root.

**Windows**: `ShellExecuteEx "runas"` launches the native `newt-agent.exe` elevated (UAC consent prompt). Because `runas` cannot redirect stdio, RPC instead runs over a **named pipe**: the host creates a single-instance server at an unguessable `\\.\pipe\newt-elevated-<uuid>` and passes `--pipe <name>` to the agent, which connects back. The host GUI stays **unelevated** (only the agent is elevated) — drag-and-drop / clipboard from normal apps keep working. Declining UAC surfaces a friendly "Elevation request was declined" in the connection overlay. Agent stderr/logs are unavailable in this mode (`runas` carries no console/stdio).

*Security model*: the boundary protected is **other users / lower trust** — the unguessable UUID name, `first_pipe_instance` + `max_instances(1)` (no squatting / single connection), and the default named-pipe DACL (creating user + admins) gate access. No auth handshake, deliberately consistent with the existing askpass/conpty named pipes. It does not (and cannot) defend against a same-user attacker, who can already tamper with the unelevated Newt process itself — identical to the Linux pkexec situation.

### WSL Sessions (Windows only)

**Connecting**: Via command palette ("Connect to WSL Distribution...", no default keybinding) or the `--wsl[=<NAME>]` CLI flag. With exactly one installed distribution the command connects immediately; with several it opens a fuzzy-searchable picker (default distro listed first); with none it reports "No WSL distributions installed".

Distributions are enumerated by reading the `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss` registry key (the source Windows Terminal / VS Code use) — `wslapi.dll` has no list API. The session is launched via `wslapi!WslLaunch`, which is loaded at runtime with `LoadLibraryW` (never linked), so a machine without WSL just fails this one transport instead of failing to start.

No bootstrap or upload: the bundled Linux-musl agent already lives on the Windows filesystem, so it is exec'd directly from its translated `/mnt/<drive>/…` path (DrvFs default-mounts world-exec). The agent architecture is taken from the Windows host arch (correct for WSL2 and x64 WSL1). The `WslLaunch` process is a normal Win32 process handle wrapped in a small adapter (Rust's `Child` can't adopt a pre-existing handle); closing the window terminates it. WSL is a remote-style session — `behavior.expose_local_fs` mounts the client-local filesystem as a Remote VFS, same as SSH. There are no saved WSL connection profiles by design.

`WslLaunch`'s relay process attaches to the caller's console, so a windows-subsystem (production) build — which has none — would get a visible console window popped per launch. Before the first launch the process pre-owns a hidden console: `AllocConsoleWithOptions(NO_WINDOW)` where available (Win11 24H2+, resolved dynamically), else `AllocConsole` + hide (one brief flash, first WSL session only). Console-subsystem dev builds and terminal launches already have a console and are untouched.

### Connection Status

Displayed as an overlay on the main window during connection:
- **Connecting**: Shows progress message and log.
- **Connected**: Overlay disappears, normal operation.
- **Disconnected**: Shows error message and a "Reconnect" button.
- **Failed**: Shows error details and connection log.

### Askpass Integration

When SSH needs interactive input (password, passphrase, host key verification), Newt handles it entirely within the app:

1. SSH invokes the askpass helper (the `newt-agent` binary in askpass mode, set via `SSH_ASKPASS` environment variable).
2. The helper connects to a Unix domain socket whose path is passed in via `NEWT_ASKPASS_SOCK`. The socket is owned by whichever process spawned `ssh` (the host for the main remote-session transport, the agent for SFTP mounts in a remote session).
3. The askpass listener forwards the request to an `AskpassProvider`. The host's provider drives the UI directly; the agent's provider proxies the request back to the host over the `API_HOST_ASKPASS` reverse RPC, so the dialog always appears in the host window regardless of where the `ssh` process actually runs.
4. The dialog shows:
   - **Title**: "Host Key Verification" (for host key prompts containing "yes/no"), "Authentication" (for passwords), or "SSH" (for other prompts).
   - **Input field**: Password field (masked) for secrets, text field for confirmations.
   - For host key confirmation: submitting empty input defaults to "yes".
5. The user's response is sent back through the socket to SSH, and authentication continues.

### Reconnect

After disconnection, a "Reconnect" button appears. Clicking it reconnects in-place on the same window using the same transport parameters (SSH host, elevated mode, etc.): the old session is torn down (agent subprocess terminated, PTYs killed), panes / terminals / operations are cleared, and a fresh session is established.

---

## 10. Hot Paths and Bookmarks

### Hot Paths Dialog (Mod+P)

A fuzzy-searchable palette for quick navigation to common locations.

**Fuzzy search algorithm**: Two-pointer consecutive character matching (case-insensitive). Score = length of longest consecutive match. Higher scores sort first within each category.

**Categories** (displayed in this order):

| Category | Source |
|----------|--------|
| User Bookmarks | User-added bookmarks from `settings.toml` `[[bookmark]]` entries |
| Standard Folders | Home, Desktop, Downloads, Documents, Pictures, Music, Videos |
| System Bookmarks | GTK bookmarks (`~/.config/gtk-3.0/bookmarks`) on Linux |
| Mounted Volumes | Entries in `/proc/self/mountinfo` filtered to `/media/`, `/run/media/`, `/mnt/` on Linux; `/Volumes` on macOS; logical drives (`C:`, `D:`, …) on Windows |
| Mounted VFS | Currently mounted S3, SFTP, and archive filesystems |
| Recent Folders | `recently-used.xbel` on Linux (top 20 by modification time); Finder GoToFieldHistory on macOS |

Each category can be independently toggled on/off in preferences (Hot Paths section).

**Each entry displays**: Name (if bookmarked or named) + path. Matching characters in the fuzzy search are highlighted.

**Keyboard navigation**: Arrow keys, Page Up/Down, Home/End, Enter to navigate to the selected path, Escape to close.

Selecting an entry navigates via `navigate_to_path`, which takes the whole `VfsPath` — `vfs_id` included, so an entry on a mounted VFS lands on that VFS. Removal is keyed by `bookmark_key`, the verbatim `[[bookmark]]` `path` from settings.toml, which is a display path rather than the wire form.

### Bookmark Operations

- **Add Bookmark** (Mod+B): Bookmarks the active pane's current directory. Optional custom name (defaults to the directory name). Stored as `[[bookmark]]` in `settings.toml`. The new entry goes to the *top* of the list, and bookmarking an already-bookmarked path moves it there instead of adding a second copy (any duplicates a hand-edited file already had are collapsed at the same time).
- **Bookmark bubble**: Mod+B pops a small acknowledgement dialog ("Bookmark Added" / "Bookmark Moved to Top", with the name and path) anchored top-right, where a browser's bookmark popover lives — it's the one modal that isn't centred. It is *not* a confirmation — the bookmark is already written, so Escape, click-away and **Done** all keep it. **Undo** restores the whole `[[bookmark]]` array as it was, so undoing a move-to-top puts the entry back at its old position with its old name rather than deleting it. The undo snapshot lives in session state, not in the pushed modal payload (no reason to ship the user's settings file to the frontend), and it only rewrites `[[bookmark]]`, so unrelated settings changes made while the bubble is open survive.
- **Remove Bookmark**: Press Delete on a user bookmark in the Hot Paths dialog. Shows an inline confirmation (Yes/No) — during confirmation, all other keys are swallowed except Enter/Y (confirm), N (cancel), and Escape (cancel). Removes every entry for that path.

---

## 11. Command Palette (Mod+Shift+P or F1)

Fuzzy-searchable list of all available commands.

- **Search input** (auto-focused): "Start typing to filter commands".
- **Fuzzy matching**: Same algorithm as Hot Paths.
- **Context filtering**:
  - Commands with `needs_pane = true` are hidden when no pane is focused.
  - User commands with an `applies_to` run filter are evaluated against current state:
    - `"file"`: Only if focused item is a regular file.
    - `"directory"`: Only if focused item is a directory.
    - `"selection"`: Only if files are selected, or a non-`..` file is focused.
  - Self-referencing commands (`command_palette`, `hot_paths`, `user_commands`) are excluded.
- **Display**: Each entry shows the command name (with search matches highlighted), category badge (e.g., "User" for user commands), and keyboard shortcut (rendered with platform-specific symbols: ⌘ on macOS, Ctrl elsewhere).
- **Keyboard**: Arrow keys, Page Up/Down, Home/End to navigate. Enter to execute. Escape to close. Wraps around (loop).

### User Commands Palette (F9)

Same as the Command Palette but filtered to show only user-defined commands (category = "User").

---

## 12. User-Defined Commands

Custom commands defined in `settings.toml` via `[[command]]` entries. Managed via the Settings dialog (Commands tab) or by editing the TOML file directly.

### Command Definition

```toml
[[command]]
title = "Archive Selection"
run = "tar czf {{ file.stem }}.tar.gz {{ files | map(attribute='name') | map('shell_quote') | join(' ') }}"
key = "alt+z"             # Optional keyboard shortcut
terminal = true           # true = run in terminal tab, false = run as background operation
keep_terminal_open = true # Terminal commands: hold the tab open past exit
silent = true             # Non-terminal commands: no progress window
applies_to = "selection"  # Optional run filter: "file", "directory", "selection" (omit = any)
```

### Template Engine (Minijinja / Jinja2)

Templates are rendered with Minijinja. A **two-pass execution** model handles interactive inputs:

1. **Pass 1 (Scanning)**: The template is rendered with empty `prompt()` responses. All `prompt()` labels and `confirm()` messages are collected. If any are found, a modal dialog appears to collect user input.
2. **Pass 2 (Execution)**: The template is re-rendered with actual user responses, and the resulting command string is executed.

If the user declines a `confirm()`, the entire command is aborted.

### Template Variables

| Variable | Type | Description |
|----------|------|-------------|
| `file` | Object | Currently focused file |
| `file.name` | String | Filename with extension |
| `file.path` | String | Full absolute path |
| `file.source` | String | Underlying real path for virtual entries (e.g. search hits); undefined for ordinary files |
| `file.stem` | String | Filename without extension |
| `file.ext` | String | Extension (without dot) |
| `file.is_dir` | Bool | Is it a directory? |
| `file.size` | Number | Size in bytes (may be undefined) |
| `file.modified` | Number | Unix timestamp in seconds (may be undefined) |
| `files` | Array | Selected files, or `[file]` if nothing selected |
| `dir` | String | Active pane's current directory (absolute path) |
| `other_dir` | String | Other pane's current directory |
| `hostname` | String | Machine hostname |
| `env.NAME` | String | Environment variable (e.g., `env.HOME`, `env.PATH`) |

### Custom Filters

| Filter | Description | Example |
|--------|-------------|---------|
| `shell_quote` | Shell-escape a string | `{{ file.name \| shell_quote }}` → `'my file.txt'` |
| `basename` | Extract filename from path | `{{ file.path \| basename }}` |
| `dirname` | Extract directory from path | `{{ file.path \| dirname }}` |
| `stem` | Filename without extension | `{{ file.name \| stem }}` |
| `ext` | Extract extension | `{{ file.name \| ext }}` |
| `regex_replace(pattern, replacement)` | Regex substitution | `{{ file.name \| regex_replace("\.bak$", "") }}` |
| `join_path` | Join path segments | `{{ [dir, "subdir"] \| join_path }}` |

All standard Jinja2 built-in filters are also available (`map`, `join`, `upper`, `lower`, `selectattr`, etc.).

### Custom Functions

| Function | Signature | Description |
|----------|-----------|-------------|
| `prompt(label, default="")` | `(string, string?) → string` | Shows a text input dialog. Returns user input or default. |
| `confirm(message)` | `(string) → bool` | Shows a yes/no dialog. Returns true if confirmed. Aborting cancels the whole command. |

### Execution Modes

**Terminal mode** (`terminal = true`):
- Renders the template into a shell command.
- Creates a new terminal tab and executes: `sh -c "rendered_command"`.
- Working directory: the active pane's current path.
- Terminal becomes visible and focused. Output appears in real-time.
- `keep_terminal_open = true` holds the tab open after the command exits (the "Press Enter to close" line) even when `behavior.keep_terminal_open` is off. One-way: the effective value is the global setting OR the per-command one, so a command cannot force a tab to close against the global preference.

**Operation mode** (`terminal = false`):
- Renders the template into a command string.
- Executes as a background operation (same as copy/move/delete).
- Shows progress in the Operations Panel.
- Can be backgrounded.
- `silent = true` suppresses the operation's UI entirely: no progress modal, no row in the operations panel, so a command that finishes in milliseconds leaves no flicker behind. Distinct from *backgrounded*, which is a visible operation the user pushed aside and which keeps its panel row. A failure clears the flag, so a failed silent command surfaces exactly like any other operation.

The two flags are mode-specific: the Commands tab shows "Keep terminal open" or "Run silently" depending on the "Run in terminal" checkbox, and each is ignored in the other mode.

### User Command Input Dialog

When a template uses `prompt()` or `confirm()`, a modal dialog appears before execution:
- Shows the command title.
- Lists all `confirm()` messages as checkboxes.
- Lists all `prompt()` inputs as text fields (with labels and defaults).
- **Special case**: A single `confirm()` with no `prompt()` calls renders as a simple Yes/No dialog.
- Cancel aborts the command. Run executes with collected inputs.

---

## 13. Preferences and Configuration

### Settings File

Stored as `settings.toml` under Tauri's platform-specific application configuration directory for `org.newt-fm.newt`:

| Platform | Configuration directory |
|----------|-------------------------|
| macOS | `~/Library/Application Support/org.newt-fm.newt/` |
| Linux | `$XDG_CONFIG_HOME/org.newt-fm.newt/`, falling back to `~/.config/org.newt-fm.newt/` |
| Windows | `%APPDATA%\org.newt-fm.newt\` |

The file is hot-reloaded — changes are picked up within 200ms and applied without restart.

### Runtime State File

`state.json` in the same platform-specific application configuration directory — machine-written, ephemeral-ish UI state, kept out of `settings.toml` (which stays purely user-authored). Plain JSON managed by `RuntimeStateManager` (`src-tauri/src/runtime_state.rs`): loaded once at startup (corrupt/missing → defaults), written on each discrete change, and broadcast app-wide via the `update:runtime-state` event (consumed by the `useRuntimeState` hook). Updated by dotted-key commands (`update_runtime_state`), validated against the typed `RuntimeState` struct (unknown keys rejected). Holds per-pane column widths (`column_widths.<pane>.<column>`), the app-wide webview zoom factor (`zoom`), the terminal panel height (`layout.terminal_height`; the file-pane split deliberately stays 50/50), sticky last-used dialog toggles (`copy_move.*`, `search.*`), and the recent ad-hoc connections MRU (`recent_connections`, see Quick Connect). Intended home for future layout state (window geometry). No file watcher — external edits apply on next launch.

### Full Settings Structure

```toml
profile = "work"  # Optional: loads profiles/work.toml under the app configuration directory

[appearance]
show_hidden = false         # Show files starting with "."
folders_first = true        # Directories before files in sort order
show_command_bar = true     # Show F-key bar at bottom of window
show_pane_header = true     # Show breadcrumb / VFS selector / free-space header per pane
show_pane_status = true     # Show file count / selection size status bar per pane
theme = "system"            # "system", "light", or "dark"
columns = ["name", "size", "modified_date", "modified_time", "user", "group", "mode"]
si_size_prefixes = false    # Size column shows "1.5 GB" instead of exact byte counts
size_units = "decimal"      # "decimal" (kB/MB/GB, 1000) or "binary" (KiB/MiB/GiB, 1024)
locale = ""                 # BCP-47 tag for numbers/dates (e.g. "de-DE"); "" = system regional format
date_format = ""            # strftime-style date column format (e.g. "%Y-%m-%d"); "" = system locale
time_format = ""            # strftime-style time column format (e.g. "%H:%M"); "" = system locale

[behavior]
confirm_delete = true       # Ask for confirmation before deleting
delete_to_trash = true      # Move deletes to the OS trash; Delete Permanently bypasses
keep_terminal_open = true   # Keep terminal tab open after shell exits
keep_finished_operations = false  # Keep completed/cancelled ops in panel
quick_search = true         # Use prefix quick-search; when false, typing opens regex filter
expose_local_fs = false     # Expose local filesystem to remote host in SSH sessions
default_sort = { key = "name", ascending = true }
history_retention = 200     # Max entries kept per pane in nav history (0 = unlimited)
shell_integration = true    # `newt` CLI in built-in terminals / user commands (local sessions; remote always on)

[enrichers]
git_status = true           # Git enricher: per-row status colors + branch badge

[archives]
default_format = "tar_zst"  # Format preselected in Pack to Archive: "zip", "tar", "tar_gz", "tar_xz", "tar_zst"
preserve_symlinks = true    # Store symlinks as symlinks (false: follow them)
zip_level = 6               # Deflate level for zip (0-9, 0 = store)
gzip_level = 6              # tar.gz level (0-9)
xz_level = 6                # tar.xz level (0-9)
zstd_level = 3              # tar.zst level (1-22)

[hot_paths]
standard_folders = true     # Show Home, Downloads, Documents, etc.
system_bookmarks = true     # Show GTK bookmarks (Linux)
mounts = true               # Show mounted volumes
recent_folders = true       # Show recently visited directories

[editor]
word_wrap = false           # Default word wrap in the text editor (per-file toggle still overrides)

[viewer]
image_background = "checkerboard" # Backdrop behind images: "checkerboard", "dark", "light"

[[bookmark]]
path = "/home/user/projects"
name = "My Projects"        # Optional

[[bind]]                    # Several entries may target the same command — each adds a binding
key = "mod+shift+f5"
command = "some_command"    # `command = "-"` disables whatever the key is bound to
when = "pane_focused"       # Optional: "pane_focused", "terminal_focused", "viewer", "editor"

[[command]]
title = "My Command"
run = "echo {{ file.name }}"
key = "alt+z"               # Optional
terminal = true             # Optional, default false
applies_to = "file"         # Optional
```

### Profile System

The `profile` field in `settings.toml` loads an additional TOML file from `profiles/<name>.toml` under the same platform-specific application configuration directory. Profile settings deep-merge on top of user settings (scalars are replaced, tables are merged).

### Settings Dialog (Mod+,)

Three tabs:

**Settings tab**:
- Sidebar with category filter (All, Appearance, Behavior, Hot Paths). Category names from schema titles.
- Search box for full-text search across setting titles and descriptions.
- Each setting rendered as a row with title, description, and appropriate control:
  - Boolean → checkbox.
  - Enum → dropdown.
  - Number → number input.
  - String → text input.
  - Custom widgets for complex preferences (rendered below the description):
    - **Columns**: Visible/Hidden panels side by side — visible rows carry a drag handle (mouse drag to reorder, arrow keys when focused); checkboxes toggle simple columns; timestamps get a presentation dropdown (Date & time / Date only / Separate columns).
    - **Default Sort**: Dropdown for sort key + ascending checkbox.
- **Reset button**: Appears next to settings that have been explicitly set in `settings.toml`. Clicking removes the key from the file, reverting to the cascade default.
- Changes are saved immediately to `settings.toml` and proactively reloaded (not relying solely on file watcher).

### Debug Dialog

Available in debug builds only. Provides:
- **Toggle DevTools**: Opens/closes the WebKitGTK inspector.
- **Reload Window**: Reloads the frontend UI.
- **Crash (throw error)**: Tests the ErrorBoundary by throwing a React error.

**Keybindings tab**:
- Table listing every command (built-in + user) with its current shortcuts and dispatch context. The "When" column shows the command's intrinsic dispatch context (e.g. "Pane focused"), independent of whether a key is currently bound.
- **Multiple bindings per command** are first-class: a command can carry any number of keys (e.g. Delete Selected defaults to F8 + Del, and ⌘⌫ on macOS; Command Palette to Mod+Shift+P + F1; viewer zoom-in to = with +/Shift+= synonyms). The first key is the primary — the one menus, tooltips and the command bar display. Extra keys are ordinary `[[bind]]` entries; removing one default from the set writes a single `command = "-"` disable entry for that key.
- Search/filter by command name, ID, shortcut, or context.
- Shortcuts rendered with platform-specific symbols (⌘ on macOS, Ctrl elsewhere).
- **Inline editor**: Click Edit (or double-click a row) to swap the shortcut cell into one key-capture input per binding, with per-row remove and "+ Add key" (user commands are limited to one key — theirs lives on the `[[command]]` entry). Press a combination to record; Escape cancels recording; the × clears a row.
- **Live conflict detection** as you record:
  - **Hard conflict** (same key + same dispatch context for another command) blocks Save and shows an "Already used by …" banner with an Override button. Override only *acknowledges* the conflict — it doesn't save until you press Save.
  - **Soft warning** when the same key is used in a different/overlapping context.
  - **Validation** rejects modifier-only combos.
- **Action buttons** (in edit mode): Save (primary), Cancel, Reset (always shown when the command has defaults — disabled when already at the default set, otherwise restores the compiled-in default keys).
- **Reset is bidirectional**: if a different command currently squats on the row's default key+context — including a user command holding it via `[[command]].key` — Reset evicts the squatter so the default reasserts. The squatter's other fields (title/run/applies_to) are preserved.
- **Modified indicator**: a small accent dot next to commands whose resolved binding differs from the compiled-in default.

**Commands tab**:
- List of user-defined commands. Each row shows the title and shortcut in a header line (shortcut right-aligned, matching the Keybindings tab), the run script in a monospace `<pre>` block (text-selectable, with `max-height` and internal scroll for long scripts), and small uppercase tags below for `applies to …` and `terminal`.
- Edit button per row. Delete is reachable inside the edit form (one extra click of friction protects against misclicks).
- **Edit mode**: the row is replaced by a form (title, run textarea, Key — same KeyCaptureInput as the Keybindings tab in `regular` size, Applies to — Any focused item / Files only / Directories only / Selection, Run in terminal). Conflict detection runs against all bindings (built-in + user). Action bar: Delete on the far left, Cancel + Save on the far right (Save is the rightmost primary action).
- **Add Command** button stays visible while editing an existing command.
- Expandable template reference panel showing variables, filters, and functions, with example commands rendered as the same kind of `<pre>` blocks used in row view.

**Footer**: a "Settings file" label beside a segmented pair of icon buttons — **Show in pane** navigates the pane the dialog was opened from to `settings.toml` and focuses it (closing the dialog), **Open in external editor** hands the file to the system handler. Both create the file with a commented skeleton if it doesn't exist yet. Show in pane needs the host machine's filesystem mounted in the session (`VfsInfo::host_local_vfs_id` — always true locally, true in a remote session while the client-local mount is present) and is hidden otherwise, leaving external open as the only route.

### Keybinding System

Bindings are resolved in cascade order (later overrides earlier):
1. **Default bindings**: Built into the application (see shortcut reference table).
2. **User overrides**: `[[bind]]` entries in `settings.toml`.
3. **Profile overrides**: `[[bind]]` entries in the profile TOML.

**Key format**: Lowercase, `+`-separated. Examples: `mod+shift+p`, `f5`, `alt+enter`, `ctrl+shift+~`.

**`mod+` prefix**: Expands to `ctrl+` on Linux/Windows, `meta+` (Cmd) on macOS.

**Disabling a binding**: Set `command = "-"` to unbind:
```toml
[[bind]]
key = "f8"
command = "-"  # Disables the default F8 = delete binding
```

**`when` conditions** on `[[bind]]` entries gate the *dispatch context* — which input focus state allows the binding to fire:
- (omitted) → Global; the binding fires regardless of focus.
- `"pane_focused"` → Only when a file pane has focus.
- `"terminal_focused"` → Only when the terminal has focus.

Not to be confused with `applies_to` on `[[command]]` entries, which is a *run filter* gating whether a user command appears in the palette / can be invoked at all (`"file"`, `"directory"`, `"selection"`, or omitted = any). The two concepts share neither schema location nor accepted values.

**Shortcut display**: Rendered with platform symbols:
- `ctrl` → "Ctrl"
- `meta` → "⌘" (macOS) / "Super" (other)
- `shift` → "Shift"
- `alt` → "⌥" (macOS) / "Alt" (other)
- Other keys: Capitalized (e.g., `f5` → "F5")

### Open Config File

Available from the command palette. Opens `settings.toml` in the built-in editor for direct editing.

---

## 14. Command Bar

Optional bottom bar (toggled in preferences: `appearance.show_command_bar`, default: on).

Shows clickable buttons for frequently used commands, each displaying the command name and its keyboard shortcut:

Command Palette | Rename | View | Edit | Copy | Move | Create Directory | Delete | User Commands

Clicking a button executes the command.

---

## 15. Focus Management

Focus is a first-class concern — broken focus means reaching for the mouse, which defeats the keyboard-centric design.

### Focus Tracking

- **Active pane**: Tracked in Rust state (`display_options.active_pane` — 0 or 1). Tab switches between panes. Clicking a pane activates it.
- **Panes vs. terminal**: `display_options.panes_focused` (boolean) tracks whether panes or the terminal have input focus. Alt+Up/Down toggles.
- **Active terminal**: `display_options.active_terminal` tracks which terminal tab is active.

### Modal Focus

- **On open**: Auto-focuses the most relevant control — the text input in input dialogs, the confirm button in confirmation dialogs. Uses `autoFocus` or ref-based `.focus()`.
- **On close**: Focus *always* returns to the previously active pane or terminal. Implemented via `onCloseAutoFocus` on Radix Dialog, which calls `refocusActivePane` (increments `focusGeneration` → Pane re-runs its focus effect).
- **Tab key**: Disabled inside modals (focus is managed by the app, not browser tab order).
- **Command middleware**: All `cmd_*` commands automatically close any open modal before dispatching, preventing stale modal state.

### Focus Theft Prevention

- Clicking splitter dividers, pane headers, column headers, or other non-interactive chrome does not steal focus from the file list or terminal.
- Most interactive elements use `tabindex=-1` (focus managed by app, not browser).

---

## 16. Miscellaneous Features

### Outbound Links

Every link that leaves the app goes through the `open_url` command, which hands the URL to `opener` on the host side. An `<a target="_blank">` is not an option: the webview has no new-window handler installed, so a blank-target link is silently swallowed on every platform. Links are therefore `<button>`s styled as links.

`documentation` ("Documentation...", Help category, no default keybinding) opens `newt-fm.org/docs/` from the command palette.

### About Dialog

Available from the command palette. Shows:

- **App icon** (96×96), title ("Newt"), tagline ("A keyboard-centric dual-pane file manager").
- **Version**: e.g., `v0.1.0 (a1b2c3d+)` — short git hash with `+` suffix if built from a dirty working tree.
- **Build date** and **target triple** (e.g., `x86_64-unknown-linux-gnu`).
- **Links**: Website (newt-fm.org), Documentation (newt-fm.org/docs/), GitHub — each hands the URL to the system browser via `open_url`.
- **License**: GNU General Public License v3.0 or later.
- **Third-Party Notices…**: opens the notices dialog (below).
- **Easter egg**: Click the icon 3 times to display a random newt fact (12 facts in rotation). The icon rotates slightly on activation.

Build metadata (git revision, date, target) is captured at compile time via `build.rs` and gracefully falls back when git is unavailable.

### Third-Party Notices Dialog

Its own dialog, opened by the "Third-Party Notices…" button in the About box —
deliberately not a command in its own right. Renders `THIRD-PARTY-NOTICES.md`
in a scrollable monospace view; the scroller takes focus on open so Page/arrow
keys work immediately.

The notices are `?raw`-imported into the frontend bundle at build time, so the
text ships inside the app shell and needs no filesystem access at runtime.
`cargo xtask notices` regenerates the file: it walks the *normal* dependency
edges out of `newt` and `newt-agent` (dev- and build-only crates are not
distributed, so they are left out), reads each dependency's own licence files
for copyright notices, and deduplicates the licence bodies per SPDX id. Asset
attributions are hand-maintained in `xtask/src/notices_assets.md`. CI runs
`cargo xtask notices --check`, so the committed file cannot drift from the
dependency tree.

Linux packages install `LICENSE` and the notices under
`/usr/share/doc/newt/`; the `.deb` additionally carries a Debian
machine-readable `copyright` file, and the Arch package mirrors both into
`/usr/share/licenses/newt-fm/`.

### Copy Pane (Mod+.)

Sets the other pane's directory to match the active pane's current path. Useful for quickly aligning both panes to the same location before a copy/move.

### Follow Symlink (Shift+Enter)

When a symlink is focused, navigates to the symlink's resolved target path. Handles both relative and absolute symlink targets.

### Open Folder / Reveal (Shift+F3)

Opens the focused file's parent directory (or the focused directory itself) in the system's default file manager (Nautilus, Dolphin, Finder, etc.).

### Navigate Dialog (Mod+L)

Text input for jumping directly to any path. Pre-filled with the current path (auto-selected for easy replacement). Supports:
- Absolute paths (`/home/user/documents`).
- Relative paths (`../sibling`).
- **Root-relative paths** (`\`, `\Users\Tibor`, `/etc`) — resolved against the root of *where the pane already is*, which on a split-root (Windows) filesystem is the current drive or share root, matching Windows' own drive-relative semantics. Typing `\` on `C:\Users\Tibor` therefore lands on `C:\`, not on the unlistable `\\?\` position the abstract VFS root corresponds to. Implemented as `VfsDescriptor::root_of`, which walks up through `navigable_parent` so it lands exactly where holding `..` down does. Applies to `newt cd \` from the shell integration too, which shares `Pane::resolve_relative`.

  Both the separator set and the root-relative marker come from the **session's** `PathStyle` (`PathStyle::separators`), never the host's. `\` separates only on a Windows-styled filesystem; on a Unix one it is an ordinary filename character, so navigating to `\` enters a directory *called* `\` rather than jumping to the root — and `a\b` is one segment, not two. So `\` works as a drive-root jump in a Windows remote session opened from a macOS host, and stays a literal name in a Unix session opened from Windows. The choice is made once from the VFS the input was typed against: tokenizing is lexical, so a `..` that later escapes into a differently-styled origin VFS doesn't retroactively change what the string meant.
- Shell expansion (`~`, `$HOME/documents`).
- VFS display paths (`s3://bucket/path`, `sftp://host/path`).

Path resolution priority: First checks if any mounted VFS claims the path (e.g., `s3://` prefix), then falls back to shell expansion for local paths.

In a remote session with the client-local Windows FS exposed, that mount claims distinctively Windows-shaped absolute input (`C:\…`, bare `X:`, `\\server\share`, verbatim `\\?\` forms) via `RemoteVfsDescriptor::try_parse_display_path` (gated on the mount's `PathStyle`, parsed by `local_path_from_typed_display`) — so typed Windows paths and Shift+\<drive letter\> land on the client-local mount instead of dying against the agent's Unix shell. Drive-relative (`C:foo`) and `..`-carrying inputs are deliberately not claimed. Unix-style absolutes keep resolving through agent-side shell expansion (so `$VAR` expansion works there; `%WINDIR%` in remote sessions does not — accepted tradeoff). Without a Windows-styled mount, Windows-shaped exact input is a harmless near-no-op: the host-native ROOT decode in `cmd::pane::navigate` only applies when the session root's `PathStyle` matches the host's (local and elevated sessions), so a stray `C:\` can't decode against a Unix agent's `/`.

### Navigation History

Each pane maintains its own navigation history. Each entry stores the path, the focused filename, the formatted display path (preserved so unmounted-VFS entries still render meaningfully), the original arrival timestamp (preserved across re-visits — back/forward into an old entry doesn't bump it), and a snapshot of the enrichment overlay as the view was left (restored stale-while-revalidate on history navigation — see Enrichers).

**Single-step navigation:**
- **Back** (Mouse XButton1, command palette): Return to the previous directory.
- **Forward** (Mouse XButton2, command palette): Re-visit a directory you backed out of.

**History dialog** (Alt+Left / Alt+Right, Mod+Y):

A single dialog showing the pane's full back/forward timeline. Forward (redo) entries appear above the current entry, back (undo) entries below; closest entries are nearest current in the list.

- **Alt+Left / Alt+Right** open the dialog alt-tab style: pre-stepped one entry in the requested direction, with **Alt-up committing** the previewed entry. Tap-and-release is therefore equivalent to single-step back/forward; hold-and-step lets the user scan further before releasing.
- **Mod+Y** opens the same dialog persistent: Alt-up does nothing, the dialog stays until dismissed (Esc / outside-click). Each non-current entry has an inline "×" button that removes that entry from the pane's history (the list updates in place — the user can keep deleting). Useful for grooming a long history or evicting an entry that's anchoring an archive mount the user wants to drop.
- **In both modes**: Alt+Left/Right or ArrowDown/Up moves the preview, skipping unreachable entries (e.g. unmounted VFS mounts). Mouse hover updates the preview. **Enter** or mouse click commits.
- The current entry is shown in bold with a "current" badge. Unmounted-VFS entries are dimmed with an "unmounted" tag and cannot be navigated to, but remain visible for context.
- Entries are grouped by time bucket (just now / 5m / 15m / 30m / 1h / 2h / 6h / earlier today / yesterday / weekday / last week / N weeks / older) with quiet section dividers between buckets. Buckets are computed at dialog open and don't tick while it's open.

**Retention**: Each pane's history is bounded by the `behavior.history_retention` preference (default 200, set to 0 for unlimited). When the cap is reached, the oldest entries roll out as new ones are pushed.

**Archive mount lifetime**: Archive VFS mounts are kept alive as long as either pane can navigate to a path inside them via back/forward history (not just when the current path is inside the mount). Stepping out of an archive no longer eagerly unmounts it, so back-navigation re-enters it cleanly. Mounts only become unreachable — and are then auto-unmounted — when every history entry referencing them has rolled out, been manually deleted, or had its forward branch truncated by a divergent navigation.

**Robustness**: History stack mutation happens at the moment the displayed path actually changes (the first batch arrives during streaming, or the final swap if no streaming), not at the start of navigation. A back-press to an unreachable target — unmounted VFS, deleted directory, permission revoked — that errors before any batch lands leaves the history stack untouched, so the user can simply press Back again. Stacks are also restored if a multi-step history jump fails to land.

### Open in Left/Right Pane (Mod+Left / Mod+Right)

Opens the directory under the cursor in the left or right pane respectively, regardless of which pane is currently active. Useful for quickly setting up both panes for a copy/move operation.

### Hidden Files (Mod+H)

Toggle visibility of files starting with `.` (dot files). The `..` parent directory is *always* visible regardless of this setting. The toggle is global (affects both panes).

---

## 17. Default Keyboard Shortcut Reference

### File Operations

| Shortcut | Action | Context |
|----------|--------|---------|
| F2 | Rename | Pane focused |
| F3 | View file | Pane focused |
| Shift+F3 | Open folder in system file manager | Pane focused |
| F4 | Edit file | Pane focused |
| Shift+F4 | Create and edit file | Pane focused |
| F5 | Copy to other pane | Pane focused |
| Alt+F5 | Pack to archive | Pane focused |
| F6 | Move to other pane | Pane focused |
| F7 | Create directory | Pane focused |
| F8 | Delete selected (to Trash by default) | Pane focused |
| Delete | Delete selected (alternative; works in quick-search too, but edits text in the regex filter box) | Pane focused |
| Shift+Delete | Delete selected permanently | Pane focused |
| Cmd+Backspace | Delete selected (macOS alternative) | Pane focused |
| Opt+Cmd+Backspace | Delete selected permanently (macOS alternative) | Pane focused |
| Alt+Enter | Properties | Pane focused |
| Mod+Shift+Enter | Calculate size of selected/focused directories | Pane focused |
| Shift+Alt+Enter | Calculate all sizes in the current directory | Pane focused |

### Navigation

| Shortcut | Action | Context |
|----------|--------|---------|
| Enter | Open / enter directory | Pane focused |
| Backspace | Parent directory | Pane focused |
| Ctrl+Backspace | Filesystem root — the current drive or share root where there are several (Windows). Unbound on macOS, where ⌘⌫ is Move to Trash; bind `navigate_root` from the Keybindings tab | Pane focused |
| Tab | Switch panes | Pane focused |
| Shift+Enter | Follow symlink | Pane focused |
| Mod+L | Navigate (Go To...) | Pane focused |
| Alt+Left | History overlay (back direction) — tap for single back step, hold + step + release to commit | Pane focused |
| Alt+Right | History overlay (forward direction) — tap for single forward step, hold + step + release to commit | Pane focused |
| Mod+Left | Open in left pane | Pane focused |
| Mod+Right | Open in right pane | Pane focused |
| Mod+. | Copy pane path to other pane | Pane focused |
| Mod+P | Hot paths | Any |
| Mod+B | Add bookmark | Pane focused |
| Mod+Shift+L | Select VFS | Pane focused |

### Selection

| Shortcut | Action | Context |
|----------|--------|---------|
| Insert | Toggle select + advance focus | Pane focused |
| Mod+A | Select all | Pane focused |
| Mod+D | Deselect all | Pane focused |

### Clipboard

| Shortcut | Action | Context |
|----------|--------|---------|
| Mod+C | Copy path to clipboard | Pane focused |
| Mod+V | Paste from clipboard | Pane focused |

### Filter & Search

| Shortcut | Action | Context |
|----------|--------|---------|
| / | Enter filter mode (`start_filter`, rebindable) | Pane focused |
| (any printable char) | Start quick search | Pane focused |
| Escape | Cancel / clear filter | Pane focused |
| Mod+F | Find in Folder (recursive search) | Pane focused |

### Terminal

| Shortcut | Action | Context |
|----------|--------|---------|
| Ctrl+\` | Toggle terminal panel | Any |
| Ctrl+Shift+~ | New terminal | Any |
| Ctrl+PageDown | Next terminal | Any |
| Ctrl+PageUp | Previous terminal | Any |
| Alt+Up | Focus file panes | Any |
| Alt+Down | Focus terminal | Any |
| Mod+Enter | Open in terminal | Pane focused |

### View & Settings

| Shortcut | Action | Context |
|----------|--------|---------|
| Mod+H | Toggle hidden files | Any |
| Mod+, | Settings | Any |
| Mod+Shift+P (or F1) | Command palette | Any |
| F9 | User commands palette | Pane focused |
| F10 | Show Next Operation (cycle foreground op) | Any |
| Shift+F10 / Menu | Context menu | Pane focused |

### Window

| Shortcut | Action | Context |
|----------|--------|---------|
| Mod+N | New window | Any |
| Mod+Shift+W | Close window | Any |
| Mod+Q | Quit (close all windows) | Any |
| Mod+Shift+R | Connect remote | Any |
| Ctrl+R | Quick Connect | Pane focused |
| Mod+= (or Mod++) | Zoom in | Any |
| Mod+- | Zoom out | Any |
| Mod+0 | Reset zoom | Any |

Note: Refresh (Mod+R) is unbound by default to avoid conflict with Quick Connect (Ctrl+R). Rebind via settings if needed.

### Viewer and Editor Commands

Viewer (F3) and editor (F4) window shortcuts live in the same central keybinding registry as main-window commands: they appear in the Keybindings tab (When column "Viewer"/"Editor"), are rebindable there or via `[[bind]]` with `when = "viewer"` / `when = "editor"`, and their toolbar tooltips and context-menu hints reflect the resolved binding. They are namespaced "Viewer: …"/"Editor: …", excluded from the command palette (they dispatch inside their own windows, not through `cmd_*`), and a command only fires in modes that implement it. Deliberately **not** rebindable (fundamental keys): Escape to close the viewer, arrow/PgUp/PgDn panning and scrolling, and quick-search-style intrinsic navigation.

| Command | Default | Modes |
|---------|---------|-------|
| `viewer_toggle_hex` | F3 | All |
| `viewer_copy` | Mod+C | Text, Hex, Image |
| `viewer_select_all` | Mod+A | Text, Hex, Image |
| `viewer_find` | Mod+F | Text, Hex |
| `viewer_goto` | Mod+G | Text, Hex |
| `viewer_zoom_in` | = (also + / Shift+=) | Image, PDF |
| `viewer_zoom_out` | - (also Shift+-) | Image, PDF |
| `viewer_zoom_fit` | 0 | Image, PDF |
| `viewer_zoom_actual` | 1 | Image |
| `viewer_rotate_cw` / `viewer_rotate_ccw` | r / Shift+R | Image |
| `viewer_flip_horizontal` / `viewer_flip_vertical` | h / v | Image |
| `viewer_cycle_background` | b | Image |
| `viewer_toggle_info` | i | Image |
| `editor_save` | Mod+S | Editor |

All keybindings are fully customizable via the Settings dialog or `settings.toml`. `Mod` = Ctrl on Linux/Windows, Cmd on macOS.
