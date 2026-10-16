# Newt Feature Dump

Exhaustive inventory of every user-facing feature in Newt, a keyboard-centric dual-pane file manager built with Tauri 2.

---

## 1. Application Layout

The main window is divided into two zones by a resizable vertical splitter:

- **Top zone**: Two file panes side by side (horizontal splitter), each showing a directory listing.
- **Bottom zone**: Terminal panel (collapsible) with a tab bar for multiple terminals.

Both splitters are user-resizable by dragging the divider. Clicking splitter dividers does not steal focus from the file list or terminal.

### Maximized Layout

**Toggle Maximized Layout** (Mod+F11, palette category View) replaces the split with whatever has focus: the active pane alone, or the terminal panel alone. Nothing else about navigation changes — the layout just follows focus. Tab still switches the active pane, which swaps the pane on screen; moving focus to the terminal (Alt+Down, Ctrl+\`, a new terminal, Send to Terminal) shows the terminal full-window, and moving it back (Alt+Up, hiding the terminal panel, the last terminal closing) shows the active pane again. For the mouse, the pane header and the terminal tab bar each carry a maximize/restore button; a pane's button first makes that pane active, so it is the one that fills the window. A maximized pane's header also starts with a pane-switch button — a two-pane glyph with the shown side filled — that switches to the other pane, as Tab does. Splitter positions are kept while maximized and come back on restore. The state is per window and not persisted — new windows open split.

Additional overlay elements:
- Operations panel (shows background file operations and progress).
- Askpass dialog (SSH/sudo password prompts, overlays the main window).
- Connection status indicator (during connect/disconnect/reconnect).
- Modal dialogs (all driven by Rust state — never by React `useState`).

### Accessibility

Newt's windows are web content in the platform webview, so screen readers see them through the webview's own accessibility bridge — VoiceOver via WKWebView on macOS, Narrator/NVDA/JAWS via WebView2 (UI Automation) on Windows, Orca via WebKitGTK (AT-SPI) on Linux. Native menus and file dialogs are native controls.

- **File lists** are listboxes named by the pane's path. As the cursor moves, the focused row is announced (by its visible columns — name, size, dates, …; file-type glyphs are silent), with marked files reported as selected and the row's position in the whole listing, even though only a window of rows is rendered. Quick search and the filter box drive the list the same way, so typing announces the row it lands on.
- **Viewer table mode** is a grid: the cursor cell is announced with its row and column, selected cells are reported as selected, and the header row and row numbers act as headers.
- **Dialogs** are named by their title and described by their summary or, for confirmations, by what will happen; form fields are labelled, with hints and validation errors attached to their field. Lists that are navigated from a search box (Quick Connect, Hot Paths, the command palette, history, sort menu) announce the highlighted entry.
- **High contrast** (Windows `forced-colors`): states that are normally drawn with colour alone are redrawn in the theme's own colours — marked files, highlighted menu and palette entries, the active terminal tab, settings category and viewer toggles use the theme's selection colours, the file-list and table cursor is an outline (so it shows on a marked row too), and progress bars keep a visible fill and track.
- **Tooltips**: every icon-only or abbreviated control carries a native tooltip, and one that runs a command shows its current shortcut (e.g. "Maximize pane (⌘+F11)" on macOS), following any rebinding. Truncated paths and names show in full on hover.
- **Names can't disguise themselves**: bidirectional formatting characters in a name (right-to-left override, embeddings, isolates, the LRM/RLM/ALM marks) show as `�`, one each, wherever a name is something to decide on — the file list, the path bar, Delete, Copy/Move, Properties and the prompts of running operations — so `photo<RLO>gpj.exe` reads `photo�gpj.exe`, never `photoexe.jpg`. Right-to-left names themselves display normally, and each name in the file list is laid out on its own, never reordering the text beside it.

The terminal and editor rely on xterm.js's and Monaco's own accessibility; neither yet switches to its screen-reader mode automatically.

### Dialog system

All dialogs share a common visual language and a set of frontend primitives (`src/main_window/modals/primitives/`):

- **DialogShell / DialogHeader / DialogBody / DialogFooter**: structural skeleton. The body scrolls while the header and footer stay pinned (matters for tall content like Properties and the Connection Log). The footer is a chrome-tinted strip with a hairline top border; it hosts the right-aligned Cancel/primary buttons plus an optional left `start` slot for secondary controls (e.g. Copy's "Pack into archive…", Properties' "Apply recursively").
- **Field / FieldGroup / CheckboxField / FieldRow**: form layout primitives (stacked label+control with optional hint, tight checkbox clusters, inline label+control rows).
- **DialogTabs**: shared tab strip (Connect transports, archive formats, Settings sections). With a tab focused, Left/Right move to the previous/next tab and select it (wrapping), Home/End to the first/last.
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
- **Close Window** (Mod+Shift+W): Closes the current window. Shifted (gnome-terminal convention) so a session window can't be closed on a stray Mod+W. Viewer/editor children can't outlive the session they were spawned from, so closing a main window takes them along: a Rust-side `CloseRequested` interceptor (covers the command, the menu item, and the OS close button alike) closes viewer children outright and sweeps editor children — dirty ones prompt, the window close resumes once the last child editor is gone, and refusing a prompt aborts the close. A window with terminals open or operations still running (copies, moves, deletes, archive work, user commands) asks first, saying how many of each the close would end; a hidden terminal panel is shown (and a maximized pane restored) so the terminals are in view behind the prompt. **Close** — **Stop and Close** when operations are running — goes ahead: running operations are cancelled and the window closes once they've stopped (the dialog reads "Stopping the running operations…" meanwhile, with **Close Now** to stop waiting); Cancel keeps the window. Every open terminal counts, idle at a prompt or not. **Behavior ▸ Confirm Close** (`behavior.confirm_close`, on by default) turns the prompt off. Quitting — the Quit command, the Dock, logging out — asks the same of every window, and an editor with unsaved changes asks too, so with terminals open a logout waits for an answer. Closing ends the window's session: its terminals are killed, anything still running is cancelled, and a remote or elevated session's agent exits with its connection.
- **Quit Newt** (Mod+Q): Closes all main windows, exiting the app. Open editor windows are swept first so their unsaved-changes prompts fire: the quit waits (event-driven, via the `Destroyed` handler) for the last editor to close before the main windows go, and refusing any editor's prompt aborts the quit (`cancel_quit`), leaving all remaining windows open. Pre-warmed hidden editors are exempt from the sweep. Windows with operations running ask as they would on Close Window; the others close. OS-initiated termination (macOS Dock quit, logout) goes through the same sweep via an `applicationShouldTerminate:` method injected into tao's app delegate at runtime (`terminate_guard` in `main.rs` — tao doesn't implement the selector and `RunEvent::ExitRequested` never fires for OS termination, see tauri#9198): a dirty editor or a running operation cancels the termination and runs the sweep (with its close confirmations), an idle app terminates immediately so it never blocks logout. Editor dirty state is mirrored to Rust for this via `set_editor_dirty`. The `ExitRequested { code: None }` handler remains as a safety net for the all-windows-closed path.
- **Reload Window**: Available in the Debug dialog (debug builds only).
- **Window title**: the active pane's folder followed by the session's title — `walker - Newt`, `logs - Newt [build-01]`, `src - Newt [Elevated]` — so windows can be told apart in the window switcher. The folder is the last segment of the path bar (a search pane shows its search label); on Wayland the title doesn't update after the window opens (upstream GTK/Tauri bug).
- **Refresh File List** (unbound by default — `mod+r` would be Quick Connect's Ctrl+R on Windows/Linux; palette, pane context menu): Force-refreshes the active pane's directory listing.

Multiple main windows coexist in the same process: "New Window", remote connections, and elevated sessions all create additional windows within the running app. Each window has its own independent session, panes, terminals, and operations; closing the last main window exits the app.

### Reopening Where You Left Off

A session reopens where the last session to the **same target** left off — its panes' directories (with the cursor on the entry it was on) and active pane, and its window's size, position and maximized state. The target is what the session is connected to: local, elevated, a WSL distribution, or a remote target keyed like Quick Connect's recents (`ssh:host`, `docker:user@container`, …), so a saved profile, an ad-hoc connect and `--target` for the same host share one memory. Behaviour flags such as agent forwarding and credentials aren't part of the key.

- **Last closed wins.** A session is remembered when its window closes — the close button, Close Window, and Quit alike. Quit closes windows least-recently-focused first, so the window used last is what each target remembers. A macOS Dock quit or logout records every open session before the process ends; a Windows or Linux logoff does not, so whatever changed since each window last closed is lost there. A session that never connected leaves the remembered panes alone (a failed SSH attempt doesn't erase a good memory), though its window geometry still counts.
- **Only the session's own filesystem is restored.** A pane inside an archive or search results reopens in the directory the archive or search sits in. A pane on S3, SFTP, an agent mount, or (in a remote session) the Local filesystem reopens at the latest history entry that was on the session's own filesystem, else at home — reopening those mounts would mean credential prompts at launch. Navigation history, terminals, mounts, sorting and filters are not restored.
- **Missing paths**: the first listing of a session is non-strict, like any refresh — a directory that no longer exists lands on its nearest existing parent, with the cursor on the entry leading back toward it (the remembered cursor applies only when the directory itself is still there). A remembered directory that exists but can't be listed falls back to home rather than failing the connect.
- **Defaults**: a pane with nothing to restore opens at the home directory of the session's user — locally too, regardless of the directory Newt was launched from. Restoring is controlled by `behavior.restore_locations`: `all` sessions (default), only sessions on this machine (`local`: local, elevated, WSL), or `none`. When a session to an excluded target closes, whatever was remembered about its panes is dropped.
- **Command-line paths win**: `newt ~/src ~/Downloads` opens the left pane at the first path and the right at the second; with one path both panes open there. A path to a file opens its directory with the file focused. Relative paths are resolved against the launching shell's directory for local and elevated sessions; a leading `~` is expanded on the session's side, so `newt --target ssh:host '~/logs'` means the remote home. `--cwd-left` / `--cwd-right` remain as the long form. Bare `newt` restores — the VS Code convention, where `code` reopens and `code .` opens here, so `newt .` opens the shell's directory. Inside a Newt terminal, `newt <verb>` is the shell-integration CLI, so a directory named like a verb needs `./`. The Linux desktop entry passes paths (`%F`), so "Open With Newt" on a folder opens it.
- **New Window** (`behavior.new_window_location`): `inherit` (default) opens with the panes, active pane and window size of the local window it was opened from, offset from it; `restore` opens where the last closed local window was; `default` opens at home. New Window from a remote window opens a local session, so there's nothing to inherit and it restores instead.
- **Reconnect** returns the panes to where they were before the connection dropped, whatever `restore_locations` says.
- **Window geometry** (`behavior.restore_window_geometry`, default on): main windows are remembered per target; a target seen for the first time opens at the size (and maximized state) of the last window closed, placed by the OS. A remembered position whose title bar would be off every attached display (a monitor since unplugged) is dropped, and a window that would land exactly on a live one is offset from it. A maximized window keeps its un-maximized size for when it's restored; a fullscreen window comes back as a normal window at its pre-fullscreen bounds. Viewer and editor windows remember one size each, centred on screen when opened; a maximized or fullscreen one isn't recorded.
- Up to 32 targets are remembered, most recently closed first. Forgetting a recent in Quick Connect forgets its session memory too.

The memory lives in `sessions.json` next to `settings.toml` — machine-written JSON, separate from `state.json` because none of it is needed by the UI, and it's re-read on every use and written atomically, so several Newt processes sharing a config directory don't overwrite each other's entries.

### Main Window Menu Bar (macOS only)

Main windows carry a minimal native menu on macOS (Windows/Linux main windows have no menu bar). Menu items dispatch window-scoped `menu-command` events to the frontend, which routes them through the same `executeCommandById` path as keybindings, and item accelerators are derived from the *resolved* keybindings at build time — a task subscribed to the preferences watcher rebuilds all main-window menus on preference changes, so rebinds are reflected live.

- **App menu**: About Newt, Settings… (Mod+,), Quit Newt (Mod+Q). Deliberately no Hide/Hide Others — their fixed Cmd+H would shadow Toggle Hidden Files.
- **File**: New Window, Connect to Remote Host…, Close Window.
- **Edit**: predefined Cut/Copy/Paste/Select All (required so macOS routes Cmd+C/V/X/A to the webview — previously the sole content of the main-window menu).
- **Window**: predefined Minimize, Zoom.
- **Help**: Newt Documentation (`documentation`).

Viewer and editor windows prepend an app submenu with Quit Newt (Cmd+Q) on macOS, so quit works whichever window is focused.

---

## 2. Dual-Pane File Browser

Each pane is an independent file browser with its own path, selection, filter, sort order, and navigation history.

### Pane Header

- **Focus dot**: A small accent dot at the start of the header of the pane that has focus. The other pane keeps the space empty, so both paths start at the same offset.
- **VFS selector dropdown**: Shows the current filesystem type (Local, S3, SFTP, Archive name). Click to open a dropdown listing all mounted VFSes and available mount options. Mounted VFSes sort first and show an unmount (×) button; below a separator, unmounted types (S3, SFTP, Remote) appear as ellipsis entries ("S3…") that open the respective mount dialog. "Remote…" opens the full Connect dialog with "Open as a new session" defaulted off (pane mount) regardless of session mode.
- **Breadcrumb path**: Current directory path displayed as clickable breadcrumb segments. Clicking any segment navigates to that directory. Clicking the *last* segment opens the Navigate (Go To) dialog instead of navigating. Breadcrumb format varies by VFS type:
  - Local: `/home/user/documents`
  - S3: `s3://bucket/prefix/key`
  - SFTP: `sftp://hostname/path`
  - Archives: origin path + inner path, e.g., `/home/user/file.tar.gz/dir/subdir`
- **Git branch badge**: When the pane's directory is inside a git repository, a quiet muted-text badge appears between the breadcrumbs and the free-space indicator: seti git glyph + branch name (short commit id when detached), a `*` suffix when the repo has uncommitted changes, and `↑N`/`↓M` ahead/behind counts when an upstream exists. Produced by the git enricher (see "Enrichers" below).
- **Free space indicator**: Shows available disk space (e.g., "123.4 GB free") when the filesystem reports stats. Link-styled; clicking opens the volume-root Properties dialog (`root_properties`), whose Volume section carries the full detail (volume classification — local disk / removable / optical / network / RAM disk / subst — filesystem name, label, target, capacity/used/free). The dialog targets the volume the stats actually describe (`VolumeInfo.mount_point` — on Unix the containing mount point, e.g. `/proc`, not `/`; on Windows the drive/share root). Volume info is probed on the FS-owning side per listing (`VolumeInfo` on `FsStats`): Win32 volume APIs on Windows, `/proc/self/mountinfo` + `/dev/disk/by-label` on Linux, `statfs` on macOS. Only visible for VFS types that support `fs_stats` (local filesystem). Directory Properties invoked *at* a volume root shows the same Volume section.

- **Maximize button**: A small icon button at the right end of the header, after the free-space indicator. Maximizes this pane (making it the active one) or, when maximized, restores the split layout — see "Maximized Layout". Clicking it leaves keyboard focus in the file list.

The whole header can be hidden via the `appearance.show_pane_header` preference (default: on). When hidden, the VFS selector trigger stays mounted in an off-screen anchor so its keyboard shortcut still opens the dropdown anchored to the top of the pane.

### Pane Status Bar

Bottom strip on each pane showing loading state, the current file's full display path, file/directory counts, total size of selection or directory, and a "(partial)" marker when the listing is windowed. Toggled by the `appearance.show_pane_status` preference (default: on).

### File List

Server-side windowed list with a fixed row height. Rust sends only a ~150-item window around the current viewport; the frontend renders all window items directly with simple spacer divs. Enables smooth performance with directories of 100k+ files. The mouse wheel and trackpad scroll from script and stop at the edge of the rows Rust has sent, going on as the window slides, so a fast flick never shows blank rows, and the list stops when the wheel does; dragging the scrollbar can still outrun the window. When the listing changes in place — a search streaming in results, a refresh, a re-sort — a cursor that is on screen keeps its screen row while entries sort in around it, and a view scrolled away from the cursor stays where it is rather than jumping back to it.

Each pane keeps its cursor on screen: a solid accent bar in the pane that has focus, and neutral grey in the other one — and in both while the terminal has focus — so the cursor a Tab or a click on the other pane would land on is always visible. Marked files get a light accent tint, joined into one block across a run of them, with the name in the accent colour. While the window itself is in the background (another app, another Newt window, or a native dialog has the keyboard), the focus marks all turn grey — the cursor like the other pane's, the header's focus dot, and the active terminal tab's line — and come back when the window does.

Row height comes from the `appearance.density` preference: `compact` (20px), `comfortable` (22px, default) or `spacious` (24px). It reaches the styles as the `--row-height` token, keyed off a `data-density` attribute on the document element; the row icon box, the column header and the resize grip follow it, so nothing else carries a per-density value. Icon artwork — seti glyph, folder image, symlink badge — is a fixed size regardless of density. The pane's virtualization arithmetic (spacer heights, viewport reports, PageUp/PageDown, drag-rectangle → index) needs the number rather than a computed style, so `src/main_window/density.ts` restates the same pixel values — the two must stay in step. Rust is density-agnostic: viewport hints cross the boundary as row indices and counts.

**Default columns** (all sortable by clicking the header):

| Column | Width | Alignment | Content |
|--------|-------|-----------|---------|
| Name | 250px | Left | File type icon (color-coded, VSCode icon set) + filename |
| Size | 100px | Right | Locale-formatted byte count — or SI-prefixed ("1.5 GB") when `appearance.si_size_prefixes` is on (default off) — "—" for directories without a computed size, "???" if unknown |
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

Column widths are resizable by dragging the grip between column headers (minimum 10px during the drag). Widths persist per pane and per column key in the runtime-state file (`state.json`, see Configuration) — not in `settings.toml` — and are restored on window reload / new windows; a zero-movement click on the grip writes nothing. Double-clicking a grip auto-sizes its column Excel-style: the width fits the widest currently rendered cell (the list is virtualized, so this is the visible window plus overscan, not the whole directory) with the header's own labels as a floor, clamped to ≥30px, then persists. Holding **Shift** applies either gesture to both panes: a Shift-drag resizes the column in the other pane along with it (pressing or releasing Shift mid-drag switches live; the other pane keeps its width unless Shift is still held when the mouse is released), and a Shift-double-click fits the column to the wider of the two panes' contents. A pane that doesn't show the column (its filesystem lacks that metadata) contributes nothing to the fit but still takes the width for when it does. Widths are applied via per-pane CSS custom properties; runtime-state updates broadcast app-wide, so the same pane slot in other windows follows (last write wins).

Click a column header to sort ascending by that key; click the same header again to toggle descending. A triangle indicator (▲/▼) shows the active sort column and direction.

**Sort menu (keyboard, Mod+Shift+S)**: opens a compact menu anchored under the pane header listing every sort key, the current one marked with its ▲/▼. Each row has an underlined accelerator letter (and 1–9 by position); pressing it sorts by that key and closes the menu — a poor-man's chord, so `Mod+Shift+S` then `m` sorts by Modified. Pressing the same key again reverses direction (same toggle as a header click), so `Mod+Shift+S m m` flips to descending. **Holding Shift arms reverse**: `Shift+<key>` sorts by that key *descending* outright (`Mod+Shift+S`, then Shift+M → Modified descending), and while Shift is held the Reverse row lights up (with a `Shift` hint badge) to show it's in effect. Arrow keys + Enter also work; the highlight seeds on the current key so a bare Enter reverses it. `R` reverses without changing the key; `F` toggles "Folders first" (mutating the `appearance.folders_first` preference, like column reordering mutates prefs). Esc dismisses; focus returns to the pane. The menu is per-pane and keyboard-launched only (no click target). The command (`sort`) is rebindable and appears once in the command palette.

**Available sort keys**: Name, Extension, Size, User, Group, Mode, Modified, Accessed, Created.

**Sort behavior**:
- The `..` parent directory entry *always* sorts first, regardless of sort key or direction.
- When "Folders first" is enabled (preference), directories sort before files (but after `..`).
- Extension sort treats directories as having no extension.
- Name and extension sorting is **case-insensitive** (Unicode `to_lowercase`), with a stable byte-order tiebreaker for entries differing only by case.
- **Natural sort** (`appearance.natural_sort`, off by default): Name sorting compares runs of digits by their value, so `IMG_2` comes before `IMG_10` and `v1.9` before `v1.10`, as Finder and Explorer order them. Off, names sort character by character, as `ls` does (`IMG_10` before `IMG_2`). Names that differ only in leading zeros or case keep the character order between them.

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
| Loading (after 200ms) | spinner, "Loading... (X items so far)" |
| Loaded, no selection | "X files, Y directories", plus ", N hidden" when hidden files exist but Show Hidden is off |
| Loaded, with selection | "X files, Y directories selected, Z bytes total" — Z includes computed recursive sizes of selected directories, so ⌘A after Calculate All Sizes totals the whole directory |
| Filter active | "(showing X of Y)" appended |
| Partial results | "(partial)" appended when the listing is incomplete — the reason on hover: "listing interrupted" when navigation moved on mid-stream, "cancelled" for a search cut short, "archive truncated" for an archive that ends inside a member, "N entries could not be read" or "cannot list …" for a search that hit unreadable directories |

### Directory Loading

Large directories are loaded incrementally via streaming:
- First batch clears old state (filter, selection, focus).
- Intermediate batches update the visible file list and statistics in real time — the user can browse and interact while loading continues.
- Navigation to a new directory auto-cancels any in-progress load for the same pane.
- Until the new directory's first rows arrive, the pane keeps showing the old ones, muted (the list at reduced opacity, no overlay): the breadcrumbs already name the new location, and the status bar says "Loading file list…". Input waits for the listing.
- **Busy indicator**: whenever the status bar reports work under way — a listing loading or streaming, an enricher running (git status, Calculate Size), a search walking — a small spinner leads it. A 200ms grace period keeps it (and the text) away on fast work, so it doesn't flicker.

### Enrichers

Background annotation of directory listings (design: `design_docs/DESIGN_ENRICHERS_AND_RESOURCES.md`). An enricher computes extra per-entry information (`Annotation`s keyed by entry key) and per-location `ContextBadge`s, streamed into a pane-side overlay that is merged into rows at window projection time. The pane layer is fully generic — annotations are opaque to it (`FileView.annotations`); only the frontend interprets the kinds it knows how to render, and the preference↔enricher-id gating lives with the toggles in the preferences schema (`EnricherPreferences::disabled_enrichers`).

**Lifecycle — anchored to the history cursor**: automatic enrichers (git) start when a navigation lands and restart on every refresh (recompute-per-refresh replaces cache invalidation); annotations survive refreshes (re-keying onto surviving rows) but are cleared by any navigation. History navigation (Alt+arrows, history-dialog jumps) restores them stale-while-revalidate: each history entry captures the overlay as it stood when that view was left, the restore happens at landing before the automatic rerun is signalled, so git supersedes its part deterministically while computed du sizes simply reappear as they were (returning to a past view is an explicit acceptance of that view's snapshot). Fresh navigation to the same path does not resurrect anything; snapshot lifetime rides history retention. An automatic rerun supersedes the previous generation wholesale (first batch carries a `reset` flag), so entries that stopped matching (e.g. a committed file) don't linger. Manual enrichers (du) run on their own lane: triggered by keybinds, never restarted by refreshes, and their runs *accumulate* within a visit (sizing one directory doesn't clear a previously sized sibling). Esc on a loaded pane and navigating away both cancel the runs in flight reliably; already-applied annotations stay.

**In-flight visibility**: while an enricher runs, the pane status bar shows its activity label ("git status… (Esc cancels)"), with a 200ms appearance delay so fast runs don't flash it.

**Architecture**: symmetric across the host↔agent boundary on the operations template — an `Enrichers` registry (`newt_common::enrich`) lives next to the `VfsRegistry` (host-side in local sessions, agent-side in remote ones), fronted by an `EnricherClient` with Local/Remote impls. Static, inventory-collected `EnricherDescriptor`s (the `VfsDescriptor` analogue: id, activity label, automatic, `applies_to_vfs`) are linked by both sides, so the host selects which enrichers to run for a pane (descriptor gate against the pane's VFS × preference gate) and the request names them explicitly — an empty selection sends no request at all, so S3/search/archive/agent-mount panes cost nothing. Data-dependent applicability (is this a repo?) lives inside the enricher; a run that finds nothing still sends an empty reset batch, clearing stale annotations. The remote path streams events down an RPC stream; cancellation is by dropping the request future (transport-level `InvokeCancel` aborts the agent-side run). Producer-side batching at 100ms.

**Du enricher (recursive directory sizes)**: manual-only, two commands — **Calculate Size** (Mod+Shift+Enter; sizes the selected/focused directories) and **Calculate All Sizes** (Shift+Alt+Enter; sizes every entry). There is deliberately no directory-total badge — selection totals include computed sizes, so ⌘A reads the directory total off the existing status line. Walks via the `Vfs` trait on the side that owns the filesystem, so it works on any VFS (S3, archives, SFTP, remote sessions) and never crosses registry mount boundaries; up to 16 entry walks run concurrently, each a serial DFS. Running totals stream into the size column ncdu-style (in-progress values dimmed with a trailing `+`, flipping to plain when a subtree completes); Esc freezes whatever was computed as marked partials. Computed sizes participate in sort-by-size (a live walk re-sorts in place while the order is active). Directory symlinks are not followed, `/proc` is excluded, unreadable subtrees are skipped (all via the shared walker, see File Operations) — and leave the finished total a **lower bound**, rendered with the trailing `+` at full weight and the count of unreadable directories on hover; an entry whose own listing fails stays unannotated. Sizing matches `du`: allocated bytes (`File.allocated_size`, `st_blocks`-based) when the filesystem reports them — sparse files (VM disk images, Docker.raw) count what they occupy, not their apparent size — with apparent-size fallback on VFSes without block metadata (S3/SFTP/archives, where the two coincide). Hardlinked files are counted once per sized entry (`(device_id, inode)` dedup gated on `hard_links > 1`), and the walk never crosses filesystem boundaries (`du -x` semantics; a mountpoint entry reports the mounted filesystem's device, so sizing the mountpoint itself works and stops at further nested mounts). All four fields are unix-only `File` metadata (`None` on Windows and non-local VFSes).

**Git enricher**: shells out to the `git` binary where the files live — the remote host's git in remote sessions; no gitoxide/libgit2 dependency. A cheap `.git` walk-up (directory or file, covering worktrees) guards the spawn, so non-repo directories never pay for a git process (their run just emits the empty reset batch). One `git --no-optional-locks status --porcelain=v2 -z --branch --ignored=matching` run at the repo root yields branch/ahead-behind/dirty for the badge and repo-wide per-file statuses for row coloring, with directory rollups (precedence: conflicted > modified > renamed > added > untracked; ignored never rolls up). `--no-optional-locks` keeps status from writing `.git/index` and re-triggering the pane watcher. The status taxonomy is deliberately coarse: copied (`C`) renders as renamed, submodule status changes as modified, and deleted-only directories roll up as Modified. Toggled by `enrichers.git_status` (default on); toggling takes effect immediately (preference changes re-run automatic enrichers).

### Keyboard Navigation

| Key | Action |
|-----|--------|
| Arrow Up/Down | Move focus one item |
| Shift+Arrow Up/Down | Move focus and extend selection |
| Page Up/Down | Jump one viewport height |
| Shift+Page Up/Down | Jump one viewport height, selecting every row passed over (destination included) |
| Home | Jump to first item |
| End | Jump to last item |
| Shift+Home / Shift+End | Jump to first/last item, selecting the whole range |
| Enter | Open file or enter directory (see "Enter behavior" below) |
| Backspace | Navigate to parent directory (`..`) |
| Tab | Switch active pane |
| Insert | Toggle selection on current file and advance focus to next (the rebindable `toggle_selection` command; macOS reports a PC keyboard's Insert as Help, which matches it too. Apple keyboards have no Insert: rebind it, or select with Shift+arrows or Mod+Click) |
| Mod+A | Select all files (except `..`) |
| Mod+D | Deselect all |
| Num * | Invert Selection — toggles every visible entry; filtered-out selections are left alone |
| Num + / Num - | Select / Deselect by Pattern (see below) |
| (unbound) | Select Same Extension (command palette) — adds every visible entry with the focused entry's extension, case-folded; directories count as one extensionless group of their own |
| Escape | Clear the quick search or filter, and stop whatever the pane is still loading — a directory listing, git status, Calculate Size. The selection is kept (Mod+D deselects) |
| Shift+Enter | Follow symlink (navigate to its target) |
| Shift+\<drive letter\> | Jump to that drive's root. Offered whenever the session has a split-root (Windows drive-lettered) mount — `MainWindowState.mount_summary.has_split_root_vfs`, refreshed on mount/unmount — not gated on the host OS. With no such mount the keystroke types into the quick filter as usual. |

**Select by Pattern** (Num + / Num -, "Select by Pattern..." / "Deselect by Pattern..." in the palette): one dialog, opened in add or remove mode by the entry point, with the mode switchable inside it. The pattern is a case-insensitive glob (`*.rs`, `IMG_*.{jpg,png}`; `*` spans the whole name, there are no separators to stop at); a leading `/` makes it a case-insensitive regex (`/^foo.*\.c$`). It matches display names of *visible* entries only (an active visual filter narrows it, as with Mod+A), never `..`, and only ever adds or removes the matches — the rest of the selection stays. A live "N entries match" count updates as you type (`count_pattern_matches`, an ephemeral query rather than pushed state); an uncompilable pattern disables Submit. The last applied pattern is remembered per session and pre-filled (selected) on the next open.

Numpad-only defaults are deliberate: `+`/`-`/`*` on the main row are printable characters and start quick search. `normalizeKeyEvent` names the numpad operators by `KeyboardEvent.code` (`numpad_add`, `numpad_subtract`, `numpad_multiply`, `numpad_divide`) since `e.key` is identical for both rows, and the pane's printable-key heuristic skips those codes so they reach the dispatcher. Laptop users rebind (`shift+=` etc.) in the Keybindings tab.

**Enter behavior** depends on what's focused, and on its [file associations](#file-associations):
- **Directory**: Navigate into it — unless an association says otherwise: on macOS, with **Open Packages** on, app bundles and other packages open instead.
- **Archive file** (`.tar.gz`, `.zip`, etc.) or **disc image** (`.iso`, `.udf`): Mount as VFS and navigate into its root (an association with `enter = "browse"`).
- **Symlink to directory**: Enter it logically — the pane keeps the link path, listing goes through the link (both Unix and healthy Windows links, `mklink /D`/`/J`). On Windows-shaped FSes, if the logical entry fails, Enter falls back to the resolved link *target*: the app-compat junctions (`C:\Users\<user>\Cookies` & co.) carry an `Everyone:(DENY)(RD)` ACE and are unlistable by design, so resolving is the only useful move (TC resolves; Salamander errors). Deliberately shallow: direct navigation to a link path (Ctrl+L, breadcrumb) is unchanged and may error.
- **Regular file**: Open with system default application, unless an association picks Newt's viewer, its editor or a user command. For host-local files this opens directly via the OS opener; for files on a non-host-local VFS (S3, SFTP, archives, remote), the file is first downloaded to a temp directory on the host using the standard Copy operation, and the system handler is launched on completion.
- **`..`**: Navigate to parent directory.

### File Associations

What Enter does with a file or directory, what **Browse Into** browses a file as, the mode the viewer (F3, Quick View) opens it in and the language the editor (F4) gives it all come from associations: `[[association]]` entries in `settings.toml`, edited on the Settings dialog's **Associations** tab, followed by the built-in ones.

```toml
[[association]]
match = ["*.nupkg", "*.vsix"]   # name globs, case-insensitive; one string or a list
enter = "browse"                # "open" (default app), "browse", "view", "edit" or "command"
format = "zip"                  # "zip", "7z", "tar" (tar, cpio or ar), "compressed" or "disc"

[[association]]
match = "*.log"
viewer = "text"                 # text, hex, dissect, table, markdown, image, audio, video, pdf
language = "plaintext"          # a Monaco language id, as in the editor's Language menu

[[association]]
match = "build"
kind = "directory"              # "file" (default) or "directory"
enter = "command"
command = "Build"               # the title of a [[command]]
```

- **The most specific pattern wins**: of the entries whose patterns match a name, the one whose pattern has the most literal characters goes first — `*.tar.gz` before `*.gz`, `Dockerfile` before `*` — and at equal specificity the user's before the built-ins; order in the file only breaks remaining ties. **Each property then resolves on its own**, from the first of those entries that sets it: an entry setting only `viewer` leaves what Enter does to the next one. The command an `enter = "command"` runs comes from the entry that chose it. A viewer mode or language no entry sets comes from the file's MIME type, then Hex mode and plain text.
- **Built-ins** cover the archive and disc formats below, the editor's language table, `.ts`/`.tsx`/`.cts` as TypeScript text (MIME databases know `.ts` only as an MPEG transport stream; `.mts`, AVCHD video as often as a TypeScript module, keeps its video type), and containers that open in their own application but can be browsed into from the context menu: Office and OpenDocument files, EPUB, Python wheels, NuGet and VS Code packages (`zip`) and Rust crates (`tar`).
- **Enter actions**: `open` hands the file to the system's default application, `browse` goes in (into a directory, or into a file as `format`), `view` and `edit` open Newt's viewer and editor, `command` runs a user command as picking it from the palette would. A renamed command takes its associations along. A directory can only be opened on this computer's own filesystem; elsewhere Enter goes in.
- **Packages** (macOS only): app bundles, frameworks, plugins, Xcode projects, `.rtfd` documents, photo libraries and other directories that Finder shows as files. Which directories are packages is what Launch Services says (`NSURLIsPackageKey`: a type conforming to `com.apple.package`, as declared by the system and installed apps, or the bundle bit set) — never a guess from a name, so a `whatever.app` folder on Linux, in S3 or in an archive is a folder. A package shows a file's icon instead of the folder. Enter goes into them; **Behavior ▸ Open Packages** makes it open them instead. A `[[association]]` with `kind = "directory"` still decides what Enter does for what it matches, and one without `enter` declares what it matches a package (offering Open in Default App). The listing is shown at once and packages are marked as Launch Services answers, an automatic enricher like git status; it runs only where a macOS kernel serves the files (locally on a Mac, or in a remote session to one).
- **Browse Into** (`browse_into`, unbound; context menu) goes into the focused entry whatever Enter does with it: a directory or package is entered, a file browsed as its association's format — or, when nothing names one, as whatever its first bytes say (ZIP, 7z, tar, cpio and ar headers, gzip/bzip2/xz/zstd streams, ISO 9660 and UDF volumes). A tar or compressed file whose name doesn't say which compression it uses is decompressed by what its first bytes say.
- **Open in Default App** (`open`, unbound; context menu) always hands the entry to the system's default application, downloading it first when it isn't on this computer.
- **A downloaded file is a copy.** Opening a file that isn't on this computer (S3, SFTP, an archive, a remote session) — by Enter, Open in Default App or Open With — copies it into a new directory under the system's temporary directory and opens the copy. Changes saved in the application stay in that copy and are never written back to where the file came from; to change the file itself, edit it in Newt's editor (F4), which saves to its own location, or copy the edited copy back. Newt doesn't delete these copies (the system's temporary-directory cleanup does, eventually), and if the application can't be started once the copy is made, that is only logged.

### Open With

The file context menu's **Open With ▸** lists the applications the system offers for the file — from Launch Services on macOS, the shell's recommended handlers for the extension on Windows, the desktop's applications for its MIME type on Linux — with the default first and marked, and opens the file in the one picked. Its last item, **Other…**, is the **Open With…** command (`open_with`, unbound; palette), the system's own chooser:

- **Windows**: the shell's "How do you want to open this file?" dialog (`SHOpenWithDialog`), with "Always use this app"; the file opens in what is chosen.
- **Linux**: the desktop portal's app chooser (`org.freedesktop.portal.OpenURI.OpenFile` with `ask`) — GNOME's and KDE's own — which opens the file in what is chosen. Without `xdg-desktop-portal` running, Open With… reports that no chooser is available.
- **macOS**, where only Finder has a chooser: the same one Finder's "Open With ▸ Other…" shows — an open panel on Applications, as a sheet on the window, with *Enable: Recommended Applications / All Applications* (only the applications recommended for the file can be chosen until "All" is picked) and *Always Open With*, which makes the choice the default for files of its type.

A file that isn't on this computer (S3, SFTP, an archive, a remote session) is downloaded to a temporary directory first, as Open in Default App does, and opens once the copy finishes. Directories and packages aren't offered: Open With is for files.

### Mouse Interactions

| Action | Behavior |
|--------|----------|
| Left click | Focus the clicked file |
| Mod+Click (Ctrl; ⌘ on macOS) | Toggle selection for that file (keeps other selections) |
| Shift+Click | Range select from focused file to clicked file |
| Double-click | Open/enter (same as Enter key) |
| Right-click | If clicked file is NOT selected: focus it (clears selection), show context menu. If clicked file IS selected: keep selection, show context menu. |
| Drag outside the Name column | Rectangle (marquee) selection — see below |
| Drag on file icon/name | Initiate drag-and-drop to other pane — see below |
| Alt+Wheel (⌥ on macOS) | Move the cursor instead of scrolling, one row per wheel notch however fast the wheel turns (scroll acceleration is ignored). A very fast spin can move a little less than a row per notch: the webview merges some notches into one event |
| Shift+Alt+Wheel | Move the cursor and select from the row it started on, over the selection as it was: wheeling back unselects, as a marquee does. Lasts until Shift or Alt is released |

**Rectangle (marquee) selection**:
- Must drag at least 5px before the rectangle appears (prevents accidental activation on click).
- A blue selection rectangle is drawn; files overlapping the rectangle are selected.
- Auto-scrolls when dragging near the top or bottom edges of the pane (44px zone). Speed increases closer to the edge.
- **Mod+Drag** (Ctrl; ⌘ on macOS): Adds rectangle selection to existing selection.
- **Shift+Drag**: Selects range from focused file to drag endpoint.
- **Normal drag**: Replaces entire selection with rectangle contents.

**Pane activation**: Clicking anywhere in a pane makes it the active pane for keyboard commands.

### Context Menu

The default browser context menu is suppressed in the main window and in the text, hex and structure viewers (not the editor). Text inputs retain their native context menus. The keyboard's menu key (Shift+F10 too, where the webview sends it) opens a view's menu at its cursor — the focused row in a pane, the selected node in the structure tree, the cursor byte or selection in hex and text views — wherever the mouse pointer happens to be; macOS WebKit aims the key's request at the pointer, so Newt tells it from a right-click by the press that precedes a click and a key has none.

**Right-click a file** or press Shift+F10 / Menu key:

| Item | Shortcut |
|------|----------|
| Open | Enter |
| Open in Default App | (`open`, unbound; when Enter does something else, or on a macOS package) |
| Browse Into | (`browse_into`, unbound; when Enter doesn't go in but could — a package set to open, a document that is a ZIP inside) |
| Open With ▸ | Files only: the applications the system offers for the file, the default marked, then **Other…** (`open_with`, unbound) |
| Change Association… | (`change_association`, unbound) — the Settings dialog's Associations tab, at the entry's row |
| Follow Symlink / Reveal Source | Shift+Enter (symlinks and aliased entries only) |
| View | F3 |
| Edit | F4 |
| Cut to Clipboard / Copy to Clipboard | Mod+X / Mod+C |
| Copy Path | ⌥⌘C on macOS, Ctrl+Shift+C elsewhere |
| Copy / Move / Pack to Archive | F5 / F6 / Alt+F5 |
| Rename | F2 |
| Delete | F8 |
| Delete Permanently | Shift+Delete |
| Open in Terminal | Mod+Enter |
| Properties | Alt+Enter |
| Windows Menu | Shift+RClick (Windows host + host-local files only) |

The Follow row appears only when it would do something: labelled "Reveal Source" on an entry carrying a synthetic-VFS alias (a search hit), "Follow Symlink" on a symlink, absent otherwise — the same precedence `cmd_follow_symlink` applies. Like the rest of the menu it describes the entry the commands act on, which is the focused row rather than the clicked one when the right-click lands inside a multi-selection.

**Right-click empty space** in the file list:

| Item | Shortcut |
|------|----------|
| Reveal in File Manager | Shift+F3 (host-local VFS only) |
| New Directory | F7 |
| New File | |
| Paste | Mod+V |
| Move Here (macOS) | ⌥⌘V |
| Refresh | (unbound) |
| Directory Properties | (`directory_properties`, unbound) |
| Windows Menu | Shift+RClick (Windows host + host-local files only) |

**Windows shell context menu** (Windows host, host-local files): the trailing "Windows Menu" item — or Shift+right-click to skip our menu entirely — pops the classic `IContextMenu` shell menu (not the Windows 11 abbreviated one) for the effective selection; on empty space (or the `..` row) it targets the current directory itself. Implementation (`main_window/shell_menu.rs`, `shell_context_menu` command): runs synchronously on the main thread (`TrackPopupMenuEx` pumps its own modal loop), `SHParseDisplayName` on de-verbatimed paths (`launch_cwd`) → parent `IShellFolder::GetUIObjectOf` → `QueryContextMenu` → `TrackPopupMenuEx(TPM_RETURNCMD)` → `InvokeCommand`, with the Tauri window temporarily subclassed to forward `WM_INITMENUPOPUP`/`WM_DRAWITEM`/`WM_MEASUREITEM`/`WM_MENUCHAR` to `IContextMenu2/3` so dynamic submenus ("Open with", "Send to") populate. User-cancelled verbs (`ERROR_CANCELLED`) are not errors; shell-side mutations (delete, rename, …) reach the pane through the directory watcher. In **elevated sessions** the menu is still built and invoked by the non-elevated UI process — items the desktop user can't read (e.g. `C:\Windows\System32\config\*`) fail at `SHParseDisplayName` with an error, and any verb that did run would run non-elevated. Deliberate: an agent-side menu (feasible for the UAC transport, which shares the interactive desktop) would invoke *every* verb elevated — "Open"/"Open with" silently spawning admin-token processes — which is why Explorer refuses to run elevated too. Power users who need shell verbs on admin-only files can just run `newt.exe` itself elevated.

**Right-click the path bar** — a breadcrumb, or anywhere else in the bar:

| Item | Shortcut |
|------|----------|
| Copy Path | (the display path up to that breadcrumb, or the folder's own off the breadcrumbs) |
| Go To… | Mod+L |
| Hot Paths… | Mod+P |
| Add to Bookmarks | Mod+B |
| New Terminal Here | Ctrl+Shift+~ |
| Directory Properties… | |
| Refresh | |

All but Copy Path are the pane's ordinary commands, acting on the pane whose bar was clicked.

### Drag and Drop

- Drag one or more files from one pane to the other by dragging anywhere in the Name column — the icon, the name, or the blank space after it. Dragging in any other column or below the last row draws a selection rectangle instead.
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

- **Activation**: Start typing any printable character while the pane is focused — Shift included (capitals, digits on AZERTY), unless that Shift chord is bound to a command; not with Ctrl, Alt/Option or Cmd. Characters typed with AltGr or Option don't start it, since those modifiers are also how chords are typed; Filter mode (`/`) takes any character. On a pane listing Windows drives, Shift+letter goes to that drive instead. Requires `quick_search = true` (default).
- **Input methods**: keys an IME is composing with stay with the IME, so Enter committing a candidate doesn't open the focused file.
- **Matching**: Case-insensitive **prefix** matching on filenames. Wraps around the file list (searches downward from cursor, then wraps to top).
- **Live updates**: Results update as you type. The cursor moves to the first match.
- **Arrow Left/Right**: Adjusts the search string based on the focused file's name. Right extends the search to include more of the focused filename; Left trims it.
- **Press the filter key** (`/` by default): Switches to full Filter mode, keeping the current search text.
- **Cleared by**: Escape, any selection action, navigating to a different directory, or the entry it landed on disappearing (deleted, renamed, moved away). The cursor then goes to the nearest remaining entry, which needn't match the prefix. A Filter-mode filter is kept, being a view rather than a jump.

### Filter Mode (Visual Regex)

While rows are hidden, the file list carries a faint accent tint (`--color-bg-filtered`) as a standing reminder that the view is partial. Applies to Filter mode with a non-empty pattern, the only state that hides entries.

- **Activation**: Press the filter key (`/` by default, numpad `/` too), switch from Quick Search with the same key, or start typing when `quick_search = false`.

  Both routes run the one `start_filter` command — "switch this pane to filter mode, keeping whatever is typed" — so it is rebindable in the Keybindings tab and runnable from the command palette like anything else. That matters for non-US layouts: `/` is Shift+7 on QWERTZ, and the binding is matched as an exact normalized string, so those layouts capture `shift+/` instead. Unlike other commands it is matched inside the pane rather than dispatched from the window-level handler — the pane's own keydown runs first and routes every unmodified printable character into quick-search, so a key left to the dispatcher would be swallowed before reaching it.
- **UI**: A filter input bar appears at the bottom of the pane.
- **Matching**: Full **regex** pattern matching (case-insensitive). Files that don't match are hidden entirely.
- **`..` always visible**: The parent directory entry is never hidden by a filter.
- **Focus follows the matches**: a non-empty pattern moves the cursor to the first match whenever it would otherwise sit on `..` or nowhere — including when the entry it was on stops matching. A cursor already on a surviving entry stays put, so refining a pattern never yanks it away, and an empty pattern hides nothing and moves nothing.
- **Status bar**: Shows "(showing X of Y)" when filtering.
- **Selection persists**: Selection is retained even for files hidden by the filter. However, operations only act on *visible* selected files (`get_effective_selection()`); when the filter hides every selected file, they act on the focused entry instead, as with no selection.
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

**Tree walks** (`vfs::walk`): every traversal — copy/move/archive planning, delete, recursive attributes, the du enricher, recursive search — drives one depth-first walker through a `Visitor` (entries in pre-order with a Descend/Skip/Stop answer, `leave` per finished directory, Retry/Skip on an unreadable directory or an unresolvable link, a callback per symlink cycle). The rules live there once: `..` rows dropped, entries whose names would leave their directory skipped (an SFTP server can send a name like `../../.bashrc`, an S3 key can spell `a/../../x`; copied out, either would land outside the destination), a directory symlink entered only in follow mode and never twice (targets already on the path from the root, or chains over 40 hops, are reported as cycles), a mount point recognized by its device against its parent's and marked on the entry, `one_file_system` stopping at mount points below a root, `excludes` omitting paths and their subtrees. Roots are classified by stat, or from the parent listing on VFSes that cannot stat directories; a VFS root is a directory by definition. A root that is itself a directory symlink is not entered — copying, deleting or changing attributes of a link acts on the link — except with `follow_root` (`find -H`), which search sets: the root is walked as the directory it leads to, under its own path. The walk sees one `Vfs`, so anything mounted over it in the registry is invisible by construction. Directory-at-a-time walks **list ahead**: up to 4 directories still to come in depth-first order are being listed while the visitor works on the current one, so SFTP and S3 overlap their round trips; events are never reordered, only the I/O is, and a listing fetched ahead for a directory the visitor then skips is dropped when its parent is left. The du enricher's 16-way concurrency is one walk per sized entry, above the walker. **Flat listings**: a root is listed in one go where the descriptor advertises `can_list_recursive` (S3: one delimiter-less `ListObjectsV2` per page over the prefix, so sizing or copying a prefix costs a few paginated calls instead of one round trip per pseudo-directory), and such a VFS is never walked a directory at a time; the listing streams in path order, a page per batch, and the walker synthesizes the same enter/leave sequence a directory-at-a-time walk would produce as the batches arrive, directories included (a `dir/` marker object is the directory's own entry, every other directory is implied by its keys, and an empty prefix without a marker does not exist), so visitors are none the wiser and search hits and du totals flow per page. A listing that fails part-way goes through the visitor's Retry/Skip like an unreadable directory; on Retry the walker restarts it and drops everything up to the last path it consumed, which the ordering makes exact. The verb is remoted like the rest of the trait (the agent collects, the proxy hands over one batch).

### Create Directory (F7)

Modal dialog with a single name input field (auto-focused). Creates a new directory in the active pane's current path. A name typed here — or into Create File, Rename, or Copy/Move's new name — that climbs out of the directory (`..`, `../x`) is refused.

### Create File (from command palette)

Modal dialog with a name input. Creates an empty file in the current directory.

### Create and Edit (Shift+F4)

Creates a new file (same dialog as Create File) and immediately opens it in the built-in editor.

### Rename (F2)

Modal dialog with the current filename pre-filled and selected up to its extension, so typing replaces the name and keeps the type (`report.final.pdf` selects `report.final`); a directory's name, or a name with no extension or only a leading dot (`.bashrc`), is selected whole. Operates on the **focused** entry, ignoring the selection — and does nothing at all when that is `..` (see the `..` note under File List). Runs as an operation (`OperationRequest::Rename`) with the same two-step execution as Move: native `Vfs::rename` when the VFS supports it (tried as `rename_no_replace` first, as Move does), else copy+delete — so S3 objects and prefixes can be renamed (server-side CopyObject, no data through the app). Conflicts raise the standard Skip / Overwrite prompt; the fallback shows real progress and is cancellable. Renaming to the byte-identical name is a no-op, and renaming to a *different spelling of the same name* — `Foo` → `foo` on a case-insensitive volume, NFC → NFD on HFS+ — is a rename rather than a conflict: the destination stats successfully as the source, so `Vfs::same_file` is consulted before the conflict probe and suppresses it. Renaming one **hard link** onto another takes the same exemption and is then a no-op — POSIX has `rename` succeed without acting when both names are links to one file, so both survive and nothing is reported. That is BSD `mv`'s behaviour (GNU `mv` refuses instead); it's rare, and inaction is the safe end of the trade. Copying one hard link onto another counts as already copied (see Copy's conflict detection), since writing over it would destroy the file. The pane refreshes and re-focuses the new name when the operation completes. A directory renamed onto an existing directory asks Merge / Replace / Skip (Replace swaps it in; a package in the way is asked about as a file) — a rename merging silently would be a surprise.

### Batch Rename (Shift+F2)

Renames every entry of the effective selection (the selection, else the focused entry; never `..`) with one regex find & replace, previewed live. Also reachable from the Rename dialog: its **Batch Rename…** button (bottom left) switches to this dialog on the same selection. No recursion — a selected folder is renamed itself, its contents untouched — and only names: a new name containing a separator is an error, not a move.

**The rule**:
- **Find** is a regular expression (Rust `regex` syntax: no lookaround or backreferences), matched anywhere in the whole name, extension included; **every** match is replaced, and a name with no match stays as it is. `^` with a replacement adds a prefix. An empty Find renames nothing. An invalid pattern shows its error in place of the counts.
- **Replace** follows Monaco's (VS Code's) syntax rather than Rust's: `$1`…`$99` a group, `$<name>` a named group, `$&` or `$0` the whole match, `$$` a dollar; `\u` `\l` before a group upper/lowercase its first character, `\U` `\L` all of it. A reference past the pattern's groups is literal text, as in Monaco (`$12` with one group is group 1 then `2`).
- **Case-sensitive** (`Aa`, Alt+C / ⌥⌘C) — off by default.
- **Keep case** (`AB`, Alt+P / ⌥⌘P) — VS Code's Preserve Case: each replacement takes the case shape of the text it replaces (all upper, all lower, capitalized, or part by part across `-` and `_`).
- The two toggles are remembered in `state.json` (`batch_rename.*`); Find and Replace start empty every time.

**Preview**: a list styled like a pane — file icons, Name and New Name columns, the matched text and its replacement in bold, unchanged names greyed. Above it, `13 renamed · 1 unchanged · 2 errors · 1 warning` and a filter (All / Renamed / Problems). Arrow keys, Page Up/Down and Home/End move a cursor in the list; **F8 / Shift+F8** jump to the next / previous row with a problem. The problem is written beside the new name and is the row's tooltip.
- **Errors** (Rename stays disabled): an empty name, `.` or `..`, a separator or NUL, a name Windows refuses on a Windows-style filesystem (`<>:"|?*`, control characters, a trailing dot or space, reserved names like `CON` or `nul.txt`), or two entries of the selection ending up with byte-identical names in one folder (including onto a selected entry the rule leaves alone).
- **Warnings** (shown, not blocking): the new name replaces an entry outside the selection; or — where the filesystem might ignore case — it matches another new name or an existing entry give or take case or Unicode normalization ("may replace", "where case is ignored"). Names on S3 and in archives are compared byte for byte, so these never appear there. On a filesystem with no rename of its own (S3) the status line says renames there copy and delete.
- Name length is not checked: the limit is the filesystem's, and a name it refuses fails that entry during the rename.
- Hidden entries the pane isn't showing aren't known to the preview; the rename itself still finds them.

The preview is computed in the Rust host, not in the webview: the selection is snapshotted when the dialog opens, every new name is recomputed (off the async runtime, ~100 ms for 100,000 names) a moment after typing pauses, and the dialog is sent the counts and the rows in view only, so selections in the hundreds of thousands stay responsive.

**Running it** (`OperationRequest::BatchRename`, in the operations panel, cancellable, agent-side in remote sessions):
- **Order.** Each entry's new name is vacated before it's taken: a chain (`1→2`, `2→3`) runs from its end, and a cycle (`a↔b`, `x→y→z→x`) is performed as one unit. Dependencies are judged conservatively — by Unicode case folding and normalization wherever names might fold — so a swap like `a→B`, `b→A` on a case-insensitive volume is recognized; on a case-sensitive one such a phantom cycle costs one unnecessary swap.
- **Cycles** use an atomic exchange where the filesystem has one (Linux `renameat2(RENAME_EXCHANGE)`, macOS `renamex_np(RENAME_SWAP)`): a cycle of *k* entries is *k*−1 exchanges, and no file is ever under a temporary name. Elsewhere (Windows, SFTP, S3) the first entry steps aside to a hidden `.newt-rename-…` name in the same folder, the rest follow, and it takes its new name last. A cycle is never interrupted by Cancel, and one that can't complete is undone — every entry back under its old name.
- **The filesystem decides.** Every rename is a no-replace rename first, as single Rename does; a re-spelling of the entry itself (`readme` → `README` where case is ignored) goes through. When something else has the name, it's either an entry of the batch that hasn't moved yet — the rename waits for it, and entries still waiting after a full round step aside to a temporary name to break the cycle the names didn't show — or one that has its name for good, which is **never overwritten** (the prompt offers Skip only), or an entry outside the batch, which gets the normal Skip / Overwrite prompt with apply-to-all. Telling a batch entry from an outsider is exact where the listing has file identities (local filesystems: device and inode) and on byte-compared names (S3, archives); over SFTP it asks the server about entries whose names fold together.
- Before starting, each folder is listed again: an entry that has disappeared is reported and skipped, and the checks run again on what is there now.
- In a search pane each hit is renamed in its own folder, and a folder's selected contents are renamed before the folder itself.
- On S3 each rename is a server-side copy and delete, as single Rename; a folder copies everything under its prefix.
- When it completes, the pane refreshes and selects the renamed entries under their new names (ready for another pass), with the cursor following the focused entry to its new name.

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

**Mount points stay** (`rm --one-file-system`): the walk compares each directory's `device_id` with its parent's, so a filesystem mounted under the selection is recognized as such — bind mounts share their source's device and are not, btrfs subvolumes have their own and are. By default the walk stays out: the mount point's contents are left alone and a Skip-only `MountPoint` issue says so, its Details naming the option below (apply-to-all silences the rest). **Descend into mount points**, in the confirmation dialog's "More options" fold, walks in and deletes the contents. Either way the mount point directory itself is never removed (it cannot be while mounted) and neither is the chain of directories above it, so the operation ends clean rather than in a cascade of "busy" and "not empty" issues; a selected directory that is itself a mount point is emptied and left standing. The tickbox is not remembered between dialogs, and it is unreachable with confirmations off, so an unconfirmed delete always stays put. Only the local filesystem reports devices (in a remote session, the agent's), so nothing is ever a mount point on SFTP, S3 or inside archives; on Windows junctions and volume mount points are reparse points, which the walk never follows.

### Copy (F5)

Opens a modal dialog with:

- **Destination path**: static label showing the other pane's directory (not an input — the destination is always the other pane).
- **Summary**: Shows the filename (single file) or "N items" (multiple selection).
- **New name** field (single item only, copy and move): pre-filled with the source's leaf name; edit it to land under a different name in the destination (`rename_to` on the operation — same-VFS moves rename directly to the new name, and the copy fallback plans the tree under it). Left unchanged it sends nothing. Also names the symlink when "Create symbolic link" is checked. Empty or separator-containing input greys out the primary button, since the value becomes a single leaf under the destination. *Which* characters separate comes from the destination's `PathStyle` (`name_separators` on the modal payload), not from the frontend: `\` is rejected on a Windows-styled destination and accepted on a Unix one, where it is an ordinary filename character.
- **Options** (checkboxes):
  - **Create symbolic link** — only available for single-file copies. Creates a symlink at the destination pointing to the source. Disables the other options when checked.
  - **Preserve timestamps** — modification and access times.
  - **Preserve permissions** (default on) — Unix mode bits.
  - **More copy options** ("More move options" for a move; collapsed section, keyboard-reachable like any other control; opens expanded when a remembered toggle inside it differs from its default (extended attributes default on for macOS, streams for macOS and Windows), so a sticky choice is never hidden):
    - **Existing files**: Ask (default), or answer every file conflict up front with any answer the prompt offers (see Issue resolution) — the same as giving it with "apply to all" at the first prompt.
    - **Existing folders**: **Merge** (default) — copy into it, keeping what only the destination has; **Replace** — put the source in its place; **Ask** — Merge / Replace / Skip per folder. Not remembered, like Existing files. A *package* in the way (see below) is not a folder to this setting but a file to Existing files.
    - **Preserve owner** and **Preserve group** (separate toggles), with **Match accounts by** numeric ID (native identity on Windows: the security-descriptor owner) or account name resolved on the destination.
    - **Preserve extended attributes** (user xattrs; ACL and resource-fork xattrs are excluded because they have their own toggles). On by default on macOS, as Finder and `cp` keep them: without them a copy loses Finder tags and colour labels, the quarantine flag (so a copied download would skip Gatekeeper's first-open check), and the signatures of signed scripts — including an app whose executable is a script, which then reports as damaged. Off on Linux, where `cp` keeps them only with `-a` and they are mostly security labels, and on Windows, which has none to read: a Windows file has nothing to carry, while one copied *to* Windows with extended attributes still raises "Cannot preserve".
    - **Preserve access control lists** — Linux POSIX ACLs, macOS extended ACLs, Windows DACLs. Carried opaquely in the source platform's encoding; only a destination of the same platform can apply them.
    - **Preserve alternate streams and resource forks** — macOS `..namedfork/rsrc`, NTFS `:stream:$DATA`; streamed like file contents. On by default on macOS and Windows (as Finder and Explorer keep them — Windows' Mark of the Web is the `Zone.Identifier` stream), off on Linux. Resource forks are legacy (signed apps can't have them); without them custom icons, Mac OS 9-era files and old aliases lose what's in the fork.
    - **Preserve hard links within the selection** — files sharing an identity (device + inode) are linked on the destination instead of copied again.
    - **Preserve sparse files** — zero runs are skipped and holes punched on the destination (Linux `fallocate`, macOS `F_PUNCHHOLE`, NTFS sparse flag).
    - **Apply source attributes to existing directories** — merged directories otherwise keep their own mode/times/owner.
    - **Stay on one filesystem** — rsync's `-x`: a mount point under the selection is copied as an empty directory. Off by default, since a backup of a tree with a volume mounted under it usually wants the volume too. A move's copy half follows the same rule, and its clean-up pass never removes a mount point or the directories above it (see Delete).
    - **Symbolic links**: copy the links (default) or copy their targets (copy only; the walker resolves links on the FS-owning side and skips cycles with an issue).
    - **Object store group** (shown when either side is S3): **Preserve object metadata and content headers**, **Preserve object tags**, **Preserve object access grants** (opt-in — grants are not carried by default, even S3→S3), and for an S3 destination a **Storage class** and **Object access** canned ACL.
  - **Sticky toggles** (`state.json` `copy_move.*`, the `CopyMoveDefaults` struct): only the "preserve the source's X" toggles and the match-accounts-by switch are remembered, and only where they differ from their default — one left at its default is stored as unset, so it follows the default if that changes (extended attributes and streams default by platform). Link modes, storage class, canned ACL, "apply source attributes to existing directories", "stay on one filesystem" and the existing-files and existing-folders answers reset every time — each changes what gets written beyond a faithful copy or who can read it.
- **Pack into archive…** button (copy only): swaps the dialog for Pack to Archive over the same selection.

**Copy execution**:

0. **Self-destination check**: before any scanning, an operation whose destination *is* one of its sources fails outright — no conflict prompt, since there is no sane resolution (Overwrite would open the destination truncating while the source read was still pending, emptying the file). Byte comparison can't answer this — `/a/Foo` and `/a/foo` are one file on a case-insensitive volume, as are NFC and NFD spellings on HFS+ — so the filesystem is asked via `Vfs::same_file`. Copy refuses every spelling of a self-copy, matching `cp`'s "'x' and 'x' are the same file". Costs one filesystem question per distinct source directory, since a source can only land on itself when its own directory is the destination.
1. **Planning phase**: Recursively traverses all source directories to build a complete file list. The UI shows "Scanning..." with a live count of items and bytes discovered so far. Subdirectory scan errors raise a skip/retry prompt rather than aborting the whole operation.
2. **Conflict detection** (ask forgiveness, not permission): a file is written with `WriteOptions::create_new`, so the destination refuses a write over anything already there, and only a refusal leads to a look at the destination and the prompt. A file copied to a free name is never stat'd, and nothing can appear between a look and its write. A destination that cannot refuse this particular write answers `NotSupported` (SFTP; S3 for anything larger than one request, and on custom endpoints) and is looked at first, as is everything that isn't a plain file: directories, symlinks, files preserved as hard links. A look that fails for any reason but "not found" (a 403, a dropped connection) is a Skip/Retry issue, never taken for an empty destination. What a look finds:
   - File → File: Offers Skip / Overwrite, with the conditional overwrites (see Issue resolution) in a menu on Overwrite, unless the dialog answered up front. A known Skip (sticky or preset) skips a refused file without the look.
   - The source itself, on the same VFS (`Vfs::same_file`; a failed check is a Retry/Skip issue, as in Move): a hard link of the source, or the source reached through a symlinked directory in the destination path. Counted as already copied, with no prompt, whatever the answer: overwriting would truncate the source before it is read. Copying a tree over its hard-linked snapshot therefore asks nothing about unchanged files. A move leaves the source name too, as renaming one hard link onto another does.
   - A symlink at the destination (to a file, to a directory, or dangling) is replaced by anything but a directory: Overwrite unlinks the link, never touching what it points to. A directory copied onto a link to a directory merges through the link into its target, as `cp -R` does (rsync would replace the link); the link was put there deliberately, and the pane presents it as the folder it points to.
   - Directory → Directory: what **Existing folders** says — Merge (copies contents into the existing directory, the default), Replace, or Ask (Merge / Replace / Skip, its own issue type so "apply to all" never answers a file). A skipped directory leaves out everything under it. On a VFS whose directories are key prefixes (S3, `can_stat_directories` off) a directory has nothing to find and is created without a look.
   - **Replace never leaves the destination missing**: the source is copied whole to a hidden `.newt-replace-…` name beside it, then swapped in with `Vfs::exchange` (`renameat2(RENAME_EXCHANGE)` on Linux, `renamex_np(RENAME_SWAP)` on macOS), and the old directory is removed. Without an exchange the old one is renamed aside, the copy renamed in, and the old one put back if that fails. A copy that fails or is cancelled is removed and the destination is untouched. Until the swap both exist, so the destination needs room for both. A move on one filesystem needs no copy: the source is swapped in directly.
   - **A package at the destination** is replaced like a file. Where a directory lands on an existing directory, the destination's filesystem is asked whether that directory is a package (`Vfs::is_package`: Launch Services on a Mac, locally or in a remote session to one; never anywhere else); if so, the conflict is a file's — "Package already exists", answered by **Existing files** with Skip / Overwrite and the conditional overwrites, where *if newer* compares the packages' own times and the size answers walk both packages for their total. So copying a newer `Foo.app` onto an older one replaces it instead of leaving the old version's stale files inside, at any depth of a copy, while ordinary folders keep merging. Only the destination decides: a package copied onto a folder on Linux merges there, as any folder does.
   - File → Directory or Directory → File (type mismatch): Error, offers Skip.
   - Nothing, after a refusal: the file vanished since, or S3 reported a concurrent delete (409). One more exclusive attempt, then an I/O issue.

   An exclusive write that fails leaves nothing: the local writer removes a file it created exclusively unless the write finished (a read error, a cancel), and S3 creates nothing until the upload completes. A retry is therefore as exclusive as the first attempt, and a refusal always means someone else's file.
3. **Copy strategies**:
   - Same-VFS `copy_within` (fastest, if available — kernel-assisted: FICLONE/`copy_file_range`/`CopyFileEx`, S3 server-side CopyObject). Under `create_new` the local VFS reserves the name with `O_EXCL`/`CREATE_NEW` and copies into that file through the descriptor the exclusive open returned (`FICLONE`, else `copy_file_range` and the source's mode, on Linux; `fcopyfile` on macOS), never by reopening the name, which would follow a symlink swapped in since; Windows' `CopyFileExW` takes names only. A failed copy removes the reservation. On macOS a clone comes first: `clonefile` follows a dangling symlink at its destination and creates the target, so the clone lands under an unguessable staged name beside the destination (retried on a collision) and is renamed in with `RENAME_EXCL`; a clone that does not go in is removed. S3 sends a conditional CopyObject. A `copy_within` that cannot refuse is retried without `create_new` after the look, rather than falling through to streaming, so a server-side copy is never lost to it.
   - Streaming copy otherwise: async read → async write in 64 KB chunks, drop-cancellable at every await. Under `create_new` the destination is opened before the source, so a refusal costs no source open and holds none across a prompt; an exclusive create that doesn't finish (an unreadable source included) is removed by its writer. Without it the source opens first, so a source that can't be read never truncates what is there.
4. **Attribute preservation**: After each file lands (and after a directory's contents, deepest first), the requested attributes are applied in an order that keeps them from undoing each other: ownership (chown can clear set-id bits), then xattrs and streams (writing them touches times), then permissions, then ACLs (chmod rewrites the POSIX ACL mask), then timestamps. Timestamps, permissions and numeric ownership come from one `Vfs::get_metadata` stat; everything else through `Vfs::read_attribute`/`write_attribute`, which keep values in their native encoding across the RPC boundary. Attributes that must exist at creation (sparse, S3 metadata/storage class/canned ACL) travel as `WriteOptions` on `overwrite_async`/`copy_within`; a VFS that cannot honour them returns not-supported and the copy retries without them after a prompt. **Any attribute that cannot be read or applied raises "Cannot preserve <kind>" with Retry/Skip**; "Apply to all" Skip is scoped to that attribute kind. A destination that declares no metadata support (S3) gets no stat-derived attributes and no prompts about them.
5. **Progress**: Reports every 100ms with bytes done, items done, and current filename.

**Special files**: FIFOs, sockets and devices (by the file-type bits of their mode) are never read — a FIFO would block until something wrote to it, `/dev/zero` would never end. Copy, a move between filesystems and Pack to Archive leave each one out, saying so with a Skip-only prompt ("Skipped …: not a regular file"); a move within one filesystem renames them like anything else, and Delete removes them.

**Names on Windows**: a local Windows destination takes a name as it is, even one other Windows programs can't handle — a trailing dot or space (`notes.`), a reserved device name (`nul`, `con.txt`, `aux.tar.gz`) — since Newt reaches local files through `\\?\` paths, which skip Win32's name rewriting. Newt lists, opens, renames and deletes such files like any other; Explorer shows them but can't open or delete them. A name with a `:` is refused (a Skip/Retry issue, "can't be created here"), for copies, moves, extraction, New Folder and renames alike: Windows would take `a:b` as an alternate stream `b` of a file `a`, so `a:b` and `a:c` would both vanish into one empty-looking `a`. Characters NTFS itself refuses (`<>"|?*`, control characters) fail with Windows' "syntax is incorrect" error. Batch Rename still refuses all of these as new names.

**No download marks**: a file Newt brings onto this computer — copied from S3, SFTP or a remote session, extracted from an archive or disc image, or downloaded to open in an application — gets no macOS quarantine flag (`com.apple.quarantine`) and no Windows Mark of the Web (the `Zone.Identifier` stream), so Gatekeeper and SmartScreen don't treat it as downloaded. That is deliberate: Newt is a file manager moving files the user chose, like `cp`, `scp` or `tar`, not a browser. A copy between local folders keeps whatever marks the source has, through **Preserve extended attributes** (macOS) and **Preserve alternate streams** (Windows).

**Symlink handling**: With "Create symbolic link" checked, creates a symlink directly (no file content copied). Only available for single files.

**Positioned reads everywhere**: `vfs::open_read_at` hands out a backend's own positioned-read handle, or one over `read_range` for a VFS without them (archive and disc mounts, whose entries never change under a reader). Everything that reads through handles — the viewer's file server, so media plays from inside archives; nested mounts, so a tar inside a zip indexes; the agent-side `OPEN_READ_AT` dispatch — goes through it.

**In-place writes**: `Vfs::write_range` overwrites a byte range of an existing file without truncating or extending it, behind the `can_write_range` capability (Local, SFTP and remote sessions; never S3). Its first user is the 7z start-header patch; anything that fixes up a fixed-layout file after streaming it belongs here too.

**Spooling** (`newt_common::spool`): a `Spooler` per session hands out `Spool`s — append-only buffers with in-place patching, kept in memory up to a threshold (32 MiB) and spilled to a temp file past it, every held byte counted against the spooler's optional quota and released when the spool or its reader is dropped, temp file included. An operation that must hold bytes before it can hand them on (a 7z bound for S3) spools rather than inventing its own temp handling.

**Cross-VFS copies**: Fully supported. You can copy files between any combination of Local, S3, SFTP, and Archive VFS types — every VFS speaks the same async read/write surface, so any readable source streams into any writable destination.

### Move (F6)

Same dialog and options as Copy (except "Create symbolic link" is not available).

**Move execution**:
0. **Self-destination check**: as for Copy, but Move refuses only the true no-op — same file *and* a byte-identical leaf name. A differing leaf (`Foo` → `foo` in place on a case-insensitive volume) is a re-spelling, which is legitimate and falls through to the rename below. This is the `mv Foo foo` works / `cp Foo foo` fails split that coreutils has.
1. **Try fast rename** (same VFS only): Attempts atomic rename for each source. Instant if it works. It first tries `Vfs::rename_no_replace`, which refuses an existing destination atomically (`renameat2(RENAME_NOREPLACE)` on Linux through the raw syscall, `renamex_np(RENAME_EXCL)` on macOS, `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows; `NotSupported` on SFTP and on filesystems without the flag), so a move into a free name stats nothing. Any failure goes on to the look below. The rename path also performs conflict detection — if the destination already exists, the same conflict prompt as Copy is shown rather than silently overwriting; a failed look is an issue, not an empty destination; a file left in place by a conditional answer stays at its source. A destination that resolves to the *source itself* is exempt: on a case-insensitive volume `Foo` → `foo` stats successfully as the source, and that is the point of the move rather than an obstacle to it. `Vfs::same_file` is consulted only once something is actually in the way, so bulk moves stay at one probe apiece. An approved overwrite still goes through the plain rename (atomic replace on POSIX and posix-rename SFTP servers); only if the backend refuses with "already exists" is the destination cleared and the rename retried. Directory-onto-existing-directory goes straight to the copy machinery, which follows **Existing folders** and the package rule (see Copy); a Replace there swaps the moved directory in. Symlinks follow Copy's rule: a link moves as a link whatever it points to, a link to a directory at the destination is replaced by anything but a directory, and a directory merges through it.
2. **Fallback to copy+delete**: Only a `NotSupported` rename — the VFS has no rename, or this particular pair can't be renamed (cross-device inside the root VFS, cross-VFS) — falls back to copying each file and immediately deleting the source after successful copy. Real rename failures (permissions, connection) raise a Skip/Retry issue instead of silently degrading. After all files are copied, empty source directories are removed in reverse order (deepest first). Directories that still contain files (because some copies were skipped) are left intact. The same rule governs the same-VFS server-side copy fast path: `copy_within` falling over with `NotSupported` (e.g. S3 CopyObject's 5 GiB cap) cascades to streaming; real errors surface as issues.

### Pack to Archive (Alt+F5)

Packs the active pane's selection into a new archive in the other pane's directory. Fully streaming through the VFS layer — archive bytes are produced chunk-at-a-time and written straight to the destination, so there are **no temp files and no whole-archive buffering**, regardless of which side is remote (local→S3, S3→local, remote-session sources, etc. all stream end-to-end).

Opens a modal dialog with:

- **Format tab bar**: `zip`, `7z`, `tar`, `tar.gz`, `tar.xz`, `tar.zst`. Switching formats swaps the extension on the name field.
- **Archive name** (auto-focused, stem pre-selected): defaults to the single selection's stem, or the containing directory's name for multi-selections. Empty or separator-containing input greys out the primary button — the archive is a single leaf in the destination, so to write it into a subdirectory you navigate the other pane there. Which characters separate comes from the destination's `PathStyle` (`name_separators`, shared with the Copy/Move dialog), so `\` blocks on a Windows-styled destination and is a legal name character on a Unix one.
- **Destination** display (read-only, the other pane's directory).
- **Compression level** (per-format, seeded from the `[archives]` preferences): gzip/xz/deflate/LZMA2 0–9, zstd 1–22; zip and 7z level 0 store entries uncompressed. Hidden for plain tar. Each format remembers its own level while the dialog is open.
- **Preserve symlinks** (default on, seeded from preferences): stores symlinks as symlink entries. When off, symlinks are followed — symlinked files are stored as regular files, symlinked directories are descended into (with cycle detection; a cycle raises a skip prompt).
- **Password** (zip and 7z, optional, with confirm field): zip gets WinZip AES-256 (AE-2), 7z gets 7-Zip's AES-256 per solid block with the header in the clear (file names stay visible, as 7-Zip without `-mhe`). Both open in 7-Zip/WinRAR/Keka and Newt's own archive VFS (lazy askpass); zip's not in Windows Explorer or macOS Archive Utility.

**Writers** (in-tree `newt-archive` crate, sans-IO streaming state machines):

- **tar**: ustar with pax extended headers when needed (long/unsplittable paths, long link targets, files ≥ 8 GiB, large uid/gid, pre-epoch or sub-second mtimes). Preserves mode, uid/gid (or uname/gname), and mtime from the source dirent; sensible defaults (0644/0755, archive-creation time) when the source VFS has no such metadata (e.g. S3).
- **zip**: streaming data-descriptor mode (no seeking — this is what makes append-only sinks like S3 multipart possible), UTF-8 names, per-entry zip64 committed up front from the scanned size, zip64 EOCD for >65k entries or >4 GiB offsets, unix modes in external attributes, symlink entries, extended-timestamp extra field. DOS times are written as UTC.
- **7z**: solid blocks of 256 MiB of input in walk order, one raw LZMA2 stream each from the bundled liblzma (level 0 stores), unix modes and mtimes in the attributes, symlinks as 7-Zip's `-snl` stores them, per-entry CRCs, a plain (uncompressed) header. The format's one non-streamable piece is its 32-byte start header, which names the header's offset, size and CRC: the writer emits a zeroed placeholder first and hands the real bytes back from `finish`. Landing them takes the destination's `write_range` (Local, SFTP, remote sessions); a destination without it (S3) gets the archive **spooled** — in memory up to 32 MiB, then a temp file on the side running the operation, the agent's host in a remote session — and uploaded whole as a second progress phase. No BCJ filters yet, so executables compress a little worse than 7-Zip's output.

**Execution**:

1. **Planning phase**: same recursive scan as Copy (live "Scanning…" counts, skip/retry on unreadable subdirectories). The destination archive itself is excluded from the walk (overwriting an archive that sits inside the selection doesn't pack its stale self). Duplicate top-level names across sources fail up front rather than silently colliding inside the archive.
2. **Conflict detection**: if the destination file exists, offers Skip (cancels the operation — single artifact) / Overwrite. An old archive among the sources is replaced, not packed into the new one; packing a file into itself alone (`foo.zip` as `foo.zip`, or `FOO.zip` on a case-insensitive volume) fails instead, since Overwrite would only empty it. A destination that is a symlink is replaced by the archive, not written through to its target. A destination that can't be looked at (permission denied, a dropped connection) fails the operation rather than being taken as absent.
3. **Per-entry streaming**: sources are opened *before* their header is committed, so open failures offer Skip/Retry cleanly. A read error mid-entry finalizes the entry as truncated (tar zero-pads to the declared size) and offers Skip only — the stream can't rewind. Files that grow or shrink between scan and pack are truncated/padded with a logged warning, matching GNU tar's "file changed as we read it" spirit.
4. **Failure/cancel cleanup**: the partial archive is removed best-effort; an S3 multipart upload is aborted (also on drop — writers discarded mid-stream no longer leak uploads).
5. **Progress**: bytes count source bytes read, so the bar tracks the scanned totals regardless of compression ratio.

The Copy (F5) dialog has a **"Pack into archive…"** button that swaps it for this dialog over the same selection.

Hardlinks are not detectable through the VFS surface and are archived as independent file copies.

### Properties (Alt+Enter)

Modal dialog showing file metadata. Supports single files and multi-file selections. The Unix permission/ownership editor appears only where the filesystem can change metadata and the entries have Unix modes — not on S3, in archives, or for local Windows files; VFSes with extended properties (S3) still get their own editable sheet groups (see below).

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
- Text input takes a numeric UID / GID; anything else (an account name — names aren't resolved) keeps Apply disabled.

**Symlinks**: a permissions change never reaches a link's target — a link's own mode is left alone (it is fixed on Linux and unused elsewhere), whether the link was selected or met in a recursive walk; ownership changes apply to the link itself (`lchown`).

**Recursive** checkbox (for directories): Applies permissions and ownership changes to all contents. The walk stays out of filesystems mounted under the selection (a `chmod -R` that runs onto a mounted volume rewrites the whole volume's permissions), raising the same Skip-only `MountPoint` issue as Delete; **Descend into mount points**, shown beside it once Recursive is ticked and forgotten when the dialog closes, walks in. Applies to sheet edits too.

**Extended properties (property sheets)**: VFSes that advertise `has_extended_properties` contribute extra editable groups below the generic metadata. The sheet is schema-driven — the backend describes fields (text, choice, key-value map, grant list) and one generic renderer edits them all; no per-VFS frontend code. Sheets load after the dialog opens (loading placeholder → filled in place), so Alt+Enter never stalls on network calls. Multi-select folds per-field: equal values show, differing ones show as mixed/indeterminate and are left untouched unless edited; grant lists fold whole (differing lists offer an explicit "replace on all"). Applying goes through the operations engine (progress, per-item retry/skip, cancel) as an `ApplyProperties` operation; the **Recursive** checkbox extends to sheet edits (per-prefix apply on S3, skipping synthetic directory entries).

Today only S3 implements a sheet:
- **S3 metadata** group: user metadata (`x-amz-meta-*`) as an editable key-value map (add/remove/edit keys), storage class, Content-Type, Cache-Control. Edits rewrite the object in place (CopyObject with metadata replacement) — untouched system headers and any non-default ACL are preserved across the rewrite; the dialog shows a hint that this can be slow for large objects. The rewrite fails on objects over 5 GiB (CopyObject cap — would need multipart copy) and on unrestored Glacier-class objects; both surface as per-item operation issues.
- **S3 access control** group: the grant list (grantee user ID / group URI / email × permission) and a write-only canned ACL selector (S3 reads back grants, not the canned value). Omitted gracefully when `s3:GetObjectAcl` is denied — the metadata group still loads.

**Directory Properties**: Available from the pane context menu (right-click empty space). Shows metadata for the current directory itself.

### Clipboard Operations

Copy, cut and paste files through the system clipboard, within Newt and to and from other apps — Finder, Explorer, Nautilus, Nemo, Thunar, Caja, Dolphin, PCManFM-Qt, mail and chat apps.

- **Copy to Clipboard** (Mod+C, also Ctrl+Ins off macOS) and **Cut to Clipboard** (Mod+X) put the selection (or the focused entry) on the clipboard: as files, for other apps, when they're on this computer; as their paths, as text, always — so a text editor or a terminal gets the paths. Cut files show faded in the panes listing them until they're pasted or something else is copied, anywhere.
- **Paste** (Mod+V, also Shift+Ins off macOS) copies what's on the clipboard into the pane's current folder — never into the folder under the cursor — and moves it after a Cut. It starts at once, like a drop, with the default copy options; clashes ask as in any copy. A cut pastes once: the clipboard is emptied when the move finishes. Pasting a cut into the folder the files are in does nothing but use up the cut (files from elsewhere among them still move); pasting a copy there says a file can't be copied onto itself.
- **Move Here** (⌥⌘V on macOS, unbound elsewhere) pastes as a move whatever was copied, as in Finder.
- **Copy Path** (⌥⌘C on macOS, Ctrl+Shift+C elsewhere) puts only the paths on the clipboard, as text, so a chat or mail app pastes the path rather than attaching the files.
- **Files that aren't on this computer** — on S3 or SFTP, in an archive, on a remote session's host — copy and paste within their window like any others, but reach other apps only as their paths: Finder or Explorer can't paste them.
- **Between windows**, a copy arrives as it would from Finder: files on this computer paste (in a remote session, only one allowed access to this computer — otherwise Paste says so); other files arrive as their paths, and Paste goes there if this window can (a path on an S3 bucket it has mounted).
- **macOS has no cut on the clipboard**: a Cut is known only to the window it was made in, where Paste moves. Finder or another window pastes it as a copy (⌥⌘V there moves it).
- **When the clipboard holds no files**, Paste goes to the path it holds, as typing it into Go to would; with nothing usable on it, Paste does nothing.

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
- The foreground modal shows the issue (e.g., "File 'readme.txt' already exists"). An issue that carries a `detail` gets a collapsed **Details** panel under the message, selectable text (paths, the option that would have avoided the issue); the compact background row shows the same text as a hover tooltip.
- Available actions depend on the issue type:

| Issue Type | Available Actions |
|-----------|------------------|
| File already exists | Skip, Overwrite; copy and move add Overwrite if newer / if size differs / if size or date differs |
| Folder already exists | Merge, Replace, Skip |
| Package already exists | as File already exists |
| Permission denied | Skip, Retry |
| Other I/O error | Skip, Retry |

- **Conditional overwrites** sit in a menu on the Overwrite button (its chevron half) and are decided per file from the two entries' size and modification time; no content is read. *If newer*: the source's time is later. *If size differs*: for destinations whose times say nothing (S3, copies made without "Preserve timestamps"). *If size or date differs*: rsync's quick check; a file matching on both is skipped. Times compare exactly, to the millisecond, so a copy to a filesystem with coarser times (DOS timestamps in zip and FAT, whole seconds over SFTP) differs from its source. An unknown size or time is never newer and always differs. Without "Preserve timestamps" a copy carries the time it was made, so it is newer than its source.
- **"Apply to all" checkbox**: When checked, the chosen answer is applied to all subsequent issues of the same type within the same operation that offer it; a conditional answer is still decided per file. An issue that does not offer the sticky answer (a directory in the way after "Overwrite" for all) still prompts, while Skip covers it. The dialog's **Existing files** answer is this same sticky answer, set before the operation starts. "Already exists" I/O errors outside a conflict check are ordinary I/O errors and are not covered. Retry always applies only to the current attempt, regardless of the checkbox; another failure prompts again.

---

## 5. File Viewer (F3)

Opens in a separate window (1100×800 pixels). Automatically detects the file's MIME type and selects the appropriate viewing mode. A FIFO, socket or device isn't opened — F3, F4, a link followed from a document and `newt view`/`newt edit` say it isn't a regular file, and Quick View shows nothing for one, as for a folder.

### Pre-warmed Windows

Viewer windows are **pre-warmed** for instant opening. A hidden window is created in the background with all web content and static UI already loaded. When F3 is pressed, the pre-warmed window receives the file path via the existing `UpdatePublisher` state mechanism, attaches its menu bar, and is made visible — avoiding the WebKit startup and JavaScript initialization latency. A replacement pre-warmed window is spawned immediately in the background. Falls back to direct window creation if no pre-warmed window is available.

Pre-warms are created only from the `init` command — i.e. after the main window's frontend has demonstrably booted — never during setup. Creating additional WebView2 windows while the main webview is still in its early composition phase intermittently wedged it into a permanently blank (white) window on Windows, at up to ~50% of Explorer launches on slow machines. Do not move prewarm creation back into the startup path.

### Mode Selection

The viewer has a **View** menu with radio buttons to manually switch between modes: Text, Hex, Structure, Table, Markdown, Image, Audio, Video, PDF. The initial mode is chosen automatically:

| Detected Type | Mode |
|---------------|------|
| `text/csv`, `text/tab-separated-values` (`.csv`, `.tsv`) | Table |
| `text/markdown` (`.md`, `.markdown`) | Markdown |
| `text/*`, `application/json`, `application/xml`, `application/javascript`, `application/typescript`, `application/x-sh`, `application/x-python`, `application/sql`, `application/x-yaml`, `application/toml`, `application/graphql`, `image/svg+xml`, anything ending in `+xml` or `+json` | Text |
| `image/*` | Image |
| `audio/*` | Audio |
| `video/*` | Video |
| `application/pdf` | PDF |
| Anything else the [Structure](#structure-mode) dissector recognises | Structure |
| Everything else | Hex |

A [file association](#file-associations) with a `viewer` mode comes before the table — the built-ins open `.ts` as text, not as an MPEG transport stream video.

### Mode Toggle

The status bar includes mode toggle buttons on the right side: the auto-detected mode and its counterpart. Pressing **F3** toggles between the two. The counterpart is Text for Hex, Text — the source — for Table and Markdown, and the `viewer.alternate_mode` preference for every other mode: Structure by default (auto=Image, current=Image → F3 → Structure → F3 → Image), or Hex. A file that opened in that mode itself pairs with Hex.

### Text Mode

- Line-numbered display with a non-selectable gutter. Gutter width adjusts to fit the number of digits in the total line count.
- **Chunked loading**: Loads files in 128 KB chunks on demand. Large files don't need to be fully loaded before viewing. LRU cache holds up to 32 chunks (4 MB); older chunks are evicted as new ones load.
- **Encodings**: Decoding happens in the webview with `TextDecoder` (in Markdown mode, in Rust with encoding_rs under the same names), so every WHATWG encoding it knows is available: UTF-8, UTF-16 LE/BE, the Windows-125x and ISO-8859-x families, KOI8, Mac, Shift_JIS, EUC-JP, ISO-2022-JP, GBK, GB18030, Big5, EUC-KR. The catalogue lives in `viewer/encoding.rs` (encoding_rs canonical names, which double as `TextDecoder` labels); UTF-32 has no `TextDecoder` and is not offered. The **Encoding** menu (Text, Table and Markdown modes) has an *Auto-detect (name)* entry followed by the catalogue grouped by script; the pick is per viewer window and the status bar shows the effective encoding, with "(BOM)" when one was honoured.
  - **Detection** is sniffed from the data the viewer already has: once the first chunk lands, the text viewer hands its leading 64 KiB to `sniff_viewer_encoding`, which records the result in the viewer's Rust state (menu and status bar follow via the usual state push). Order: BOM (UTF-8, UTF-16 LE/BE), then BOM-less UTF-16 by null-byte parity, then UTF-8 validity (a sequence cut off by the prefix boundary is not held against it), then `chardetng` (Firefox's detector) for legacy codepages. The view renders as UTF-8 until the result lands and then switches once; the line index is rebuilt only when the newline scan changes (UTF-16 needs an aligned code-unit scan; every other catalogue encoding never places 0x0A inside a multibyte sequence) or when a BOM moves the start of line 0.
  - **Columns ↔ bytes**: selection columns are UTF-16 code units of the decoded line. Mapping back to bytes is 1:1 for single-byte encodings, 2:1 for UTF-16, a lead-byte count for UTF-8, and a byte-at-a-time streaming decode for the legacy multibyte encodings (the browser has no encoder for them). ISO-2022-JP is decoded per line, so a line that ends inside a JIS run shows the next line wrong until an escape resets it.
  - **Copy and search** transcode on the host with `encoding_rs`: copy decodes the selected bytes in the effective encoding, literal search encodes the query into it (UTF-16 spelled out by hand, since encoding_rs encodes UTF-16 as UTF-8 per WHATWG). Regex search stays a byte regex over the raw file, so non-ASCII regex literals only match in UTF-8 files. The hex viewer's text search is always UTF-8.
- **Virtual scrolling**: Only renders visible lines plus 5-line overscan. The mouse wheel and trackpad move the view from script rather than natively, so the lines a scroll exposes are drawn in the same frame and never flash blank (the scrollbar stays native). Scroll scaling for files exceeding browser's max element height (16M px).
- **Incremental line index**: Line positions are built by scanning for `0x0A` in chunks as they load. The `+` after the line count in the status bar indicates more lines may exist in unscanned chunks.

**Selection**:
- **Mouse drag**: Click and drag to select character ranges. Selection is character-granular (uses `caretRangeFromPoint`).
- **Double-click**: Selects the word under the cursor.
- **Shift+Click**: Extend selection from anchor to clicked position.
- **Ctrl+A**: Select entire file.
- **Auto-scroll**: Dragging near top/bottom edges (20px margin) auto-scrolls.
- **Escape**: Clears the selection. If there is no selection, closes the viewer.

**Copy** (Ctrl+C): Copies selected text to clipboard via the Rust backend (`copy_viewer_range`, decoded in the effective encoding). 10 MB copy size limit: a larger selection fails with an error rather than copying part of it.

**Select All** (Ctrl+A) selects to the end of the file, including the part not yet scanned for lines; nothing is read until it is copied. The status bar shows the selection's end as "end". For a file whose size isn't known up front (a `/proc` file, a pipe), it ends where the scan has reached.

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

### Table Mode (CSV/TSV)

A spreadsheet-style grid over delimited text. It streams like Text mode — the file loads in 128 KB chunks on demand, so a multi-GB CSV on S3 opens at once — and decodes in the encoding the Encoding menu picks (UTF-16 included; Excel's "Unicode Text" export is UTF-16 TSV).

- **Parsing**: RFC 4180, leniently: a `"` at the start of a field opens a quoted field that may hold delimiters and line breaks, with `""` for a quote; a stray quote mid-field is data, and text after a closing quote is kept (like Python's `csv`). `\r\n` and `\n` both end rows. Rows are found by a streaming scan of the raw chunks that carries quote state across chunk boundaries, extended as the view approaches the scanned end — so row counts show a `+` until the scan reaches the end of the file, and the scrollbar is estimated from the average row size meanwhile.
- **Table menu** (per viewer window): **Delimiter** — *Auto-detect (…)*, Comma, Semicolon, Tab, Pipe; **First Row Is Header** — *Auto-detect (Yes/No)*, Yes, No; **Quoted Fields** (on by default; off splits on the delimiter and line breaks only, keeping quotes as data). Auto-detection reads the first chunk: the delimiter is the one that splits its rows most consistently into more than one field (the extension breaks ties — `.tsv`/`.tab` → Tab, otherwise Comma); a header is recognized the way Python's `csv.Sniffer.has_header` does, by columns of numbers or fixed-width values under a label that isn't one.
- **Layout**: a header row (the first row's values, or spreadsheet letters A, B, … AA when there is no header) stays on top and a row-number gutter on the left while the grid scrolls both ways; only the visible rows and columns are rendered. Numeric cells are right-aligned, and a line break inside a cell shows as `↵`. Column widths start fitted to the first chunk's values (capped at 300 px); drag a header's right edge to resize, double-click it to fit the column to the visible rows (up to 480 px).
- **Selection**: a cell cursor with a rectangular range. The cursor starts on A1 (once the first data row is in; a table with no data rows — an empty file, a header on its own — has no cursor and its status shows `0 rows × N cols`). Click a cell, drag or Shift+click to extend; click a column header to select the column (drag or Shift+click for several), a row number for the row, the corner for everything. Keys: arrows move (Shift extends), Tab and Shift+Tab move right and left, Mod+arrows jump to the edge (Mod+Down scans to the end), Page Up/Down move a page, Home/End go to the first/last column, Mod+Home/End to the first/last cell; Escape shrinks the selection to the cursor, then closes the viewer.
- **Select mode**, for copying part of a cell: double-click a cell, or press **Enter** or **F2** (Google Sheets' and Excel's keys for entering a cell), and it becomes a read-only text box showing the full value with its real line breaks. The box grows over its neighbours to fit the value: wider up to 640 px, then taller as long lines wrap (visually only — a copy keeps the text as it is), up to 12 lines, then it scrolls. The value starts out selected; the caret keys move and extend the selection as in any text field, Home and End going to the start and end of the value (typing, pasting and deleting do nothing), Mod+A selects the cell's text again, and Mod+C (or Edit ▸ Copy, or the context menu) copies the selected text — the whole value when nothing is selected. Escape or Enter returns to the cell cursor, Tab and Shift+Tab to the next and previous cell; clicking elsewhere in the grid leaves it too, and so does scrolling the cell out of view. The status bar names the cell while select mode is on.
- **Copy** (Mod+C, Edit ▸ Copy): the selected cells as TSV, which pastes into Excel, Numbers and Google Sheets as cells — a field holding a tab, quote or line break is quoted. The context menu adds **Copy as CSV**. Copying whole columns includes their header. Up to 10 MB of the file per copy, like Text mode.
- **Search** (Mod+F) is the text viewer's search bar, run over the raw file on the backend (remote files included); a hit selects the cell it falls in (a hit in the header selects its column). **Go to Row** (Mod+G, Edit ▸ Go to Row) jumps to a data row.
- **Status bar**: path | Table | encoding | `Row r / n+ | Col C` | selection size | file size.

### Markdown Mode

Renders GitHub-flavored Markdown (tables, task lists, strikethrough, autolinks, footnotes) with `comrak`, styled after GitHub's and following the app's light/dark theme. Parsing runs off the UI thread, and a long document fills in from the top a few blocks per frame, so the window — and the file list beside Quick View — stays responsive while it does. Files over 4 MB open in Text mode instead.

- **Safety**: the rendered HTML, raw HTML in the document included, is parsed in Rust and kept only as far as an allowlist goes: ordinary text, list, table and heading elements, `<details>`, footnote sections and read-only task-list checkboxes, each with a few attributes (links, image sources and sizes, table spans and alignment, `id`, `title`). An element that isn't listed gives way to its text; scripts, styles, frames, embedded objects, form controls, SVG and MathML are dropped with their content, and no `style`, `class` or event-handler attribute survives. Links keep only `http(s):`, `mailto:` and relative targets, and images only `data:image/` and relative ones — nothing remote. The page receives the result as a tree of elements and text that it builds with the DOM API — never as markup to parse — inside a shadow root, which keeps the document's styles and ids from touching the viewer's own; the viewer's CSP forbids inline scripts and inline event handlers besides. The CSP also keeps remote content out: images load only from the file itself (relative paths, served through the viewer's file server — so they work in remote sessions and archives too) or `data:` URIs, so README badges and tracking pixels don't load. A relative image is resolved (in Rust, against the document's folder) and kept only while it stays on the document's own volume; so is a relative link followed into the viewer. On Windows a path off the volume — root-relative like `/server/share/x.png`, or climbing past the drive with `..` — would name a network share, and merely loading it would hand the user's Windows credentials to that server. On macOS, Linux and every network filesystem there is one root, so nothing changes there.
- **Links**: `http(s):` and `mailto:` links open in the system browser; a relative link opens that file in the same viewer window (in whatever mode suits it); `#anchor` links scroll to the heading with GitHub's anchor for it, to a footnote, or to an element the document's raw HTML gives an `id` (prefixed `user-content-`, as on GitHub). Links with any other scheme do nothing — the app's URL opener refuses anything but web and mail links, since it would otherwise launch local files and programs.
- The document scrolls with the arrow and page keys, and the viewer's keys (Escape, F3, …) work as in any mode; text selection and Mod+C work as in a web page. Right-clicking the document offers Copy (with a selection) and Select All, in the viewer window and in Quick View alike. It follows theme changes live.

### Hex Mode

- Classic hex dump layout: offset column (8 hex digits) | 16 hex bytes (grouped 8+8 with a gap) | ASCII representation.
- Non-printable bytes shown as `.` in the ASCII column. Printable range: 0x20–0x7E.
- **Virtual scrolling** with max scroll height clamping (prevents browser rendering issues with very tall elements).
- **On-demand chunk loading**: 128 KB chunks with LRU cache (32 chunks). Preloads chunks for the visible viewport and overscan area.
- **Mouse wheel**: moves the view from script, as in Text mode, so rows a scroll exposes are drawn in the same frame. Handles both pixel-mode and line-mode scroll deltas.

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

### Structure Mode

The file dissected into a tree of its fields, Wireshark-style: an executable's headers, sections and imports, an archive's members, an image's chunks, a database's pages, a packet capture's frames down to their TCP headers. Powered by [fillyfoal](https://github.com/tibordp/fillyfoal), a dissector for some 1,600 formats, from executables, archives, disk images and filesystems to network captures, media, documents, databases, fonts, game assets, ROMs and scientific data. Embedded content is dissected in turn (a PNG inside a ZIP inside an installer), and compressed or fragmented data is decoded or reassembled into a stream of its own.

- **Depth varies by format.** Common formats are taken apart field by field, down to their records, tables and payloads; for many of the more obscure ones the dissection is little more than recognising the format by its magic bytes and decoding its header.
- **For information, not evidence.** The dissection is meant for understanding a file and may be incomplete or wrong; don't rely on it for forensics or anything else that needs an exact account of a file's contents.
- **Lazy**: only what is expanded is read. Opening an executable reads a few kilobytes; a section's contents, a member's data or a resource is read when its node is expanded. On S3, in an archive or in a remote session, only those bytes cross the connection. Long lists (an archive's members, a table's rows) come 200 at a time, with a **Show 200 more** line under them.
- **Progress and stopping**: work runs in small steps, and requests take turns between them. A long expansion (the members of a big `.tar.gz`) never holds up expanding something else, the hex pane, or the details panel. A field whose children are being read shows a spinner in place of its chevron, and its children appear under it as they're found rather than all at once at the end. The status bar names what's loading and how far it has got ("Loading “Central Directory” 42% and 1 more · Esc to stop"), with a **Stop** button. The measure is what the dissector reports where it can say; else, while it waits on a compressed stream decoding up to where it reads, how far that has got, and the status bar names the codec ("Loading “Packet 0” · decoding zstd 61%"); else how far its reads have got through the field. Like a pane's loading indicators, none of these appear until the work has taken 200 ms, so quick expansions don't flicker. **Escape** stops everything in flight before it does anything else (clear the filter, close the viewer); collapsing a field that's loading stops just that one; leaving the viewer stops it all. Stopping keeps what was produced: the tree shows the children found so far with a **Stopped — continue** line under them, which carries on from where it stopped rather than starting over. Reading a decompressed stream far from what's been decoded (the hex pane, copying bytes) is stepped and stoppable the same way, with "Decoding zstd 37%" in the status bar (the innermost stream decoding, and how far it is to the bytes wanted), and **Save Bytes As…** shows "Saving 42%". A compressed stream whose size nothing records (a `.tar.bz2`, or a `.tar.zst` written through a pipe) is decoded on demand too, so its first entries show without decoding the rest. Its size reads "Size unknown" (in the byte-space bar, and for a field running to its end) until decoding reaches the end, e.g. when the hex pane reads its last bytes.
- **Layout**: the tree on the left — each field's name, its value (numbers, text and enum or flag names, the raw number dimmed beside a name), a dimmed summary, a badge for problems, and its offset. On the right, the bytes the selected field lives in, its span highlighted (and, for a pointer field, what it points to, in another tint); above them the byte space they're in — the file, or e.g. "inflate of 0x2A0 + 0x1F4" for a decompressed stream. Below, the field's details: value and raw number, summary, the dissector's description of the field, its bytes, what it points to, how many children it has, and its diagnostics (truncated, malformed, unsupported, limit, warning, note) with where they occur. In a window narrower than 900px (typically Quick View) the bytes and details go under the tree instead. **Mod+B**, the **Bytes** button in the status bar, or the context menu's **Bytes Panel** hides and shows them, leaving the tree the whole window. A file that's cut short or malformed keeps whatever was dissected, with the problem attached where it occurred.
- **Keyboard**: in the tree, Up/Down, PgUp/PgDn and Home/End move the selection; Right expands a field, or moves into it when expanded; Left collapses it, or moves to its parent; Enter or Space toggles it, or loads more on a **Show more** line. **Tab** moves between the tree and the bytes (when shown). In the bytes, the arrow keys, PgUp/PgDn and Home/End (Mod+Home/End: the start/end of the stream) move a byte cursor, Shift extends a selection, and **Enter** selects the innermost field holding the byte under the cursor — expanding the tree down to it. Clicking a byte does the same. The way down follows decoded streams (a byte of a decompressed member is found through the node holding the compressed data) and parts listed away from their bytes (a ZIP member's data, under the Central Directory), searching a few thousand nodes at most where no child holds the byte; it shows "Working…" and stops on Escape like any load. A byte no field of its stream holds leaves the selection, and the stream shown, where they are.
- **Copying and saving**: **Mod+C** copies the selected field as `Name: value — summary` (or, in the bytes, the selected bytes as hex); **Mod+Shift+C** just its value. The context menu (also from the keyboard's menu key) adds **Copy Tree**, the tree from the selected field down as shown (`▾`/`▸` for expanded and collapsed fields, two spaces a level, each line's bytes after it as `[0xoffset+0xlength]`, problems below it as `! …`, and `… (n shown of m)` where a list continues — a filtered tree copies filtered), and **Copy Bytes ›** as hex, text (printable ASCII) or Base64 (Copy Tree and Copy Bytes as Hex can be bound in the Keybindings tab). **Save Bytes As…** (**Mod+S**) writes the field's bytes — or, in the bytes pane, the selected bytes — to a file chosen in the system save dialog, suggesting the name of the file the field holds (a member's or a decompressed stream's) or the field's own; it reads a step at a time, so a large decompressed member saves without loading at once, shows how far it has got ("Saving 42%") and stops on Escape, leaving no partial file. In a remote session the file lands on this machine. The menu then offers **Go to Target** (Mod+Enter) for a field that points somewhere and **Go to Offset** (Mod+G), which takes a hex offset and selects the field there. Mod+A selects the selected field's bytes. The bytes pane has its own context menu: **Copy as Hex**, **Copy as Text** (the selected bytes, else the selected field's) and **Go to Offset**.
- **Encrypted content** (an encrypted archive's entries, a password-protected document or key file): a bar at the bottom asks for the password, naming what it unlocks. **Enter** unlocks (an empty password counts, since some formats default to one), **Escape** skips, leaving the encrypted content as an unsupported field. A wrong password asks again, up to three times. A field waiting for one carries a lock; Enter on it asks again. One answer serves everything it unlocks, e.g. every entry of an archive. Answers last only as long as the dissection and are never stored. Quick View doesn't ask: a locked field stays locked until the file is opened in the viewer window.
- **Filter** (**/**, numpad **/** too): a box floats in the tree's corner, as in a pane, and narrows the tree to the lines matching a case-insensitive regex (an invalid one hides nothing) — name, value and summary alike — together with the way down to each; non-matching lines are hidden, not flattened away. Only what's expanded is searched: a collapsed node is matched by its own line, never opened to look inside, and a **Show more** line stays under a shown parent since what it hasn't loaded wasn't searched. While lines are hidden the tree carries the filtered tint (`--color-bg-filtered`) and the status bar counts the matches. A selection the pattern no longer matches moves to the first match. Up/Down and PgUp/PgDn in the box move the selection; Enter or Tab hands the keys to the tree with the filter kept (**/** returns to the box); Escape, in the box or the tree, clears it. Locating a field (clicking a byte, Enter in the bytes, Go to Offset or Target) clears it too, since the field found may be hidden.
- **Dissect As** (**Mod+D**, or the tree's context menu): a palette of every format, those that list the file's extension first, filtered by title, name or extension as you type. The chosen format replaces identification, for content the dissector doesn't recognise or recognises as the wrong thing (a Guitar Pro `.gpx` is not a GPS track); on data that doesn't match it, the tree shows what could be parsed and where it stopped. **Identify automatically** goes back.
  - **What it applies to**: the file, or a file within it — an archive member's data, decompressed data, an embedded resource: the selected node if it holds one, or its one child that does (an archive member holds its data beside its headers, so selecting the member is enough). Any other node does nothing but say so in the status bar; there is no reaching up to an enclosing file. The context menu works the target out as it opens: the item names it ("Dissect “song.gpx” As…") and is disabled on a node without one. Whether a node holds a file is known once it has been expanded (Mod+D on an unexpanded one finds out). The palette names it ("Dissect “song.gpx” as…"), and its suggestions follow that name's extension.
  - **Where it shows**: once expanded, a node holding a file carries its format's short name as a quiet tag ("png", "zip", "?" when not recognised; the full name on hover), and one dissected as a chosen format carries an accent "as …" tag instead. The field details list its **Format** (identified, chosen, or not recognised). For the file itself the status bar reads e.g. "Structure as Guitar Pro 6 score".
  - **How long it lasts**: every choice, the file's own and those inside it, lasts for the viewer window's file: kept by the position of its node in the tree, and replayed when the file is dissected again, e.g. after switching to another mode and back. Choosing a format for content drops the choices made inside it. Quick View always identifies.
- **Detection**: a file whose association or MIME type would leave it in Hex mode opens in Structure mode when the dissector recognises it from its first 66 KiB and last 1 KiB. A [file association](#file-associations) can pick it explicitly with `viewer = "dissect"`.

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
- Links in a document open in the default browser or mail client. No Newt window ever navigates away from Newt itself: a navigation to anywhere else (a link, a script) is cancelled, web and mail links are handed to the system, and requests for new windows are refused.
- **Toolbar**: Previous/Next page buttons, page display ("1 / 42"), zoom in/out/fit buttons, zoom percentage.
- **Keyboard**: `=` zoom in, `-` zoom out, `0` reset to fit (the shared, rebindable `viewer_zoom_*` commands — Mod+= and friends stay webview zoom).
- **Escape**: Close viewer (via window-level menu accelerator).

**Status bar**: `path/to/document.pdf | PDF | 2.3 MB`

### Quick View (Mod+Shift+F3)

**Toggle Quick View** (Mod+Shift+F3, palette category View) turns the right half of the main window into a preview of the file under the active pane's cursor. The left half shows the active pane — both panes stay loaded there, and Tab (or the pane-switch button at the start of the pane's header, whose two-pane glyph fills the side on screen) switches which one is shown. The preview follows the cursor once it comes to rest (~80 ms, so holding an arrow key doesn't load every file it passes), and the file on screen stays there until the next one is ready, so moving through a folder never flashes an empty "Loading…" pane. In a search-results pane it previews the matched file itself, under its real path. Folders, `..` and empty listings show a "No preview" placeholder. Nothing special happens on remote filesystems: text, hex and media stream as in the viewer window, while images and PDFs load whole.

The preview has a bar of its own, matching the pane's location bar: the file's name, then the viewer window's menus as dropdowns — the mode (auto-detected per file), the encoding for text, table and Markdown, and the table options (delimiter, header row, quoted fields) in table mode. A choice made there lasts until the cursor moves on. At the right end, an open button shows the file in the full viewer window (as F3; Quick View stays on) and a close button turns Quick View off.

The preview is for the mouse; the keyboard stays with the file list. It doesn't search or take keyboard commands — F3 opens the full viewer window for that. Clicking into it (to scroll, or to select text) is fine: the list keeps its cursor, and the first key pressed goes to the list as if it had been pressed there, typing into quick search included. The copy shortcut (Copy Path to Clipboard, Mod+C by default) copies what's selected in the preview — text, a hex range, an image region, table cells — wherever keyboard focus is; with nothing selected there it copies the path as usual. A caret, a single hex byte or the table's cursor cell alone doesn't count as a selection. Relative links in previewed Markdown open the target in a viewer window. Maximize shows the active pane alone; F5/F6 still target the other pane, hidden while Quick View is on. The state is per window and not persisted.

### Viewer Menu Bar

- **App menu** (macOS only): Quit Newt (Cmd+Q).
- **File**: Close (Escape, or Mod+W — the rebindable `viewer_close` command)
- **Edit** (Text, Hex, Table and Structure modes): Copy, Select All, separator, Go to Line / Go to Offset
- **View**: Text / Hex / Image / Audio / Video / PDF (radio buttons — one always checked)
- **Encoding** (Text mode only): Auto-detect (detected name), separator, then the encoding catalogue as per-script submenus (radio buttons, same one-checked convention as View).

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

The language comes from [file associations](#file-associations); the built-in ones are below, and an association with a `language` overrides them.

**By file extension** (prioritized):

| Extensions | Language |
|-----------|----------|
| `.js`, `.mjs`, `.cjs`, `.jsx` | JavaScript |
| `.ts`, `.tsx`, `.mts`, `.cts` | TypeScript |
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
| `.scss` | SCSS |
| `.less` | Less |
| `.json`, `.jsonc` | JSON |
| `.yaml`, `.yml` | YAML |
| `.toml`, `.ini` | INI/TOML |
| `.xml`, `.svg` | XML |
| `.md`, `.mdx` | Markdown |
| `.sql` | SQL |
| `.graphql`, `.gql` | GraphQL |
| `.dockerfile` | Dockerfile |
| `.tf` | HCL (Terraform) |

**By filename** (case-insensitive): `Dockerfile` → Dockerfile.

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
- **Encoding**: UTF-8, with a byte-order mark kept if the file has one. A file that isn't valid UTF-8 opens read-only ("Read-only: not UTF-8" in the status bar), since saving it would replace every byte that didn't decode.

### Saving

- **Ctrl+S / Cmd+S**: Save the file.
- **Changed on disk**: if the file's size or modification time differs from when it was opened or last saved — another program wrote to it meanwhile — the save asks before writing over that.
- Encodes content to UTF-8 and swaps the new contents in whole where the filesystem can promise it, so a full disk, a dropped connection or a crash mid-save leaves the old file intact:
  - **Local** (macOS, Linux): written to a temporary file beside it — given the file's owner, extended attributes, mode and ACL — then renamed over it; a symlink is saved through to its target and stays a link. A file with other hard links, one owned by someone the new file couldn't be given to, one in a directory that won't take a new entry, and anything but a regular file are written in place instead, since renaming over them would change what they are. Windows writes in place for now.
  - **SFTP**: the same through the server's posix-rename (OpenSSH has it); servers without it, and files whose new copy would have another owner, are written in place. SFTP reports no link count, so a hard-linked file on an SFTP server comes out of the save as a file of its own.
  - **S3**: an upload replaces an object whole anyway; a save keeps its metadata (Content-Type, Cache-Control, user metadata…), storage class and tags, and puts its ACL grants back once it lands. If the tags or the ACL can't be read, the save is a plain upload.
- Status bar briefly shows "Saving..." during the write.
- After save, the dirty indicator clears and the file size updates.

### Dirty State and Closing

- **Dirty indicator**: Window title shows `* filename - Editor` when unsaved.
- **Escape**: Closes the editor window (confirmation first if unsaved changes) — but only when Monaco doesn't consume it: dismissing intellisense/find/palette widgets or canceling a selection spends the Escape, and only an unclaimed press closes. Save dispatches through the rebindable `editor_save` command; the resolved key also rides Monaco's Save action (re-registered on rebind — `addAction` commands are editor-scoped, so global keybinding rules can't target them) so the key works natively in-editor and Monaco's palette shows the real shortcut.
- **Close with unsaved changes**: A warning confirmation dialog appears: "You have unsaved changes. Close without saving?" User must confirm to discard changes.
- **Close without unsaved changes**: Window closes immediately.

### Editor Menu Bar

- **App menu** (macOS only): Quit Newt (Cmd+Q).
- **File**: Save, Close (Mod+W). On macOS the Save accelerator mirrors the resolved `editor_save` binding and menus rebuild on preference changes (the menu bar claims the key ahead of the webview, so a stale accelerator would shadow a rebind); other platforms register no Save accelerator — the webview handles the key.
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
- **Maximize button** (right end of the tab bar): Focuses the terminal and maximizes the panel, or restores the split layout when it is maximized — see "Maximized Layout".
- **Context menus**: every terminal menu ends with New Terminal, Maximize Terminal (Restore Split Layout when maximized) and Hide Terminal Panel. Right-clicking a tab adds Close Terminal and Close Other Terminals for that tab; right-clicking inside a terminal adds Copy (with a selection), Paste, Select All, Clear (the scrollback, keeping the prompt) and Close Terminal. Paste reads the clipboard through the host, so the webview never asks for clipboard permission, and goes in as a bracketed paste where the shell asked for one. While the program in the terminal is reporting the mouse (tmux, vim with `mouse=a`, htop), a right-click goes to the program instead; Shift+right-click opens the menu regardless, as Shift+drag selects text regardless. Right-clicking the empty part of the tab bar offers just the panel items.

A hidden panel — toggled off, or behind a maximized pane — leaves the shell's window size as it was, so full-screen programs aren't squeezed to a single row while out of view.

All terminals are always mounted in the DOM but only the active one is visible. This preserves terminal state (scrollback, running processes) when switching tabs.

### Terminal Emulation

- **xterm.js**: Full VT100/ANSI terminal emulation with 256-color and truecolor support.
- **Scrollback**: `terminal.scrollback` lines (default 1000).
- **Font**: `terminal.font_family` (a CSS font-family list, default Menlo, Monaco, Courier New, monospace) at `terminal.font_size` (default 12), 1.2 line height. Changes in Settings apply to open terminals at once.
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
- **Paste**: Ctrl+Shift+V on Linux/Windows, Cmd+V on macOS (through the Edit menu — see below), or Paste in the terminal's context menu. The text goes to the program as one paste, bracketed where the shell asked for one; there is no confirmation for multi-line text. Ctrl+V reaches the program as ^V, and Mod+V's Paste Path from Clipboard applies only while a pane has focus.
- **macOS Edit menu**: Main, viewer, and editor windows include a native Edit submenu with Undo / Redo / Cut / Copy / Paste / Select All entries. Without this menu, macOS silently swallows Cmd+V/C/X/A before they reach the webview, so this is required for clipboard shortcuts to work in any text input.
- **Input assist disabled globally**: `index.html` sets `autocorrect="off" autocapitalize="off" spellcheck="false"` on `<html>`, which inherits to every `<input>` and `<textarea>`. macOS WebKit otherwise applies these by default and silently mangles typed paths, regex patterns, and shell commands; Linux WebKit doesn't, so this normalises behaviour across platforms.
- **Selection**: Highlight text with the mouse to select. Text is selectable by default.

### Terminal Lifecycle

- **Running**: Full interactivity. Input goes to the shell, output is displayed.
- **Defunct/Exited**: When the shell process exits:
  - If `behavior.keep_terminal_open` is **true** (default): The tab stays open, showing a dimmed message: `[Process exited with code X. Press Enter to close.]` (or signal name if killed). User presses Enter to close the tab.
  - If `behavior.keep_terminal_open` is **false**: The tab is automatically removed. If it was the active terminal, the next terminal becomes active. If no terminals remain, the terminal panel hides.

### Terminal in Remote/Elevated Mode

Terminals in remote sessions run on the remote host. The PTY is allocated by the agent process. Terminal I/O is forwarded over the RPC protocol: output streams continuously rather than a round trip per read, so a command printing a lot over a high-latency link isn't throttled by the latency, and keystrokes and resizes share one ordered input stream. From the user's perspective, the terminal behaves identically to local mode.

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
| Mod+F11 | Toggle maximized layout (terminal alone ↔ split) |
| Mod+Enter | Open focused file/directory in a new terminal (sets working directory) |
| Enter (in defunct terminal) | Close the terminal tab |

A key typed in a terminal goes to the program running there, with two exceptions that go to Newt: the keys of the terminal and focus commands above (toggle panel, new terminal, next/previous terminal, focus panes/terminal, maximize), at whatever they're bound to, and any binding scoped to `when = "terminal_focused"`. Everything else stays the program's even when a Newt command has it — Ctrl+R, Ctrl+N, Ctrl+P or F10 mean something to a shell or a TUI; Mod+C copies the selection (Ctrl+Shift+C off macOS).

### Shell Integration (`newt` CLI)

Every spawned terminal (and every user command, both terminal- and operation-mode) gets a `newt` CLI that remote-controls the owning session (design: `design_docs/DESIGN_SHELL_INTEGRATION.md`). Gated by `behavior.shell_integration` (default on; currently gates local sessions — remote agents always provide it).

**Plumbing**: the PTY-owning side (host in local sessions, agent in remote/elevated) creates one fresh 0700 temp dir per session holding the control endpoint and the shim, and injects `NEWT_SHELL_SOCK` (socket path / pipe name), `NEWT_TERMINAL` (handle), and a PATH prepend of that dir into every spawned child. Unix: a Unix-domain socket plus a `newt` symlink to the agent binary; Windows: a named pipe (`\\.\pipe\newt-shell-…`) and a generated `newt.cmd` (which sets `NEWT_CLI=1`, since a .cmd shim can't control argv[0]). The agent path written into that shim has its `\\?\` verbatim prefix undone, because cmd.exe's parser rejects verbatim paths: Tauri canonicalises its own exe to resolve symlinks and `fs::canonicalize` on Windows always returns the `\\?\` form, so the agent located relative to the resource dir inherits it. Verbatim UNC is unwrapped to a real share (`\\?\UNC\server\share` → `\\server\share`) rather than trimmed, which would leave a relative path. Every other consumer of that path is a Win32 API and takes the verbatim form happily — only the shell needs this, as `wsl_launch::to_wsl_path` already found. The agent binary enters CLI mode only when invoked *as* `newt` (argv[0] / `NEWT_CLI`) with the env var set — `newt-agent` invoked by its own name always behaves as the agent. The main `newt` executable answers the same modality when `NEWT_SHELL_SOCK` is set and argv[1] is a known verb, so an already-on-PATH `newt` works on Linux without the shim.

**Protocol**: HTTP/1.1 over the socket/pipe (hyper connection-level; no axum in the shared code), deliberately version-tolerant (unknown route → 404, never a panic) because shells outlive app upgrades. On Unix, `curl --unix-socket "$NEWT_SHELL_SOCK" http://newt/v1/panes/active/cwd` works. In remote sessions the agent forwards control-plane verbs to the host over `API_HOST_SHELL_CONTROL`; `cat` bytes stream from the agent-side VfsRegistry without a host round-trip.

**Verbs** (`--pane active|other|left|right`, a clap global flag, default active; exit codes 0 ok / 1 usage or request error / 2 no session). The CLI is a clap derive `Parser` in `shell_control/cli.rs`: `newt --help` carries the path-resolution rules, exit codes and examples, every verb has a long `--help` and a short `-h`, and the main executable's CLI-mode guard derives its verb list from the clap command so the two can't drift.

| Verb | Behavior |
|------|----------|
| `newt pwd` | Print the pane's directory (display path — native on the root VFS, `s3://…` etc. otherwise). |
| `newt cd [path]` | Navigate the pane (non-strict: a leaf path lands on the parent with the entry focused). Bare `newt cd` syncs the pane to the shell's cwd. Relative paths resolve against the shell's cwd. |
| `newt focus <path>` | Alias for the leaf-focus form of `cd`. |
| `newt cat <path>` | Stream a file to stdout through the session VFS. Relative paths resolve against the *pane* — works inside archives/S3 mounts. |
| `newt view <path>` / `newt edit <path>` | Open the built-in viewer / editor (pane-relative resolution like `cat`). |
| `newt cp <src>… <dest>` / `newt mv` | Enqueue a copy/move through the operations framework (fire-and-forget; prints the operation id). Multiple sources need an existing dir; single source to a non-existent leaf copies/moves under the new name (a same-directory `mv` is a plain rename). Trailing slash asserts directory-ness; existence checks go through the session VFS. |
| `newt select [pattern…] [--add\|--remove]` | Set the pane's selection. With patterns: the Select by Pattern dialog's syntax (case-insensitive glob, `/` prefix for regex) against visible entries, matches of several patterns unioned — so a shell-expanded unquoted `*.c` works, each file name being a literal glob. Without: names read from stdin, one per line — a bare name is a pane entry as listed (works on S3/archives; `ls \| newt select`), a name containing a separator resolves against the shell's cwd and only counts if it lands in the pane's directory (so `git ls-files \| newt select` picks the top-level tracked files of the pane's directory and ignores nested ones). Replaces the selection by default; `--add`/`--remove` adjust it instead. Prints "N matched". |
| `newt cmd [id]` | Tier-1 mechanical dispatch of any command-registry id (same ids as `[[bind]]`/palette): closes any open modal first, exactly like a keybinding. Bare `newt cmd` lists ids + names (including user commands). Excluded: `new_window`, `quit`, `open_elevated`, `connect_wsl` (non-uniform signatures). In a remote session it needs [access to this host](#access-to-this-host), since commands reach the clipboard and local apps; without it, it's refused. |

Path arguments resolve exactly like the Go To dialog (`resolve_display_path`: mounted-VFS URLs, native absolutes, `~` expansion on the session side), so `newt cd "$(newt pwd)"` round-trips on any pane. A URL matching no mounted VFS is an error (no auto-mounting).

---

## 8. VFS (Virtual Filesystem) Support

All filesystem access goes through trait abstractions. Multiple VFS types can be mounted simultaneously and accessed independently from either pane.

**File identity** (`Vfs::same_file`): whether two paths denote the same file is a question only the filesystem can answer — case folding, Unicode normalization, Windows short names and trailing-dot stripping, hardlinks and bind mounts all let distinct strings name one file, and the folding rules can't be reproduced on our side (NTFS's uppercase table is frozen per volume, APFS has its own, ext4 casefold is per *directory*). So there is no `is_case_sensitive` flag anywhere; the verb asks instead. Local compares `(st_dev, st_ino)` on Unix and volume serial + 128-bit `FILE_ID_INFO` on Windows (falling back to the legacy file index on network redirectors); SFTP compares the server's own `realpath` of each side, which also folds the way *that* server's filesystem does — except where either side is a symlink, since `realpath` sees through links: a link is the same file only as itself (same canonical directory, same name), never as its target, so moving a link onto the file it points at prompts like any conflict instead of replacing the file. The default is exact path equality, which is correct rather than merely conservative for byte-keyed namespaces (S3, archives, search results). Consumed by the copy/move self-destination check and the rename re-spelling exemption — see File Operations.

**Read primitives**: three shapes on the `Vfs` trait — sequential streams (`open_read_async`), stateless one-shot ranges (`read_range`), and positioned-read handles (`open_read_at` → `VfsRandomReader::read_at`), held open for a whole read session instead of paying an open per chunk. The split follows chunk count: a caller that will issue a single chunk uses `read_range` (one round trip, nothing to reap); a multi-chunk loop opens a handle. Handle consumers: the archive/disc engine loops, `find_in_file`, `newt cat` streaming, media streaming through the file server (ranges longer than one chunk), and viewer range-copy for selections past one chunk. The F3 viewer's own text/hex chunk reads deliberately stay on `read_range` — the viewer should observe live changes to the file and must not hold the file open (locking it on Windows) just because a viewer window is up. The handle pins the file's identity where the backend can, so a file replaced mid-session fails or keeps serving the original rather than mixing bytes across generations: an open fd on local (pread), an SFTP file handle (protocol-native offset reads, size stat'd once at open), a pinned ETag on S3 (reads after the first carry `If-Match`; a concurrent overwrite fails the read with "object changed while reading"). The same surface is exposed on the shell-facing `Filesystem` trait (`VfsPath`-addressed, hairpin-diverted like `read_range`), which also carries the file-content verbs (`read_range`/`read_file`/`write_file`/`find_in_file`/details/property sheet) — there is no separate reader trait. Over RPC a handle is a server-held session (`OPEN_READ_AT` mints a `StreamId`, `READ_AT` is request/response per chunk, `READ_AT_CLOSE` on proxy drop reaps it) — the same open/read/close triple exists at both the VFS verb level and the Filesystem verb level.

**Auto-refresh**: Panes auto-refresh on window focus for local and remote VFS types, and when one of Newt's own operations finishes (or stops) having changed the folder they show — a copy or move into or out of it, a rename, delete, pack or property change in it; a user command refreshes both panes — rather than waiting on file watching, which can miss changes made in quick succession. Auto-refresh is disabled for S3, SFTP, and archive VFS types (where listing is expensive). Manual refresh is always available via Mod+R.

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

**Operations supported**: Read, write, create directory, delete, copy within the same S3 bucket, touch, rename (via the operation's copy+delete fallback — server-side CopyObject per object, works on prefixes too), extended properties (user metadata, storage class, Content-Type/Cache-Control, ACL grants + canned ACL — see Properties dialog). Server-side copies (copy/move/rename within S3) carry over user metadata and system headers (CopyObject default), and explicitly re-apply the source's storage class and any non-default ACL — a failed ACL restore is logged and the copy still succeeds, since the streaming fallback couldn't restore it either.

**Operations NOT supported**: Hard link, symlink, Unix permissions, filesystem stats, trash (plain Delete prompts for permanent deletion).

**Uploads**: an object up to 10 MB is one PutObject. A larger one becomes a multipart upload, created only when its first part fills, so small objects never pay for one. Part size comes from the writer's `size_hint` so the object fits S3's 10,000 parts (50 TB objects take 5 GiB parts); without a hint, or past it, parts double every 1,000 (about 10 TB). A part past 32 MiB spills to a temp file and uploads from it, which also lets the SDK re-read it to retry. A writer dropped mid-upload aborts the multipart upload.

**Conditional writes**: `create_new` puts `If-None-Match: *` on whichever request completes the object (PutObject, CompleteMultipartUpload, CopyObject). S3 evaluates it when the write completes, after the body is sent, so a write offers `create_new` only when its size hint fits one PutObject; larger or unhinted writes answer `NotSupported` and the copy looks first. A refusal is 412, or 409 `ConditionalRequestConflict` when a concurrent delete won; both map to `AlreadyExists`. The condition needs only `s3:PutObject`, but the look before a write that can't refuse is a HeadObject, which needs `s3:GetObject`: credentials that can list and put but not get (a write-only backup role) copy objects up to 10 MB on AWS, while larger ones, and any on a custom endpoint, fail the look with 403 and can only be skipped. Directory buckets (`--x-s3`) take it on PutObject and CompleteMultipartUpload, not CopyObject. A mount whose endpoint isn't AWS (from the mount's Endpoint URL, `AWS_ENDPOINT_URL[_S3]` or the profile, resolved as the SDK does) never offers `create_new`, since a store that ignores the header overwrites silently; there `touch` checks with HeadObject before its conditional put, rather than truncating an existing object.

**Errors**: SDK failures take their kind from the HTTP status: 404 is `NotFound` (a listing of a deleted bucket falls back to its parent), 403 `PermissionDenied`, 412 and a conditional 409 `AlreadyExists`.

**Copy attributes**: a same-bucket copy keeps the source's storage class unless the dialog picks one. User metadata, content headers, tags and access grants carry over only with the matching "Preserve object …" toggle; the canned-ACL selector, when set, replaces grants outright.

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

**Operations NOT supported**: Copy within SFTP (cross-file copy goes through the host), filesystem stats, trash (plain Delete prompts for permanent deletion), `create_new` writes and no-replace renames (copy and move look before writing). v3 has no "file exists" status, so a rename refused by a server without `posix-rename` is classified by a stat of the target and reports `AlreadyExists`, which lets an approved overwrite clear the target and retry.

**Symlink handling**: Reads symlink targets for display, stats targets to determine if they're directories.

**Display path**: `sftp://hostname/path/to/file`

**MIME detection**: Reads the first 8 KB of a file via `read_range()` and uses MIME type detection.

**Lifecycle**: SSH process is killed when the SFTP VFS is unmounted.

### Archives (Read-Only)

Mount and browse archive files as virtual read-only filesystems.

Members whose path climbs out of the archive (`../../.bashrc`, the "zip slip" trick) are left out of the listing, so copying a tree out can't write above its destination. A name the archive mentions more than once — repeated, or once as a member and once as the folder later members imply (`d` a symlink, then `d/x`) — is listed once, as its last mention makes it: a mention by a child makes it a folder. So `d` (a link), `d/x` lists a folder `d` holding `x`; `d/x`, `d` (a link) lists the link, with `x` out of reach and left out; `d/a`, `d` (a link), `d/b` lists a folder holding both. Copying out therefore never writes through a link an archive planted where a folder is (a symlink `d -> ~/.ssh` followed by `d/authorized_keys`). The same rule applies to tar, zip and 7z. Every member is still there in the archive's [Structure mode](#structure-mode) (F3 on the archive file), shadowed ones included, and its bytes can be saved from there.

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
| ar (static libraries, Debian packages) | `.a`, `.ar`, `.deb` — plain or compressed, detected by content. Windows `.lib` is ar too but isn't claimed, since the extension also names plain-text library files |
| ZIP | `.zip`, `.jar`, `.war`, `.ear`, `.apk`, `.ipa` |
| 7z | `.7z` |
| Bare compressed file | `.gz`, `.bz2`, `.xz`, `.zst`, `.zstd` (not on a tar or cpio name) |

**Auto-detection**: Pressing Enter on a file with a recognized archive extension mounts it automatically and navigates into the archive root (instead of opening the file). The extensions above are built-in [file associations](#file-associations); an association can browse other names as any of these formats, or open an archive in its application instead, and Browse Into mounts any file by what its first bytes say.

**TAR indexing** (streaming/incremental):
- Index is built by scanning the archive stream. Files appear incrementally in the UI as indexing progresses — you can browse partial results while the rest of the archive is still being indexed.
- Periodic snapshots every 200ms update the file list.
- Indexing runs while something is using the archive — a pane listing inside it, or a read in flight — and pauses when nothing is. Navigating out of the archive stops it; entering it again, or reading from it, continues from exactly where it stopped, with nothing redone. Moving between folders inside the archive keeps it going. A paused archive keeps its decoder state (for zstd and xz, typically the compression window — a few MB) until it's unmounted.
- Members sharing a path (two `foo.o` in a static library, a tar appended to with `tar -r`) list as one entry: the last one, which is also what reads return.
- An archive cut off inside a header or a member still mounts, compressed or not — a partially downloaded `.tar.gz` included: everything before the cut lists and reads, the listing carries the partial badge ("archive truncated"), and the member the cut lands in is listed but fails its read. A compressed stream that is corrupt rather than cut short still fails to mount with the decompressor's error. Only the first gzip member, bzip2 stream or xz stream of a concatenated file is read.

**Bare compressed files** (`whatever.gz`): mount as a one-entry filesystem whose entry is the archive's name without its suffix (`notes.txt.gz` → `notes.txt`), with no mode, owner or timestamps — the containers carry nothing worth showing. Nothing but a full pass tells the unpacked length, so the stream is indexed in the background at first use like a tar: the listing lands at once without a size and refreshes with it when indexing completes (indexing pauses like a tar's when nothing is using the file, and continues where it stopped on the next listing or read), and reads wait for the complete index. Reads then go through the same reader pool and stream driver as tar and 7z. The checkpoints that make reads random-access (as in a compressed tar) share a 256 MiB budget spread evenly over the whole stream, however far it turns out to unpack (when they outgrow it, every other one is dropped), so a multi-terabyte disk image indexes in bounded memory; the price is that a read into such a stream decodes forward from its nearest checkpoint, which may lie a few gigabytes back.

**ZIP indexing** (one-shot, sans-IO): the in-tree `newt_archive::zip` reader (disc-image architecture, not the tar one — no external zip crate, no `spawn_blocking`/`block_on` anywhere) fetches the central directory at EOF in bounded 1 MiB slices with determinate progress, yielding the complete entry table; listings and `file_details` afterwards cost zero upstream reads. Reads are random-access: stored entries map 1:1 onto upstream range reads (ISO-extent style), compressed entries stream through a resumable decrypt→decompress cursor that gets parked per archive and resumed by the next sequential range read — the viewer's chunk fan-out costs one decompression pass total, not one per chunk. Dropping a read future drops the in-flight upstream read (async-native cancellation).

- **Methods**: stored, deflate, deflate64, bzip2, lzma, zstd, xz. PPMd and pre-1993 methods (shrink/implode/reduce) list fine and report a clean unsupported error on read.
- **Container gamut**: zip64 (large files, many entries), self-extracting stubs and prepended data (base-offset skew derived from the EOCD position), data-descriptor entries, implicit directories, duplicate entries (last wins), archive comments, split archives detected and refused cleanly.
- **Names**: UTF-8 flag honored, unicode-path extra field (0x7075) with CRC staleness guard, CP437 fallback for legacy names, `\`-separated paths normalized.
- **Metadata**: unix mode/symlinks from external attributes, uid/gid from the Info-ZIP extra (0x7875), timestamps from the extended-timestamp (0x5455) and NTFS (0x000a) extras at full precision with DOS-time fallback, DOS hidden attribute. Feeds copy-out metadata preservation via the standard `get_metadata` listing derivation.
- **Integrity**: full sequential reads verify CRC32 (and the WinZip AES HMAC) at end of stream; range reads can't see the whole stream and skip verification by construction.

**Encrypted ZIP archives**: The ZIP central directory is always cleartext, so mount and listing always succeed even for encrypted archives — you can browse the entry tree without unlocking anything. The password prompt happens lazily, the first time an encrypted entry is read: the standard askpass UI (same dialog used for SSH/SFTP) is shown, and a working password is cached for subsequent reads. ZipCrypto and WinZip AES (AE-1 and AE-2, all three strengths: AES-128/192/256) are supported; PKWARE SES "strong encryption" is refused cleanly. Passwords are validated against the entry's cheap verifier (AES PBKDF2 verifier bytes / ZipCrypto check byte) — no trial decompression. Wrong passwords re-prompt with an "Incorrect password" hint; dismissing the prompt returns a Cancelled error for that read but does not lock you out — the next read re-prompts. Cleartext entries are always readable without prompting, even in mixed-encryption archives. If individual entries use different keys, the cached password is replaced when a later entry needs a new one. AES-CTR is seekable, so range reads inside encrypted entries stay random-access.

**7z indexing** (one-shot, sans-IO): the in-tree `newt_archive::sevenz` reader fetches the signature header and the next header at first use (a packed header is decoded in memory; an encrypted one prompts for the password *at mount*, since nothing can be listed without it, and a wrong password re-prompts with a hint). Content lives in **folders** — one compressed stream per folder, many entries back to back in the solid case — and every folder decodes through an iluvatar codec chain: Copy, LZMA, LZMA2, Deflate, BZip2, Zstd, Delta, the BCJ converters (x86, PowerPC, IA-64, ARM, ARM Thumb, SPARC, ARM64, RISC-V), BCJ2 and AES-256 as the outermost stage. BCJ2 — 7-Zip's default for the `.exe`/`.dll` block of a Windows software archive — is a four-stream graph: the main LZMA2 stream flows through the chain while the CALL, JUMP and range-coder side streams (LZMA-packed, a few percent of the code's size, capped at 128 MiB decoded each) are fetched and decoded whole at the folder's first read and held for the mount's lifetime; a password-protected BCJ2 block carries one AES coder per packed stream, unlocked by the one prompt. Random access inside a folder is iluvatar's checkpoint machinery: each folder gets a checkpoint index built lazily, only as far as reads reach (a cold read deep into a big solid folder decodes forward once, reporting "Decoding · folder n of m" on the mount's progress channel, and leaves checkpoints behind), and every read — a viewer chunk or a streamed copy-out — parks its reader when it finishes (or is dropped part-way), so a read whose nearest checkpoint lies behind a parked reader resumes that reader instead of restoring (never more decoding than a fresh start). An LZMA checkpoint carries the live dictionary, so the checkpoint interval is derived from the codec's window cost against a 256 MiB per-folder budget rather than a fixed spacing. Streaming reads (copy-out) verify the entry CRC at end of stream; range reads skip it by construction. The same pool (`vfs/archive/stream.rs`) sits under the tar VFS, whose reads are one `StreamReader` over the whole compressed stream.

**Archive-order extraction**: `Vfs::read_order` gives each path its position in the source's cheapest read order — the entry index for 7z (folder by folder, unpack order), the local header offset for ZIP, the uncompressed offset for tar; every other VFS answers `None`. The copy planner asks once per operation and, when it gets an order, emits directories first in walk order and then files and links sorted by it, so "enter archive, select all, copy out" streams a solid folder in one decoding pass with the parked reader carrying from entry to entry. The verb is remoted like the rest of the trait, so an archive mounted in a remote session orders on the agent side.

- **Entry-level encryption**: listing never prompts; the first read from an encrypted folder prompts through askpass, and the key is verified by trial-decoding the folder's first chunk (7z has no cheap verifier). The password is remembered per mount; dismissing a prompt cancels that read and the concurrent batch behind it, and the next read prompts again.
- **Metadata**: unix mode from the attributes' high half when the archiver stored one (no owner exists in the format), FILETIME timestamps at full precision, DOS hidden/read-only bits. Symlink targets are entry content and are read eagerly at index time (links inside encrypted folders stay unresolved rather than prompting at mount).
- **Names**: UTF-16 decoded, both separators normalized, anti items skipped, empty streams distinguished from empty files.
- **Listed but refused on read** (clean `NotSupported`): PPMd folders and any other coder outside the set above. Split volumes and SFX stubs are not recognized. RAR is out of scope: no GPL-compatible reference implementation exists (unrar's license is non-free).

**Navigation out of archives**:
- Pressing `..` at the archive root exits the archive and returns to the parent directory containing the archive file.
- The archive filename itself is focused after exiting.
- Breadcrumbs show the full path including the origin: clicking archive-level breadcrumbs exits back to the origin filesystem.

**Nested archives**: Archives can contain other archives. Opening an inner archive creates a new VFS mount with the outer archive as its origin. The cleanup system prevents unmounting a parent archive while a child archive is still open.

**Stale mount cleanup**: Archive mounts are *ephemeral* — automatically removed when no pane's current path or back/forward history references them (or any origin-derived children — nested archives, searches over them — transitively). The same cleanup machinery handles other ephemeral VFS types (currently SearchVfs) via a shared `is_ephemeral()` descriptor flag.

**Symlink and hard link resolution**: Symlinks (TAR/CPIO/ZIP/7z) and hard links (TAR/CPIO) inside the archive are resolved internally. Directory listings show the *target's* size and `is_dir` for symlinks, and reading or viewing a file through a symlink or hard link transparently fetches the target's contents. ZIP and 7z symlink targets are entry content, so they are read eagerly (tiny) at index time.

**Limitations**: Read-only. No create, modify, delete, rename, or metadata changes inside archives. (Creating new archives is a separate operation — see Pack to Archive under File Operations.)

### Disc Images (ISO 9660 / UDF, Read-Only)

Mount and browse optical disc images (`.iso`, `.udf`) as virtual read-only filesystems, exactly like archives: Enter on a disc image mounts it and navigates into its root; `..` at the root exits back to the image file. Same ephemeral-mount lifecycle, origin-styled breadcrumbs, and read-only capability surface as archive mounts.

**Formats**: ISO 9660 with Joliet and Rock Ridge extensions (Rock Ridge preferred over Joliet when both exist — real POSIX names, permissions, uid/gid, timestamps, symlinks), and UDF 1.02–2.60 including the Metadata Partition used by Blu-ray-era images (Type-1 and metadata partition maps; logical sector sizes 512–4096, so both optical `.iso` dumps and hard-disk-profile UDF images work). Hybrid/bridge images carrying both filesystems use the UDF view (the authoritative one, and the only correct one for >4 GB files). Multi-extent ISO files (the >4 GB mechanism), inline UDF data, sparse UDF extents, and UDF/Rock Ridge symlinks (resolved internally like archive symlinks) are all supported.

**Fully range-read native — no downloading, no indexing pass**: unlike archives, disc-image file data is stored as raw contiguous extents, so every read inside the image translates directly into a range read on the image file. Combined with S3's HTTP-Range reads, a Blu-ray-sized ISO on S3 can be browsed and its files viewed/copied out without the image ever being downloaded. Directory metadata is parsed *lazily per-directory* (no upfront tree walk; entering a 100 GB image costs a handful of small reads) through a 16 MiB block cache that coalesces the structure walk into few upstream GETs; listings are cached permanently since the image is immutable. The in-tree sans-IO `newt-disc` parser crate hands the VFS layer byte ranges to fetch, which are issued concurrently per round — high-latency backends pay round-trips, not per-structure reads.

**Limitations**: Read-only. VAT/virtual and sparable partition maps (packet-written CD-RW/DVD±RW media), El Torito boot image exposure, zisofs compression, interleaved ISO files, and multi-session images are not supported (clean errors where detectable). Raw-sector formats (`.bin`/`.cue`, `.nrg`) are out of scope.

### Remote VFS (client-local filesystem in SSH sessions)

In remote (SSH) sessions, the client-local filesystem can be mounted as a VFS on the remote agent, allowing the user to browse local files alongside remote ones. The root VFS label shows "Remote" in SSH sessions to distinguish it from the client-local VFS which shows "Local".

**Mounted only for sessions given access to this host** — see [Access to this host](#access-to-this-host). Otherwise the remote VFS isn't available and nothing of the local filesystem is exposed to the remote host.

**Architecture**: The agent mounts a `RemoteVfs` that proxies all Vfs trait calls back to the Tauri host over bidirectional RPC. The host runs a `VfsDispatcher` that dispatches these calls to a real `LocalVfs`. Streaming reads and writes each ride their own flow-controlled RPC byte stream, so a slow disk on either end holds up only that transfer, never the rest of the session. An open error comes back from the open, a mid-stream read error from the next read, and a far-side write error (a full disk) from the next write rather than after the whole file has been sent. Dropping a reader stops the far side reading the file; dropping a writer before `finish` leaves the far-side write uncommitted.

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

**Opening**: From any pane, press Mod+F (or run "Find in Folder…" from the command palette). The dialog is rooted at the current pane's directory. Labels are bare names: what each field accepts is in its tooltip, with an example as its placeholder. It offers:

- **Name**: one or more patterns separated by `;` (`*.rs; *.toml`), any of which matches. A pattern is a substring by default — typing `Cargo` matches anything containing `Cargo` — and switches to glob semantics (must match the whole basename) as soon as it contains any of `*`, `?`, `[`. A pattern with a `/` is a glob over the path below the search root instead (`src/**/test_*.rs`; `*` stays within one folder, `**` crosses them). Empty = match every entry. Matches both files *and* directories.
- **Exclude**: `;`-separated patterns of what is neither a result nor searched into — `node_modules; target; *.min.js; docs/generated`. Same forms as Name, except that a plain word is a whole name, never a substring: excluding `test` leaves out a folder called `test`, not `latest.rs`.
- **Case-sensitive**: applies to name and exclude patterns and to content search.
- **Follow symlinks**: off by default (avoids double-counting). This governs links *inside* the searched folder; the folder itself is always searched, including when the pane reached it through a link (results stay keyed under the path the pane shows). On, the shared walker resolves each link and skips cycles, so a link back up the tree cannot loop the search; hits found through a link carry the link's name and the traversed key, with the target as their source.
- **Skip hidden entries**: hidden files and folders are neither results nor searched into.
- **Skip what .gitignore ignores**: the rules of `.gitignore` files — those above the searched folder up to the top of its repository, then each folder's own as the search reaches it — and `.git/info/exclude`, the same reading Quick Open does. `.git` itself is skipped too.
- **Content** (a collapsed section of its own — searching by name is the common case): a substring, or a regex with **Regular expression** set. Files larger than 10 MiB are skipped from content matching but still surface on name match. Directories are skipped when a content filter is set (they have no bytes to scan), and so are FIFOs, sockets and devices, which would never stop giving bytes. **Whole words** requires the match to start and end at word boundaries; **Files not containing it** turns the search around, so the results are the files the content is *not* in (`grep -L`).

**More search options** (a collapsed section):

- **Type**: files and folders, files, folders, or symbolic links. A link is only seen as one while links aren't followed; followed, it counts as what it points to.
- **Size**: at least / at most, inclusive — `10 MB`, `1.5G`, `512 KiB`, or plain bytes; decimal prefixes unless an `i` makes them binary. Folders never match a search with a size bound.
- **Modified / Accessed / Created**: *newer than* and *older than*, each either a span counted back from when the search starts (`7d`, `2w`, `3h`, `90min`, `6mo`, `1y`; a bare `m` is refused as ambiguous) or a date (`2026-01-31`, the start of that day in the user's time zone, wherever the search runs). Created is the birth time, which not every filesystem records.
- **Owner**: user and group, by name or numeric id.
- **Permissions**: any, three presets — executable by anyone (any of `111`), setuid or setgid (any of `6000`), writable by everyone (all of `002`) — or `find -perm`'s three comparisons against octal bits typed in: exactly, all of, any of. Only the permission bits are compared, not the file type.
- **Attributes** (in place of Owner and Permissions where the searched folder has Windows attributes): read-only, hidden, system, archive; a result must have every attribute ticked.
- **Limits**: how many levels deep to search (1 is the folder's own entries), and a number of results to stop at — a stopped search's listing is marked partial with "stopped at N results".
- **Stay on one filesystem**: mount points below the searched folder are not entered (`find -xdev`).
- **Skip package contents** (macOS): apps and other packages can be results but aren't searched inside, as Spotlight does; off by default, so a search goes everywhere Enter does. Remembered with the other toggles.

A filter on metadata an entry doesn't have is no filter for that entry: an S3 object has no owner, so an owner filter lets it through. Where the searched folder's filesystem has no Unix owners or permissions at all (S3, most archives), the Owner and Permissions rows are disabled rather than offered to no effect.

**Unreadable directories** are passed over, not fatal: the walk counts them, the progress line carries the count as a `skipped` sidecar while it runs, and the finished listing is marked partial with "N entries could not be read" as the reason. A search root that cannot be listed at all is an empty listing marked partial with "cannot list …" — the same shape as every other outcome rather than a failed navigation, since the pane has already landed on the search when the walk starts.

**Sticky toggles** (`state.json` `search.*`): how to search is remembered, what to find is not. Regular expression, case-sensitive, whole words, follow symlinks, skip hidden, skip what `.gitignore` ignores, stay on one filesystem and skip package contents seed the next fresh search; the patterns, "files not containing it" and every filter start empty, since a remembered size or date bound would silently hide results from the next search. The **Content** and **More search options** sections open expanded whenever anything in them is set — a remembered toggle (regular expression and whole words in Content, stay on one filesystem in More) or, when refining, a content pattern or filter of the search being refined — so nothing in effect is ever folded away. Refining an existing search (below) restores that search's own params, which take precedence.

Submitting mounts a `SearchVfs` and navigates the active pane to its root. The walker runs in the background; matches stream into the pane as they're found, with the secondary "where from" hint inline next to each filename (formatted through the source VFS's descriptor — so an archive entry shows `/path/to/foo.zip/inner/dir`, not a raw inner-archive path).

**Liveness while nothing is found.** A search over a large tree can run for a long time before its first hit, so the walker reports what it is doing through the VFS progress channel — at most 5×/sec, independent of hits — and the pane status bar renders it as `Searching · 48,120 items · src/main_window`. The count is entries *scanned*, not matched (matches are already the pane's own item count), and the directory is shown relative to the search root, which the pane header already names. The pane lands on the (empty) search VFS immediately rather than waiting for the first match, so the line appears at once and the old directory's listing is not left on screen.

**Display & navigation**:
- The pane's breadcrumb shows the searched folder followed by the name patterns, `/home/foo/projects [*.rs; *.toml]` (`[*]` without any), marked `†` when the search was asked for anything more — content, exclusions, filters, or a toggle off its default: `/home/foo/projects [*.rs]†`. Only the folder is a link (to Go to Path, like any current location); the bracketed suffix is plain text whose tooltip lists the whole search, one line per aspect (`Content: "TODO", whole word`, `Modified: within 2 weeks`, `Options: honours .gitignore`, …). The search's display path is the folder alone — Go to Path and Copy Path get a real path — while the window title and the history list carry the suffix too, to tell searches apart. No `Search:` prefix — the VFS selector already conveys that.
- `try_parse_display_path` returns nothing for SearchVfs paths, so the Navigate dialog will never accidentally drop the user back into a search.
- **Reveal source**: Shift+Enter on a result navigates the pane out of the search to the result's real parent directory in the source VFS, with the file focused. (Same key as Follow Symlink; the alias takes priority when the entry has one.)

**Behavior**:
- **Flat list, with paths shown.** Identically-named matches sort/select independently — entries are keyed by their relative path under the search root, not basename.
- **Rooted at the searched folder.** The SearchVfs's origin is the search root, so `..`/Backspace (and the synthetic `..` row) back out of the results into the directory the search ran over — like exiting an archive, except the origin is a directory so the escape lands *in* it rather than beside it (`OriginKind::Directory` vs. the archives' `Entry`, per `VfsDescriptor::origin_kind`). History (Alt+Left) and Shift+Enter on a hit still work as exits too. A terminal opened on a search pane gets the search root as its cwd.
- **Mod+F inside a search refines it.** The dialog reopens pre-filled with the current search's params (name field focused and selected), rooted at the original root; submitting mounts a fresh search that *replaces* the pane's current history entry, so refinements don't pile up in Alt+Left history and the superseded mount is auto-unmounted. Nested search stays impossible — it would produce duplicate-keyed aliases and break operation routing, so SearchVfs opts out via `VfsDescriptor::can_search` and exposes its params via `VfsDescriptor::search_params` instead. On any other VFS that opts out of search without offering params, Mod+F transparently falls back to the in-pane quick filter (`/`).
- **Invalid patterns fail the mount.** A malformed glob or regex errors in the search dialog on submit (validated with the same engines the walker uses) instead of mounting a search that silently finds nothing. A filter value that can't be read (a size, time, permission bits, depth or limit) is caught before that: the dialog says what's wrong with it, expands More search options if it was collapsed, and focuses the field with its text selected. Nothing is required — a search with only exclusions or toggles lists everything else.
- **Operations are transparent.** Open, view, edit, rename, delete, copy/move, drag-out — every action targets the underlying real file. The display still shows the basename + source-path hint, but the bytes the operation touches are the source file's bytes. The viewer and editor windows, and Quick View, open the source file itself, so their titles and status bars show its real path and relative Markdown links resolve beside it.
- **Walker boundaries.** Walks within a single VFS; mounted child VFSes (archives, etc.) are *not* descended into. OS-level mounts (bind mounts, autofs, network shares) look like ordinary directories and are traversed.
- **Lifecycle.** SearchVfs is *ephemeral* (see below) — it auto-unmounts as soon as it's no longer reachable from any pane's current path or back/forward history, and it does not show up in the VFS selector dropdown. Its origin link also keeps the source mount (e.g. an archive the search ran over) alive while the search is reachable.
- **Deferred for v1**: tree-view toggle, native search inside archives / S3 / SFTP.

### VFS Selector Dialog (Mod+Shift+L)

- Lists all currently **mounted** VFS instances (with VFS ID, type, and mount label).
- Lists **available** VFS types to mount, as a trailing "connect" section (separator + ellipsis-suffixed entries):
  - S3: Mounts immediately on selection (uses ambient credentials).
  - SFTP: Opens hostname input dialog.
  - Remote: Opens the Connect dialog (`DialogKind::MountRemote`) with "Open as a new session" defaulted off, i.e. pre-scoped to a pane mount whatever the session mode. Always offered, even inside a remote session.
- **Volumes** (`hot_paths.mounts_in_vfs_selector`, default on): the mounted volumes Hot Paths lists (see Hot Paths below) appear in a section for the filesystem they're mounted in, the way a split-root filesystem's drives do: a header with the filesystem's name, its root (`/`), then each volume with an icon for its kind (disk, USB drive, disc, disk image, network share, phone or camera), where it's mounted — or, for a network share, where it points (`//user@server/share`, with any password left out) — and its free space. Selecting one navigates the pane there; it's an ordinary path, nothing is mounted or unmounted. They're listed for each filesystem that is a machine's own: the session's root, and in a remote session also this computer's filesystem when it's mounted there, so each machine's volumes sit under its own entry. The list is queried each time the dropdown opens and fills in after it has opened, so a stalled network mount can't hold it up. Volumes the selector already lists as drives (all of them, on Windows) aren't repeated. The ✓ marks the most specific entry holding the pane's path: a volume rather than the filesystem it's mounted in.
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

**Connection profiles** are saved connection configurations stored in `connections.toml` under Tauri's platform-specific application configuration directory for `org.newt-fm.newt`: `~/Library/Application Support/org.newt-fm.newt/` on macOS, `$XDG_CONFIG_HOME/org.newt-fm.newt/` (falling back to `~/.config/org.newt-fm.newt/`) on Linux, and `%APPDATA%\org.newt-fm.newt\` on Windows. Secrets (e.g., AWS access keys) are stored in the system keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service via the `keyring` crate) under the service name `com.newt.credentials`.

**Profile types**:
- **S3**: Region, bucket, endpoint URL, credential mode (default/profile/IAM user/assume role), and associated secrets.
- **SFTP**: Host (`user@hostname`).
- **SSH**: Host (`user@hostname`) + optional `forward_agent` flag (`-A`) + `login_shell` (defaults true). Connecting opens a new window.
- Every profile that opens as a session (SSH, Docker, Podman, Kube, Custom) can carry `host_access`, its own choice for [Allow access to this host](#access-to-this-host).
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

**Connect only to machines you trust.** A remote session runs Newt's agent there as you, and the remote machine can misuse whatever the session gives it.

#### Access to this host

A remote session can be given access to this host, the computer running Newt: its files, mounted in the session as a browsable filesystem beside the remote ones, and — through `newt cmd` in the session's terminals — the clipboard and Newt's commands (opening files in local apps, Reveal in Finder, new windows). It is off unless both of these say otherwise:

- **Consent, once, in Settings**: `behavior.allow_remote_host_access` ("Allow Remote Sessions Access to This Host", default off). Until it's on, no session gets access.
- **Per connection, in the Connect dialog**: **Allow access to this host**, shown for connections that open as a session. While Settings hasn't allowed it, the checkbox is greyed out, with a link to that setting. Once allowed, it starts as it was last left (on, the first time), and a saved profile remembers its own choice (`host_access` in `connections.toml`). Connections that open without the dialog — a recent connection, `--target` — use the dialog's last choice; a saved profile without a choice of its own does too.

Turning the setting off revokes access for every session connected from then on, whatever their profiles say. Without access, `newt cmd` in a remote terminal is refused, naming the checkbox; the other `newt` verbs work as ever, since they act on the session's own panes. Local and elevated sessions are this host already, and pane mounts of a remote (FS-only agents) never reach it.

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
3. It checks a cache directory (`$XDG_CACHE_HOME/newt/`, else `~/.cache/newt/`) for a matching agent, `newt-agent-<hash>-<triple>`: keyed by a blake3 hash of the local agent binary and by the target's platform, so a home shared between machines of different architectures keeps one agent for each. With no home at all, it uses a private directory created for that connection (`mktemp -d`) and caches nothing — a fixed name under `/tmp` could be one another user made first.
4. If cached: Executes immediately (`NEWT:READY`).
5. If missing: Requests upload (`NEWT:NEED:triple:caps`). Newt gzip-compresses the agent binary if the remote supports it and uploads it. The script checks the upload arrived whole (a short read is never installed), caches it for future use, cleans up this platform's older versions (another platform's agents belong to another machine sharing the home), and confirms with a second `NEWT:READY` — the host holds off RPC traffic until then, because some `head -c` implementations (BSD/macOS) read ahead and would swallow bytes sent while the upload is still being consumed.
6. The agent enters RPC mode; all further communication is bincode over stdin/stdout.

**Replacing a broken cached agent**: Reconnecting after a connection that *failed* (rather than one that ran and was lost) uploads the agent afresh even when one is cached (`NEWT_FORCE_UPLOAD=1`), so a cached binary that is there but broken — damaged on disk, or from an interrupted copy — is replaced rather than retried forever.

**Bootstrapless (direct-copy) protocol** (Docker / Podman only):
1. Newt runs `<engine> inspect --format='{{.Os}}/{{.Architecture}}' <container>` and maps the result to an agent target triple.
2. It runs `<engine> cp <local-agent> <container>:/tmp/newt-agent-<hash>`.
3. It execs `<engine> exec -i <container> /tmp/newt-agent-<hash>` and uses that pipe as the RPC channel.
No shell or coreutils in the container required, but every connect re-uploads (no cache).

**After connection**: All filesystem operations, terminal PTYs, file operations, and VFS mounts execute on the remote side. The UI is identical to local mode. If the session has [access to this host](#access-to-this-host), the client-local filesystem is automatically mounted as a Remote VFS (see VFS section).

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

No bootstrap or upload: the bundled Linux-musl agent already lives on the Windows filesystem, so it is exec'd directly from its translated `/mnt/<drive>/…` path (DrvFs default-mounts world-exec). The agent architecture is taken from the Windows host arch (correct for WSL2 and x64 WSL1). The `WslLaunch` process is a normal Win32 process handle wrapped in a small adapter (Rust's `Child` can't adopt a pre-existing handle); closing the window terminates it. WSL is a remote-style session — given [access to this host](#access-to-this-host), it mounts the client-local filesystem as a Remote VFS, same as SSH. There are no saved WSL connection profiles by design.

`WslLaunch`'s relay process attaches to the caller's console, so a windows-subsystem (production) build — which has none — would get a visible console window popped per launch. Before the first launch the process pre-owns a hidden console: `AllocConsoleWithOptions(NO_WINDOW)` where available (Win11 24H2+, resolved dynamically), else `AllocConsole` + hide (one brief flash, first WSL session only). Console-subsystem dev builds and terminal launches already have a console and are untouched.

### Connection Status

Until the session is connected, the main window shows the connection's transcript — the spawn command, ssh's own output, the agent upload — styled like the terminal panel and selectable for copying, above a status bar like a pane's:
- **Connecting**: a block cursor blinks after the last line of the transcript, as in a terminal waiting for output, and the status bar shows a spinner, what is happening ("Connecting via SSH (user@host)…") and how long it has taken so far — the status bar once that has passed 200 ms, so a quick connection doesn't flash it.
- **Connected**: the panes take the window's place.
- **Disconnected / Failed**: the status bar shows the error in red with a **Reconnect** button, which takes the focus, so Enter reconnects.

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

After disconnection, a "Reconnect" button appears in the status bar (focused, so Enter works too). It reconnects in-place on the same window using the same transport parameters (SSH host, elevated mode, etc.): the old session is torn down (agent subprocess terminated, PTYs killed), panes / terminals / operations are cleared, and a fresh session is established.

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
| Mounted Volumes | Linux: network mounts (cifs, nfs, sshfs, rclone, kio-fuse, …) wherever they're mounted, other mounts under `/media/`, `/run/media/` and `/mnt/`, and the locations GNOME has mounted (Nautilus' Other Locations, a phone over MTP) through the gvfs FUSE bridge at `/run/user/$UID/gvfs`, labelled like "media on nas". macOS: what's mounted under `/Volumes`, from the kernel's mount table, so the boot volume's link there isn't listed. Windows: logical drives (`C:`, `D:`, …). Mounts are never stat'ed while listing, so a stalled network share can't hold up the dialog. Each is classified (disk, removable drive, disc, disk image, network share, device; on macOS through DiskArbitration) |
| Mounted VFS | Currently mounted S3, SFTP, and archive filesystems |
| Recent Folders | `recently-used.xbel` on Linux (top 20 by modification time); Finder GoToFieldHistory on macOS |

Each category can be independently toggled on/off in preferences (Hot Paths section).

**Each entry displays**: Name (if bookmarked or named) + path. Matching characters in the fuzzy search are highlighted.

**Keyboard navigation**: Arrow keys, Page Up/Down, Home/End, Enter to navigate to the selected path, Escape to close.

Selecting an entry navigates via `navigate_to_path`, which takes the whole `VfsPath` — `vfs_id` included, so an entry on a mounted VFS lands on that VFS. Removal is keyed by `bookmark_key`, the verbatim `[[bookmark]]` `path` from settings.toml, which is a display path rather than the wire form.

### Go to File (Mod+G)

A palette that finds files and folders anywhere under the active pane's directory by name, as you type — the file manager's take on VS Code's Ctrl+P. Enter focuses the chosen entry in the pane it was opened from: the pane goes to the entry's folder and puts the cursor on it, a folder as much as a file. Opening it from there is the pane's own Enter, so Mod+G, a few characters, Enter, Enter opens a file through its association.

- **Live**: the directory is walked when the palette opens, nearest entries first (breadth-first: everything in a folder before anything in its subfolders), and results appear and re-rank while the walk goes on. A status line under the input counts the entries seen ("Searching… 12,345 items"), says when the walk stopped at the limit, and how many folders couldn't be read. Nothing is cached: each opening walks afresh, and closing the palette stops the walk.
- **Matching** is VS Code's: an entry matches when the query's characters appear in its path in order, ignoring case — `fobr` finds `foo_bar.txt`. Spaces split the query into pieces that all have to match (`pane tsx`). There is no other query syntax.
- **Ranking**: a match within the name ranks above one that needs the folders too, so `pane` puts `Pane.tsx` before `pane_helpers/x.ts`, while `mw/pane` still reaches `main_window/Pane.tsx`. Among matches of the same kind, runs of consecutive characters and characters at word starts (after `/`, `_`, `-`, `.`, a case change) score higher; ties go to the shallower entry, then the shorter path. With an empty query the list is simply the nearest entries. The top 100 are shown, with the matched characters in bold.
- **What's left out**: hidden entries, unless the pane is showing hidden files; `.git` always; names matching `quick_open.exclude`; and, with `quick_open.gitignore` (on by default), whatever `.gitignore` files ignore — those in the folders walked, those in the folders above up to the repository's top, and the repository's `.git/info/exclude`. The user's global git excludes file is not consulted, and no `git` is run: the rules are read and applied as the walk goes, on any filesystem. Symlinked folders are entered only with `quick_open.follow_symlinks`; with `quick_open.skip_packages` (macOS, off by default) apps and other packages are results but aren't walked into; the walk stays on one filesystem and stops after `quick_open.limit` entries (200,000).
- **Where it runs**: next to the files — in a remote session on the remote host, so only the top results cross the connection, whatever the size of the tree; this machine's own files, browsed from a remote session, are walked here. It works on any filesystem a pane can show: local, SFTP, S3, archives, disc images.

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
run = "tar czf {{ file.stem }}.tar.gz {{ files | map(attribute='name') }}"
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

### Quoting

Everything a template interpolates is quoted for the shell the command runs in, so a file name is always one argument, whatever characters it holds — a name like `x'; rm -rf ~ #` is just a name. The shell is the one of the machine the command runs on: `sh` (POSIX quoting; dash on Debian and Ubuntu has no `$'…'`) or, on Windows, `cmd.exe` (double quotes, with `%` and `"` written outside them; a value with a line break can't be expressed and fails the command). In a remote session that's the remote machine's shell.

- A list interpolates as one word per item: `{{ files | map(attribute='name') }}` → `'a b.txt' c.txt`. `join` works as in Jinja — plain values join into one string, so `join(', ')` makes text for a `confirm()` message, or one word on the command line — while values already quoted (`map('shell_quote') | join(' ')`) stay separate words.
- Text in the template itself is not quoted: `{{ file.stem }}.tar.gz` → `'my file'.tar.gz`, which the shell reads as one word.
- `| raw` inserts a value as-is, for a value meant as shell syntax — say `{{ prompt("Flags") | raw }}` for several flags at once (a list goes in space-separated). Jinja's `| safe` does the same for a single value.
- `| shell_quote` quotes explicitly and marks the result quoted, so it's never quoted twice.
- Quoting doesn't stop a name that starts with `-` from being read as an option; put `--` before file arguments where the program supports it.

### Template Variables

| Variable | Type | Description |
|----------|------|-------------|
| `file` | Object | Currently focused file |
| `file.name` | String | Filename with extension |
| `file.path` | String | Full absolute path, as Copy Path gives it (`C:\Users\…` on Windows) |
| `file.source` | String | Underlying real path for virtual entries (e.g. search hits), likewise; undefined for ordinary files |
| `file.stem` | String | Filename without extension |
| `file.ext` | String | Extension (without dot) |
| `file.is_dir` | Bool | Is it a directory? |
| `file.size` | Number | Size in bytes (may be undefined) |
| `file.modified` | Number | Unix timestamp in seconds (may be undefined) |
| `files` | Array | Selected files, or `[file]` if nothing selected |
| `dir` | String | Active pane's current directory (absolute path, as Copy Path gives it) |
| `other_dir` | String | Other pane's current directory |
| `hostname` | String | Machine hostname |
| `env.NAME` | String | Environment variable (e.g., `env.HOME`, `env.PATH`) |

### Custom Filters

| Filter | Description | Example |
|--------|-------------|---------|
| `raw` | Insert as-is, unquoted (see Quoting) | `{{ prompt("Flags") \| raw }}` |
| `shell_quote` | Quote explicitly (interpolations are already quoted; see Quoting) | `{{ file.name \| shell_quote }}` → `'my file.txt'` |
| `basename` | Extract filename from path | `{{ file.path \| basename }}` |
| `dirname` | Extract directory from path | `{{ file.path \| dirname }}` |
| `stem` | Filename without extension | `{{ file.name \| stem }}` |
| `ext` | Extract extension | `{{ file.name \| ext }}` |
| `regex_replace(pattern, replacement)` | Regex substitution | `{{ file.name \| regex_replace("\.bak$", "") }}` |
| `join_path` | Join path segments | `{{ [dir, "subdir"] \| join_path }}` |

All standard Jinja2 built-in filters are also available (`map`, `join`, `upper`, `lower`, `selectattr`, etc.); see Quoting for `join`.

### Custom Functions

| Function | Signature | Description |
|----------|-----------|-------------|
| `prompt(label, default="")` | `(string, string?) → string` | Shows a text input dialog. Returns user input or default. |
| `confirm(message)` | `(string) → bool` | Shows a yes/no dialog. Returns true if confirmed. Aborting cancels the whole command. |

### Execution Modes

**Terminal mode** (`terminal = true`):
- Renders the template into a shell command.
- Creates a new terminal tab and executes `sh -c "rendered_command"` (`cmd.exe /V:OFF /C` on Windows), with the shell picked on the machine the terminal runs on.
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

**Mistakes don't cost the rest of the file.** Each setting is read on its own: one with the wrong type (`history_retention = "lots"`), an unknown key, or a `[[bind]]`/`[[command]]`/`[[bookmark]]`/`[[association]]` entry that doesn't read is left out, and everything else applies — a left-out setting keeps its default. Settings that read but can't work are flagged too: a binding to a command that doesn't exist, a key whose modifiers aren't in the order `meta`, `ctrl`, `shift`, `alt` (it would never fire), a pattern that isn't a valid glob, an association whose `command` names no user command, an editor language that doesn't exist. Only a file that isn't TOML at all is ignored as a whole; on a reload, the settings in force before stay in force.

When the file has problems — at startup or after any reload — a toast appears over the main window: the first problem with its line, and how many more there are. It doesn't time out, and offers **Show** (opens Settings) and **Open settings.toml**; once dismissed (×), it stays away until a reload turns up a different set of problems, and goes on its own once the file is fixed. The Settings dialog lists every problem with its line at the top, whether or not the toast was dismissed.

### Panic Reports

A panic — a bug that stops a piece of Newt dead — is written to `panic-<start time>-<pid>.txt` in the platform's log directory for `org.newt-fm.newt` (`~/Library/Logs/org.newt-fm.newt/` on macOS, `$XDG_DATA_HOME/org.newt-fm.newt/logs/` (falling back to `~/.local/share/org.newt-fm.newt/logs/`) on Linux, `%LOCALAPPDATA%\org.newt-fm.newt\logs\` on Windows): the version and revision, the thread, the message with its source location, and a backtrace. One file per run, capped at 1 MB; the ten newest runs' files are kept. One process hosts every window, so a panic inside a command handler ends all of them, while one in a background task ends only that task — either is reported.

A remote or elevated session's agent ends itself on a panic, which the window sees as its connection closing (the agent's report is in the connection log). A connection whose handler on either side panics is closed rather than left looking alive with nothing answering: calls in flight fail instead of waiting forever.

### Runtime State File

`state.json` in the same platform-specific application configuration directory — machine-written, ephemeral-ish UI state, kept out of `settings.toml` (which stays purely user-authored). Plain JSON managed by `RuntimeStateManager` (`src-tauri/src/runtime_state.rs`): loaded once at startup (corrupt/missing → defaults), written on each discrete change, and broadcast app-wide via the `update:runtime-state` event (consumed by the `useRuntimeState` hook). Updated by dotted-key commands (`update_runtime_state`), validated against the typed `RuntimeState` struct (unknown keys rejected). Holds per-pane column widths (`column_widths.<pane>.<column>`), the app-wide webview zoom factor (`zoom`), the terminal panel height (`layout.terminal_height`; the file-pane split deliberately stays 50/50), sticky last-used dialog toggles (`copy_move.*`, `search.*`), and the recent ad-hoc connections MRU (`recent_connections`, see Quick Connect). Per-target pane locations and window geometry live in `sessions.json` instead (see Reopening Where You Left Off). No file watcher — external edits apply on next launch.

### Full Settings Structure

```toml
[appearance]
folders_first = true        # Directories before files in sort order
natural_sort = false        # Numbers in names sort by value (IMG_2 before IMG_10)
show_command_bar = true     # Show F-key bar at bottom of window
show_pane_header = true     # Show breadcrumb / VFS selector / free-space header per pane
show_pane_status = true     # Show file count / selection size status bar per pane
density = "comfortable"     # File list row spacing: "compact" (20px), "comfortable" (22px) or "spacious" (24px)
theme = "system"            # "system", "light", or "dark"
columns = ["name", "size", "modified_date", "modified_time", "user", "group", "mode"]
si_size_prefixes = false    # Size column shows "1.5 GB" instead of exact byte counts
size_units = "decimal"      # "decimal" (kB/MB/GB, 1000) or "binary" (KiB/MiB/GiB, 1024)
locale = ""                 # BCP-47 tag for numbers/dates (e.g. "de-DE"); "" = system regional format
date_format = ""            # strftime-style date column format (e.g. "%Y-%m-%d"); "" = system locale
time_format = ""            # strftime-style time column format (e.g. "%H:%M"); "" = system locale

[behavior]
confirm_delete = true       # Ask for confirmation before deleting
confirm_close = true        # Ask before closing a window with terminals open or operations running
delete_to_trash = true      # Move deletes to the OS trash; Delete Permanently bypasses
keep_terminal_open = true   # Keep terminal tab open after shell exits
keep_finished_operations = false  # Keep completed/cancelled ops in panel
quick_search = true         # Use prefix quick-search; when false, typing opens regex filter
allow_remote_host_access = false  # Allow remote sessions to be given access to this host (per connection)
default_sort = { key = "name", ascending = true }
history_retention = 200     # Max entries kept per pane in nav history (0 = unlimited)
shell_integration = true    # `newt` CLI in built-in terminals / user commands (local sessions; remote always on)
restore_locations = "all"   # Reopen panes where the target's last session left them: "all", "local" (local/elevated/WSL), "none"
new_window_location = "inherit"  # New Window panes: "inherit" (opener's), "restore" (last closed local window's), "default" (home)
restore_window_geometry = true   # Reopen windows at their last size/position (per target; viewer/editor one size each)
open_packages = false       # macOS: Enter opens app bundles and other packages instead of entering them

[enrichers]
git_status = true           # Git enricher: per-row status colors + branch badge

[archives]
default_format = "tar_zst"  # Format preselected in Pack to Archive: "zip", "seven_z", "tar", "tar_gz", "tar_xz", "tar_zst"
preserve_symlinks = true    # Store symlinks as symlinks (false: follow them)
zip_level = 6               # Deflate level for zip (0-9, 0 = store)
sevenz_level = 6            # LZMA2 level for 7z (0-9, 0 = store)
gzip_level = 6              # tar.gz level (0-9)
xz_level = 6                # tar.xz level (0-9)
zstd_level = 3              # tar.zst level (1-22)

[hot_paths]
standard_folders = true     # Show Home, Downloads, Documents, etc.
system_bookmarks = true     # Show GTK bookmarks (Linux)
mounts = true               # Show mounted volumes
recent_folders = true       # Show recently visited directories
mounts_in_vfs_selector = true  # Also list mounted volumes in the VFS selector

[quick_open]                # Go to File (Mod+G)
gitignore = true            # Leave out what .gitignore files and .git/info/exclude ignore
follow_symlinks = false     # Enter folders reached through symlinks
exclude = []                # Name globs never shown or descended into, e.g. ["node_modules", "*.pyc"]
limit = 200000              # Entries looked at before the search stops
skip_packages = false       # macOS: find apps and other packages, but don't look inside them

[environment]
extra_path = ["/opt/homebrew/bin", "/usr/local/bin"]  # Searched after PATH for the programs Newt runs itself (ssh, docker, podman, kubectl, git); not added to terminals or user commands. Defaults: this on macOS; ["~/.local/bin", "/snap/bin", "/var/lib/flatpak/exports/bin"] on Linux; [] on Windows

[terminal]
font_family = 'Menlo, Monaco, "Courier New", monospace'  # CSS font-family list; the first installed font is used
font_size = 12              # Points (6–72)
scrollback = 1000           # Lines kept above the screen

[editor]
word_wrap = false           # Default word wrap in the text editor (per-file toggle still overrides)

[viewer]
image_background = "checkerboard" # Backdrop behind images: "checkerboard", "dark", "light"
alternate_mode = "dissect"  # What F3 switches to (except from Hex, Table, Markdown): "dissect" (Structure), "hex"

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

[[association]]             # See File Associations; first match first, property by property
match = ["*.nupkg", "*.vsix"]
enter = "browse"            # Optional: "open", "browse", "view", "edit", "command"
format = "zip"              # Optional: "zip", "7z", "tar", "compressed", "disc"
kind = "file"               # Optional: "file" or "directory"
command = "My Command"      # With enter = "command": a [[command]] title
viewer = "text"             # Optional: viewer mode
language = "typescript"     # Optional: editor language
```

### Settings Dialog (Mod+,)

Four tabs:

**Preferences tab**:
- Sidebar with category filter: All, Appearance, Behavior, Enrichers, Archives, Hot Paths, Go to File, Environment, Editor, Viewer — one per section of `settings.toml`, named by the schema's titles. All lists every setting, with a header at the start of each category. From the keyboard, Tab reaches the selected category and Up/Down/Home/End move between them.
- Search box for full-text search across setting titles and descriptions.
- Each setting rendered as a row with title, description, and appropriate control:
  - Boolean → checkbox.
  - Enum → dropdown.
  - Number → number input.
  - String → text input.
  - List of strings → a text input per item with a × to remove it, and a dashed row at the end that adds what's typed into it (Enter or leaving the row); an item emptied out is removed.
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

**Associations tab**: a table of name patterns — one row per pattern, the user's ("Yours") above the built-ins — with what happens to a match in three columns: **Enter**, **View (F3)** — the mode the viewer opens it in — and **Syntax highlighting (F4)**, the language the editor highlights it as (F4 always opens Newt's editor). Directory rows have only Enter.
- **Adding**: the first row is an add row — type a pattern into its Name field (`.rtf` is read as `*.rtf`; a glob, or an exact name like `Makefile`), pick Files or Folders, and its cells show at once what that pattern would inherit; setting any of them adds it, and the field empties for the next one. Enter in the field moves to its Enter cell. Typing a pattern that is already listed edits that row.
- **Every cell shows what applies.** A value the row's own `[[association]]` entry sets is shown plainly with an accent dot; one it inherits is muted, and says on hover where it comes from — built-in, by file type, or another of the user's patterns (`*.gz` showing through on `*.tar.gz`).
- **A cell is a menu.** Click it, or focus it and press Enter or Space: the first choice is always *Default — what would apply otherwise*, which takes the row's own setting away; the rest are the values. Enter's menu has Open in default app, Open in viewer, Open in editor, Browse as ZIP / 7-Zip / tar / compressed file / disc image, and each user command; for a file Enter doesn't browse, a Browse Into submenu picks the format Browse Into uses. A choice writes one key of the pattern's entry in `settings.toml`, in place — comments and the order of entries survive — or adds a one-pattern entry for a pattern that had none. **Delete** on a row of the user's removes its entry (or, from an entry shared with other patterns, just that pattern), and the pattern goes back to whatever applied before.
- **Arrow keys move between cells**, Home and End along a row; Down from the search box goes into the table and Up from its top row back.
- **The search box** narrows the table to patterns containing the text or matching it as a file name, so typing `release.tar.gz` lists `*.tar.gz` and `*.gz` and sums up what happens to that file above the table. When nothing matches, **Add** puts the text into the add row.
- **Change Association…** (`change_association`, unbound; file context menu, palette) opens this tab at the row that decides the focused entry — the most specific pattern matching it — or, when none does, with the add row holding its extension (`*.rtf`) or, for a name without one, the name itself.

**Footer**: a "Settings file" label beside a segmented pair of icon buttons — **Show in pane** navigates the pane the dialog was opened from to `settings.toml` and focuses it (closing the dialog), **Open in external editor** hands the file to the system handler. Both create the file with a commented skeleton if it doesn't exist yet. Show in pane needs the host machine's filesystem mounted in the session (`VfsInfo::host_local_vfs_id` — always true locally, true in a remote session while the client-local mount is present) and is hidden otherwise, leaving external open as the only route.

### Keybinding System

Bindings are resolved in cascade order (later overrides earlier):
1. **Default bindings**: Built into the application (see shortcut reference table).
2. **User overrides**: `[[bind]]` entries in `settings.toml`.

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

### Open Settings File in Editor

Available from the command palette (File category, unbound). Hands `settings.toml` to the system's default application for it — the same as the Settings dialog's **Open in external editor** — creating it with a commented skeleton first if it doesn't exist yet. To edit it in Newt's own editor, use the dialog's **Show in pane** and press F4.

---

## 14. Command Bar

Optional bottom bar (toggled in preferences: `appearance.show_command_bar`, default: on).

Shows clickable buttons for frequently used commands, each displaying the command name and its keyboard shortcut:

Command Palette | Rename | View | Edit | Copy | Move | Create Directory | Delete | User Commands

Clicking a button executes the command. Right-clicking the bar offers Hide Command Bar, which turns the preference off; Settings turns it back on.

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

### Viewer Windows

Focus stays on the viewer's content, so its keys keep working: Tab and Shift+Tab do nothing outside a text field (the table viewer uses them to move between cells) instead of walking focus off into the page, and closing a viewer's context menu returns focus to the viewer — or, in the table viewer's select mode, to the cell's text box.

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

### Windows Installer

Windows ships a single MSI, authored from scratch in `packaging/windows/`
and built with WiX (dotnet tool, pinned in `.config/dotnet-tools.json`) by
`packaging/windows/build-msi.ps1` — Tauri's bundler is not involved, same as
on Linux. The app binary itself is a plain `cargo build --release -p newt
--features custom-protocol`; tauri-build embeds the icon, manifest, and
frontend at compile time. The WiX invocations pass `--acceptEula wix7`
(Open Source Maintenance Fee EULA; Newt is fee-exempt as an open-source
project).

The **UpgradeCode is pinned** in `newt.wxs` to the value tauri-bundler
derived for previously shipped MSIs (`uuid5(DNS, "Newt.exe.app.x64")`) —
changing it orphans existing installs.

Installer UI (custom WixUI set in `ui.wxs`): Welcome → License →
per-user/per-machine scope → install dir + "Create a desktop shortcut"
checkbox (unchecked by default) → confirm; the exit page offers "Launch
Newt". Rerunning the installer gives the stock Change/Repair/Remove
maintenance flow, where Change opens the feature tree — that is how an
existing install adds or drops the desktop shortcut later. The desktop
shortcut is a Level-2 feature: silent installs opt in with
`NEWT_DESKTOP_SHORTCUT=1` or `ADDLOCAL=DesktopShortcut`, and a previous
install's choice is re-detected from the registry (old Tauri-bundled MSIs
wrote the same `Desktop Shortcut` value, so their users keep the shortcut
they already have).

WebView2 is detect-only: a launch condition blocks the install with a
pointer to the runtime download if it is missing; nothing is downloaded by
the installer. One dual-scope caveat: a per-user install cannot see or
upgrade an existing per-machine install (MSI context isolation), so
switching scope leaves the other copy behind.

The Add/Remove Programs entry carries the icon, homepage, and support
links. Banner and dialog artwork is generated from the app icon and logo
palette by `packaging/windows/generate.py` (Pillow); WiX stretches the
fixed-size BMPs on high-DPI displays — there is no oversized-asset escape
hatch.

### Copy Pane (Mod+.)

Sets the other pane's directory to match the active pane's current path. Useful for quickly aligning both panes to the same location before a copy/move.

### Swap Panes (Mod+U)

Exchanges the two panes side for side. Not a mutual Copy Pane: the panes themselves move, so each arrives with its selection, sorting, filter, computed sizes and navigation history intact — Alt+Left after a swap steps back through the history that came across with the pane. `active_pane` moves with them, so the cursor stays on the directory it was in and that directory changes sides. Bound with `pane_focused` so it never shadows a shell's Ctrl+U (kill-line) in the terminal panel; the swap flips `active_pane` without touching `panes_focused`, so running it from the palette while the terminal has focus doesn't steal focus back.

Scroll offset is the one thing that stays put — it lives in the DOM, not in pane state — but the focused item is scrolled back into view as soon as its pane is active.

### Follow Symlink (Shift+Enter)

When a symlink is focused, navigates to where it leads, with the target focused in its directory. The link is resolved by the filesystem it lives on, the way that filesystem resolves links — `realpath` on the local and remote filesystems and over SFTP, the archive's or disc image's own tree inside one — so relative targets climbing out with `..`, chains of links, and links under a linked directory all land where the OS would. The pane lands on the resolved (physical) path, not a path through the link. A link that leads nowhere reports the error.

### Reveal in File Manager (Shift+F3)

Shows the focused entry in the system's file manager, selected in its folder: `SHOpenFolderAndSelectItems` on Windows (Explorer's `/select`), Finder's reveal (`open -R`) on macOS, and on Linux `org.freedesktop.FileManager1.ShowItems` over D-Bus — which Nautilus, Dolphin, Nemo and most others implement — falling back to the desktop portal's Open Directory where no file manager provides it. In a search-results pane it reveals the matched file where it actually lives. On `..` (or in an empty folder) it opens the pane's directory instead. Host-local filesystems only.

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

Toggles whether hidden entries are shown, in every pane of every window at once. What counts as hidden is the filesystem's call: a leading `.` on Unix hosts, over SFTP and inside archives and disc images; on a Windows host the Hidden or System attribute instead (a dot file is an ordinary file there); ZIP and 7z entries also by their DOS hidden attribute, disc-image entries by their own hidden flag. S3 objects are never hidden. The `..` parent directory is *always* visible. The choice is remembered across restarts (`show_hidden` in `state.json`), and a new window starts with it; there is no setting for it in `settings.toml`.

---

## 17. Default Keyboard Shortcut Reference

### File Operations

| Shortcut | Action | Context |
|----------|--------|---------|
| F2 | Rename | Pane focused |
| Shift+F2 | Batch Rename | Pane focused |
| F3 | View file | Pane focused |
| Shift+F3 | Reveal the focused entry in the system file manager | Pane focused |
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
| Mod+U | Swap panes (whole pane state, history included) | Pane focused |
| Mod+P | Hot paths | Any |
| Mod+G | Go to file | Any |
| Mod+B | Add bookmark | Pane focused |
| Mod+Shift+L | Select VFS | Pane focused |

### Selection

| Shortcut | Action | Context |
|----------|--------|---------|
| Insert | Toggle select + advance focus (`toggle_selection`) | Pane focused |
| Mod+A | Select all | Pane focused |
| Mod+D | Deselect all | Pane focused |
| Num * | Invert selection | Pane focused |
| Num + | Select by pattern… | Pane focused |
| Num - | Deselect by pattern… | Pane focused |
| (unbound) | Select same extension | Pane focused |

### Clipboard

| Shortcut | Action | Context |
|----------|--------|---------|
| Mod+C, Ctrl+Ins (not macOS) | Copy to clipboard | Pane focused |
| Mod+X | Cut to clipboard | Pane focused |
| Mod+V, Shift+Ins (not macOS) | Paste from clipboard | Pane focused |
| ⌥⌘V (macOS) | Move here from clipboard | Pane focused |
| ⌥⌘C (macOS), Ctrl+Shift+C | Copy path | Pane focused |

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
| `viewer_copy` | Mod+C | Text, Hex, Structure, Image |
| `viewer_copy_value` | Mod+Shift+C | Structure |
| `viewer_copy_bytes` | (unbound) | Structure |
| `viewer_copy_tree` | (unbound) | Structure |
| `viewer_save_bytes` | Mod+S | Structure |
| `viewer_follow_target` | Mod+Enter | Structure |
| `viewer_toggle_bytes` | Mod+B | Structure |
| `viewer_dissect_as` | Mod+D | Structure |
| `viewer_filter` | / (also numpad /) | Structure |
| `viewer_select_all` | Mod+A | Text, Hex, Structure, Image |
| `viewer_find` | Mod+F | Text, Hex |
| `viewer_goto` | Mod+G | Text, Hex, Structure |
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
