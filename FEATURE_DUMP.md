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

### Zoom

- **Ctrl+=**: Zoom in.
- **Ctrl+-**: Zoom out.
- **Ctrl+0**: Reset zoom to default.

Zoom is applied as frontend-side CSS scaling.

### Window Management

- **New Window** (Mod+N): Opens a fresh Newt window (new session).
- **Close Window** (Mod+W): Closes the current window.
- **Reload Window** (command palette): Reloads the frontend UI.

---

## 2. Dual-Pane File Browser

Each pane is an independent file browser with its own path, selection, filter, sort order, and navigation history.

### Pane Header

- **VFS selector dropdown**: Shows the current filesystem type (Local, S3, SFTP, Archive name). Click to open a dropdown listing all mounted VFSes and available mount options. Mounted VFSes show an unmount (×) button. Unmounted types (S3, SFTP) show "connect..." entries that open mount dialogs.
- **Breadcrumb path**: Current directory path displayed as clickable breadcrumb segments. Clicking any segment navigates to that directory. Clicking the *last* segment opens the Navigate (Go To) dialog instead of navigating. Breadcrumb format varies by VFS type:
  - Local: `/home/user/documents`
  - S3: `s3://bucket/prefix/key`
  - SFTP: `sftp://hostname/path`
  - Archives: origin path + inner path, e.g., `/home/user/file.tar.gz/dir/subdir`
- **Free space indicator**: Shows available disk space (e.g., "123.4 GB free") when the filesystem reports stats. Only visible for VFS types that support `fs_stats` (local filesystem).

### File List

Virtualized (viewport-rendered) list with 22px fixed row height. Only visible rows plus a small overscan buffer are rendered, enabling smooth performance even with very large directories.

**Columns** (all sortable by clicking the header):

| Column | Width | Alignment | Content |
|--------|-------|-----------|---------|
| Name | 250px | Left | File type icon (color-coded, VSCode icon set) + filename |
| Extension | (sub-column of Name) | | Sorted separately from name |
| Size | 100px | Right | Locale-formatted byte count, "DIR" for directories, "???" if unknown |
| Modified Date | 80px | Right | Locale-formatted date |
| Modified Time | 80px | Right | Locale-formatted time |
| User | 70px | Left | Owner name (or numeric UID if name unavailable) |
| Group | 70px | Left | Group name (or numeric GID) |
| Mode | 70px | Left | Unix permissions string, e.g., `drwxr-xr-x` |

Column widths are resizable by dragging the grip between column headers. Minimum width: 10px. Column widths persist per-pane during the session.

Click a column header to sort ascending by that key; click the same header again to toggle descending. A triangle indicator (▲/▼) shows the active sort column and direction.

**Available sort keys**: Name, Extension, Size, User, Group, Mode, Modified, Accessed, Created.

**Sort behavior**:
- The `..` parent directory entry *always* sorts first, regardless of sort key or direction.
- When "Folders first" is enabled (preference), directories sort before files (but after `..`).
- Extension sort treats directories as having no extension.

**Visual indicators**:
- Selected files: distinct background highlight.
- Focused (cursor) file: different highlight from selection.
- Hidden files (starting with `.`): dimmed styling.
- Symlinks: special styling (CSS class).
- `..` parent directory: always shown at the top, even when hidden files are hidden or a filter is active. Cannot be selected.

**Status bar** (bottom of each pane) — content changes dynamically:

| State | Display |
|-------|---------|
| Loading (first 200ms) | (nothing — grace period to avoid flicker) |
| Loading (after 200ms) | "Loading... (X items so far)" |
| Loaded, no selection | "X files, Y directories" |
| Loaded, with selection | "X files, Y directories selected, Z bytes total" |
| Filter active | "(showing X of Y)" appended |
| Partial results | "(partial)" appended when directory listing was truncated |

### Directory Loading

Large directories are loaded incrementally via streaming:
- First batch clears old state (filter, selection, focus).
- Intermediate batches update the visible file list and statistics in real time — the user can browse and interact while loading continues.
- Navigation to a new directory auto-cancels any in-progress load for the same pane.
- A 200ms grace period suppresses the loading spinner to avoid flicker on fast directories.

### Keyboard Navigation

| Key | Action |
|-----|--------|
| Arrow Up/Down | Move focus one item |
| Shift+Arrow Up/Down | Move focus and extend selection |
| Page Up/Down | Jump 10 items |
| Shift+Page Up/Down | Jump 10 items with selection |
| Home | Jump to first item |
| End | Jump to last item |
| Enter | Open file or enter directory (see "Enter behavior" below) |
| Backspace | Navigate to parent directory (`..`) |
| Tab | Switch active pane |
| Insert | Toggle selection on current file and advance focus to next |
| Mod+A | Select all files (except `..`) |
| Mod+D | Deselect all |
| Escape | Clear filter text, or clear selection if no filter active |
| Shift+Enter | Follow symlink (navigate to its target) |

**Enter behavior** depends on what's focused:
- **Directory**: Navigate into it.
- **Archive file** (`.tar.gz`, `.zip`, etc.): Mount as VFS and navigate into the archive root.
- **Symlink to directory**: Follow the symlink and enter the target directory.
- **Regular file**: Open with system default application.
- **`..`**: Navigate to parent directory.

### Mouse Interactions

| Action | Behavior |
|--------|----------|
| Left click | Focus the clicked file |
| Ctrl+Click | Toggle selection for that file (keeps other selections) |
| Shift+Click | Range select from focused file to clicked file |
| Double-click | Open/enter (same as Enter key) |
| Right-click | If clicked file is NOT selected: focus it (clears selection), show context menu. If clicked file IS selected: keep selection, show context menu. |
| Drag on empty area | Rectangle (marquee) selection — see below |
| Drag on file icon/name | Initiate drag-and-drop to other pane — see below |

**Rectangle (marquee) selection**:
- Must drag at least 5px before the rectangle appears (prevents accidental activation on click).
- A blue selection rectangle is drawn; files overlapping the rectangle are selected.
- Auto-scrolls when dragging near the top or bottom edges of the pane (44px zone). Speed increases closer to the edge.
- **Ctrl+Drag**: Adds rectangle selection to existing selection.
- **Shift+Drag**: Selects range from focused file to drag endpoint.
- **Normal drag**: Replaces entire selection with rectangle contents.

**Pane activation**: Clicking anywhere in a pane makes it the active pane for keyboard commands.

### Context Menu

Right-click a file or press Shift+F10 / Menu key:

| Item | Shortcut |
|------|----------|
| Open | |
| View | F3 |
| Edit | F4 |
| Copy Path | Mod+C |
| Rename | F2 |
| Delete | F8 |
| Open in Terminal | Mod+Enter |
| Properties | Alt+Enter |

### Drag and Drop

- Drag one or more files from one pane to the other by clicking and dragging the file icon or name.
- **Multi-file drag**: If multiple files are selected, dragging any selected file drags them all. A ghost preview shows "N items" at the cursor.
- **Drop targets**: Drop on a folder to copy/move into it. Drop on the pane background to copy/move to the pane's current directory.
- **Modifier keys**: Normal drop = copy. Shift+drop = move.
- **Visual feedback**: Drop target pane/folder highlights on hover.
- **Same-pane restrictions**: Cannot drop on `..`, cannot drop a folder onto itself.

### Focus Preservation

- When navigating to a new directory, the first file is focused by default.
- When navigating **back** (Alt+Left), the previously focused file is restored.
- When exiting an archive VFS via `..`, the archive file itself is focused in the parent directory.
- On refresh (e.g., after file system changes), existing selection and focus are preserved if the files still exist.
- Selection state survives filter changes in Filter mode (but not in Quick Search mode).

---

## 3. Filtering and Search

Two filter modes for narrowing the visible file list within a pane:

### Quick Search Mode

- **Activation**: Start typing any printable character (not modified with Ctrl/Shift/Alt) while the pane is focused.
- **Matching**: Case-insensitive **prefix** matching on filenames. Wraps around the file list (searches downward from cursor, then wraps to top).
- **Live updates**: Results update as you type. The cursor moves to the first match.
- **Arrow Left/Right**: Adjusts the search string based on the focused file's name. Right extends the search to include more of the focused filename; Left trims it.
- **Press `/`**: Switches to full Filter mode, keeping the current search text.
- **Cleared by**: Escape, any selection action, or navigating to a different directory.

### Filter Mode (Visual Regex)

- **Activation**: Press `/` (or switch from Quick Search by pressing `/`).
- **UI**: A filter input bar appears at the bottom of the pane.
- **Matching**: Full **regex** pattern matching (case-insensitive). Files that don't match are hidden entirely.
- **`..` always visible**: The parent directory entry is never hidden by a filter.
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

Modal dialog with the current filename pre-filled and fully selected (so you can type a new name immediately). Renames the file or directory within the same VFS.

### Delete (F8 / Shift+Delete / Cmd+Backspace)

Deletes all selected files and directories (recursive for directories).

- If `behavior.confirm_delete` is enabled (default: yes), a confirmation dialog appears first showing what will be deleted.
- If nothing is selected, the focused file is the target (unless it's `..`).

**Delete strategies** (tried in order):
1. **Fast tree removal**: If the VFS supports atomic `remove_tree()`, deletes the entire tree in one call.
2. **Manual tree walk**: Depth-first traversal — deletes files first, then directories bottom-up.

### Copy (F5)

Opens a modal dialog with:

- **Destination path** input (auto-focused, pre-filled with the other pane's directory).
- **Summary**: Shows the filename (single file) or "N items" (multiple selection).
- **Options** (checkboxes):
  - **Create symbolic link** — only available for single-file copies. Creates a symlink at the destination pointing to the source. Disables the other options when checked.
  - **Preserve timestamps** — maintains file modification and access times.
  - **Preserve owner** — maintains UID.
  - **Preserve group** — maintains GID.

**Copy execution**:

1. **Planning phase**: Recursively traverses all source directories to build a complete file list. Reports total bytes and items. UI shows "Scanning..." during this phase.
2. **Conflict detection**: For each file, checks if the destination already exists:
   - File → File: Offers Skip/Overwrite.
   - Directory → Directory: Merges (copies contents into existing directory without error).
   - File → Directory or Directory → File (type mismatch): Error, offers Skip.
3. **Copy strategies** (cascading fallback for cross-VFS compatibility):
   - Same-VFS `copy_within` (fastest, if available).
   - Sync read + sync write (64 KB chunks).
   - Async read + async write.
   - Mixed sync/async bridges for VFS combinations that don't match.
4. **Metadata preservation**: After copying, optionally sets permissions, timestamps, owner, and group on the destination. Silently skipped if the destination VFS doesn't support metadata operations.
5. **Progress**: Reports every 100ms with bytes done, items done, and current filename.

**Symlink handling**: With "Create symbolic link" checked, creates a symlink directly (no file content copied). Only available for single files.

**Cross-VFS copies**: Fully supported. You can copy files between any combination of Local, S3, SFTP, and Archive VFS types. The system automatically selects the appropriate read/write strategy.

### Move (F6)

Same dialog and options as Copy (except "Create symbolic link" is not available).

**Move execution**:
1. **Try fast rename** (same VFS only): Attempts atomic rename for each source. Instant if it works.
2. **Fallback to copy+delete**: If rename fails (cross-device, cross-VFS, permission error), falls back to copying each file and immediately deleting the source after successful copy. After all files are copied, empty source directories are removed in reverse order (deepest first). Directories that still contain files (because some copies were skipped) are left intact.

### Properties (Alt+Enter)

Modal dialog showing file metadata. Supports single files and multi-file selections.

**Information displayed**:
- Name
- Size (human-readable + exact byte count)
- Type (file / directory / symlink)
- Symlink target path (if applicable)
- Owner (name or UID)
- Group (name or GID)
- Mode (Unix permissions)
- Modified, Accessed, Created timestamps (locale-formatted)

**Permission editor** (when applicable):
- 3×3 checkbox grid: Owner/Group/Other × Read/Write/Execute.
- Special bits row: Set UID, Set GID, Sticky.
- Octal notation display that updates live (e.g., "0755").
- **Recursive** checkbox (for directories): Applies permissions to all contents.
- **Multi-file selections**: Shows "(mixed)" for permission bits that differ across selected files. Checking/unchecking a mixed bit applies uniformly to all selected files.

### Clipboard Operations

- **Copy Path** (Mod+C): Copies the paths of all selected files (or the focused file if none selected) to the system clipboard.
- **Paste** (Mod+V): Pastes file paths from the system clipboard into the current pane.

### Operation Progress and Issue Resolution

When a copy, move, or delete operation runs, it's tracked in the **Operations Panel**:

**Foreground modal** (default for new operations):
- Large overlay showing operation kind, description, progress bar, percentage, and the current file being processed.
- **Cancel** button: Stops the operation. Partially copied files are left as-is.
- **Background** button: Minimizes the operation to the compact panel, freeing the UI for other work.

**Background panel** (compact list):
- Shows all backgrounded operations as a compact list.
- Each operation shows: kind, description, progress bar, percentage.
- Cancel and Dismiss buttons per operation.

**Operation states**: Scanning → Running → Completed / Failed / Cancelled.
- Completed and Cancelled operations are automatically removed from the panel.
- Failed operations persist with an error message until dismissed.

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

### Text Mode

- Line-numbered display with a non-selectable gutter. Gutter width adjusts to fit the number of digits in the total line count.
- **Chunked loading**: Loads files in 128 KB chunks. Large files don't need to be fully loaded before viewing.
- **UTF-8 aware**: Detects incomplete UTF-8 sequences at chunk boundaries and handles them gracefully.
- **Virtual scrolling**: Only renders visible lines plus 5-line overscan for smooth scrolling.
- **Text is selectable** (`<pre>` element with user-select).

**Keyboard**:
| Key | Action |
|-----|--------|
| Arrow Up/Down | Scroll one line |
| Page Up/Down | Scroll one page |
| Home | Jump to start of file |
| End | Jump to end of file |
| Escape | Close viewer |

**Status bar**: `path/to/file.txt | Text | Line 42 / 1250+ | 125.4 KB | Loading 45%`

The `+` after the line count indicates the file is still loading. Loading percentage shows progress.

### Hex Mode

- Classic hex dump layout: offset column (8 hex digits) | 16 hex bytes (grouped 8+8 with a gap) | ASCII representation.
- Non-printable bytes shown as `.` in the ASCII column. Printable range: 0x20–0x7E.
- **Virtual scrolling** with max scroll height clamping (prevents browser rendering issues with very tall elements).
- **On-demand chunk loading**: 128 KB chunks cached in memory and loaded as the user scrolls. Preloads chunks for the visible viewport and overscan area.
- **Mouse wheel**: Handles both pixel-mode and line-mode scroll deltas. Accumulates sub-row pixel deltas across events to snap to row boundaries.

**Keyboard**: Same as Text mode.

**Status bar**: `path/to/file.bin | Hex | Offset 00000A20 / 000FFFFF | 1.0 MB`

### Image Mode

- Displays the image centered, initially fit to the window (aspect ratio preserved).
- **Zoom**: Mouse wheel zooms in/out, centered on the cursor position. Factor: ×1.11 per wheel tick. Min zoom = fit-to-window (or 100% if image is smaller). Max zoom = 50×.
- **Pan**: Left-click or middle-click drag to pan when zoomed in. Pan is clamped to keep the image visible (no empty edges).
- **Reset**: Press `0` (zero) to reset to fit-to-window.
- **Escape**: Close viewer.

**Status bar**: `path/to/image.png | Image | 1920×1080 | 150% | 2.4 MB`

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

- Embedded in an `<iframe>` using the browser's built-in PDF viewer (typically PDF.js).
- Fills the entire window. Native PDF zoom, scroll, and search.
- **Escape** is handled via the native window menu accelerator (not a keyboard event listener, since the iframe captures events).

**Status bar**: `path/to/document.pdf | PDF | 2.3 MB`

### Viewer Menu Bar

- **File**: Close (Escape)
- **View**: Text / Hex / Image / Audio / Video / PDF (radio buttons — one always checked)

### File Serving

The viewer and editor access files through an internal HTTP server on localhost (random port, token-protected). This supports:
- Range requests (HTTP 206) for chunked loading.
- 1 MB streaming chunks to avoid buffering entire files.
- MIME type detection for proper content-type headers.

---

## 6. File Editor (F4)

Opens in a separate window (900×700 pixels) using Monaco Editor (the editor core from VS Code).

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
- **Close with unsaved changes**: A warning confirmation dialog appears: "You have unsaved changes. Close without saving?" User must confirm to discard changes.
- **Close without unsaved changes**: Window closes immediately.

### Editor Menu Bar

- **File**: Save (Ctrl+S), Close (Ctrl+W)
- **View**: Word Wrap (checkbox toggle)
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

- Collapsible panel at the bottom of the main window.
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
- **Working directory**: New terminals inherit the current directory of the active pane.
- **Shell**: System default shell (from passwd database or `$SHELL` environment variable).
- **Environment**: Sets `TERM=xterm-256color`, `COLORTERM=truecolor`.
- **Responsive**: Automatically resizes when the panel is resized (via ResizeObserver + FitAddon).

### Theming

Terminal colors follow the system/app theme:
- Separate light and dark color palettes (VSCode-inspired).
- Theme updates reactively when the OS color scheme changes.
- Checks `document.documentElement.dataset.theme` first (explicit app override), then falls back to `prefers-color-scheme` media query.

### Copy/Paste

- **Copy**: Ctrl+Shift+C (or Cmd+C on macOS) copies the terminal selection to the system clipboard.
- **Paste**: Handled by xterm.js built-in paste support.
- **Selection**: Highlight text with the mouse to select. Text is selectable by default.

### Terminal Lifecycle

- **Running**: Full interactivity. Input goes to the shell, output is displayed.
- **Defunct/Exited**: When the shell process exits:
  - If `behavior.keep_terminal_open` is **true** (default): The tab stays open, showing a dimmed message: `[Process exited with code X. Press Enter to close.]` (or signal name if killed). User presses Enter to close the tab.
  - If `behavior.keep_terminal_open` is **false**: The tab is automatically removed. If it was the active terminal, the next terminal becomes active. If no terminals remain, the terminal panel hides.

### Terminal in Remote/Elevated Mode

Terminals in remote sessions run on the remote host. The PTY is allocated by the agent process. Terminal I/O is forwarded over the RPC protocol. From the user's perspective, the terminal behaves identically to local mode.

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

---

## 8. VFS (Virtual Filesystem) Support

All filesystem access goes through trait abstractions. Multiple VFS types can be mounted simultaneously and accessed independently from either pane.

### Local Filesystem (always mounted, VFS ID 0)

- Full read/write support.
- File watching: Panes automatically refresh when the underlying directory changes on disk.
- All operations supported: rename, hard link, symlink creation, metadata (permissions, timestamps, owner, group), filesystem stats.

### S3 (Amazon S3 / S3-Compatible)

**Mounting**: Via command palette ("Mount S3") or VFS selector dropdown. Uses ambient AWS credentials (environment variables, IAM role, AWS profiles). Optional region parameter.

**Browsing**:
- Root (`/`) lists all buckets.
- Bucket contents are listed using `ListObjectsV2` with delimiter, simulating a directory structure via common prefixes.
- "Directories" in S3 are virtual (based on `/` separators in object keys). Created directories are 0-byte objects with trailing `/`.

**Operations supported**: Read, write (multipart upload with 10 MB chunks), create directory, delete, copy within the same S3 bucket, touch.

**Operations NOT supported**: Rename, hard link, symlink, permissions, filesystem stats.

**Display path**: `s3://bucket/prefix/key`

**Breadcrumbs**: `s3:// → bucket → prefix → key`

**In remote sessions**: S3 connections originate from the remote host, using the remote host's AWS credentials and network.

### SFTP

**Mounting**: Via dialog (Mod+Shift+L → SFTP, or "Mount SFTP" in command palette) with `user@hostname` input.

**Connection**: Spawns an SSH process (`ssh <host> -s sftp`) with stdin/stdout piped. SFTP handshake happens over the SSH connection. 30-second timeout on connection.

**Authentication**: Relies on the SSH client's configuration:
- Public key (SSH agent, key files).
- Password (via askpass dialog — see Connection Management).
- Keyboard-interactive.
- SSH config file (`~/.ssh/config`) is respected.
- Host key verification prompts appear as in-app dialogs.

**Operations supported**: Read, write, rename, create directory, delete, symlink creation, hard link, metadata (permissions, timestamps, owner, group), file watching.

**Operations NOT supported**: Copy within SFTP (cross-file copy goes through the host), filesystem stats.

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

**ZIP indexing** (one-shot):
- The complete index is built before displaying. ZIP files allow random access, so individual files are extracted on demand.

**Navigation out of archives**:
- Pressing `..` at the archive root exits the archive and returns to the parent directory containing the archive file.
- The archive filename itself is focused after exiting.
- Breadcrumbs show the full path including the origin: clicking archive-level breadcrumbs exits back to the origin filesystem.

**Nested archives**: Archives can contain other archives. Opening an inner archive creates a new VFS mount with the outer archive as its origin. The cleanup system prevents unmounting a parent archive while a child archive is still open.

**Stale mount cleanup**: Archive mounts are automatically removed when no pane references them (or their child archives).

**Limitations**: Read-only. No create, modify, delete, rename, or metadata changes inside archives. Tar archives support symlinks; ZIP archives do not.

### VFS Selector Dialog (Mod+Shift+L)

- Lists all currently **mounted** VFS instances (with VFS ID, type, and mount label).
- Lists **available** VFS types to mount:
  - S3: Mounts immediately on selection (uses ambient credentials).
  - SFTP: Opens hostname input dialog.
- **Unmount button** (×) on mounted VFSes (except Local).
- Mount labels: S3 shows nothing extra, SFTP shows hostname, Archives show the source file path.

---

## 9. Session and Connection Management

### Local Mode (Default)

All operations run directly in the Tauri process. No agent subprocess, no serialization, no network. This is the default when launching Newt without arguments.

### Remote Mode (SSH)

**Connecting**: Via dialog (Mod+Shift+R) or command line (`newt --connect user@host`).

**Bootstrap protocol**:
1. Newt spawns an SSH process and sends a bootstrap shell script to the remote host.
2. The script detects the remote platform and architecture (`uname -s`, `uname -m`).
3. It checks a cache directory (`~/.cache/newt/`) for a matching `newt-agent` binary (keyed by a blake3 hash of the local agent binary).
4. If cached and current: Executes immediately (`NEWT:READY`).
5. If missing or outdated: Requests upload (`NEWT:NEED:triple:caps`). Newt compresses the agent binary with gzip (if remote supports it) and uploads it. The script caches it for future use and cleans up old versions.
6. The agent enters RPC mode, and all further communication happens over the binary RPC protocol (bincode over stdin/stdout).

**After connection**: All filesystem operations, terminal PTYs, file operations, and VFS mounts execute on the remote host. The UI is identical to local mode — the abstraction is transparent.

**Connection logging**: Every step (SSH negotiation, bootstrap progress, agent startup) is logged. The Connection Log dialog shows this log in real-time.

**SSH stderr**: Captured in a background task and appended to the connection log. Useful for debugging authentication failures.

**Process safety** (Linux): The SSH process has `PR_SET_PDEATHSIG=SIGTERM` set, so if the Tauri process crashes, the remote agent is automatically killed. Prevents zombie processes on the remote host.

### Elevated Mode (pkexec — Linux only)

**Connecting**: Via command palette ("Open Elevated").

Spawns `pkexec <agent-binary-path>`. The system's privilege escalation dialog (e.g., Polkit) prompts for the user's password. The agent runs as root, providing full filesystem access to the entire system.

### Connection Status

Displayed as an overlay on the main window during connection:
- **Connecting**: Shows progress message and log.
- **Connected**: Overlay disappears, normal operation.
- **Disconnected**: Shows error message and a "Reconnect" button.
- **Failed**: Shows error details and connection log.

### Askpass Integration

When SSH needs interactive input (password, passphrase, host key verification), Newt handles it entirely within the app:

1. SSH invokes the askpass helper (the `newt-agent` binary in askpass mode, set via `SSH_ASKPASS` environment variable).
2. The helper connects to a Unix domain socket that Newt is listening on.
3. Newt displays a modal dialog with the SSH prompt.
4. The dialog shows:
   - **Title**: "Host Key Verification" (for host key prompts containing "yes/no"), "Authentication" (for passwords), or "SSH" (for other prompts).
   - **Input field**: Password field (masked) for secrets, text field for confirmations.
   - For host key confirmation: submitting empty input defaults to "yes".
5. The user's response is sent back through the socket to SSH, and authentication continues.

### Reconnect

After disconnection, a "Reconnect" button appears. Clicking it re-establishes the connection using the same transport parameters (SSH host, elevated mode, etc.).

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
| Mounted Volumes | Entries in `/proc/self/mountinfo` filtered to `/media/`, `/run/media/`, `/mnt/` on Linux; `/Volumes` on macOS |
| Mounted VFS | Currently mounted S3, SFTP, and archive filesystems |
| Recent Folders | `recently-used.xbel` on Linux (top 20 by modification time); Finder GoToFieldHistory on macOS |

Each category can be independently toggled on/off in preferences (Hot Paths section).

**Each entry displays**: Name (if bookmarked or named) + path. Matching characters in the fuzzy search are highlighted.

**Keyboard navigation**: Arrow keys, Page Up/Down, Home/End, Enter to navigate to the selected path, Escape to close.

### Bookmark Operations

- **Add Bookmark** (Mod+B): Bookmarks the active pane's current directory. Optional custom name (defaults to the directory name). Stored as `[[bookmark]]` in `settings.toml`.
- **Remove Bookmark**: Press Delete on a user bookmark in the Hot Paths dialog. Shows an inline confirmation (Yes/No) — during confirmation, all other keys are swallowed except Enter/Y (confirm), N (cancel), and Escape (cancel).

---

## 11. Command Palette (Mod+Shift+P)

Fuzzy-searchable list of all available commands.

- **Search input** (auto-focused): "Start typing to filter commands".
- **Fuzzy matching**: Same algorithm as Hot Paths.
- **Context filtering**:
  - Commands with `needs_pane = true` are hidden when no pane is focused.
  - Commands with a `when` condition are evaluated against current state:
    - `"file"`: Only if focused item is a regular file.
    - `"directory"`: Only if focused item is a directory.
    - `"selection"`: Only if files are selected, or a non-`..` file is focused.
    - `"pane_focused"`: Only if a file pane has focus.
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
key = "alt+z"         # Optional keyboard shortcut
terminal = true       # true = run in terminal tab, false = run as background operation
when = "selection"    # Optional: "any", "file", "directory", "selection"
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

**Operation mode** (`terminal = false`):
- Renders the template into a command string.
- Executes as a background operation (same as copy/move/delete).
- Shows progress in the Operations Panel.
- Can be backgrounded.

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

Located at `~/.config/newt/settings.toml`. Hot-reloaded — changes to the file are picked up within 200ms and applied without restart.

### Full Settings Structure

```toml
profile = "work"  # Optional: loads ~/.config/newt/profiles/work.toml overlay

[appearance]
show_hidden = false         # Show files starting with "."
folders_first = true        # Directories before files in sort order
show_command_bar = true     # Show F-key bar at bottom of window
theme = "system"            # "system", "light", or "dark"

[behavior]
confirm_delete = true       # Ask for confirmation before deleting
keep_terminal_open = true   # Keep terminal tab open after shell exits

[hot_paths]
standard_folders = true     # Show Home, Downloads, Documents, etc.
system_bookmarks = true     # Show GTK bookmarks (Linux)
mounts = true               # Show mounted volumes
recent_folders = true       # Show recently visited directories

[[bookmark]]
path = "/home/user/projects"
name = "My Projects"        # Optional

[[bind]]
key = "mod+shift+f5"
command = "some_command"
when = "pane_focused"       # Optional

[[command]]
title = "My Command"
run = "echo {{ file.name }}"
key = "alt+z"               # Optional
terminal = true             # Optional, default false
when = "file"               # Optional
```

### Profile System

The `profile` field in `settings.toml` loads an additional TOML file from `~/.config/newt/profiles/<name>.toml`. Profile settings deep-merge on top of user settings (scalars are replaced, tables are merged).

### Settings Dialog (Mod+,)

Three tabs:

**Settings tab**:
- Sidebar with category filter (All, Appearance, Behavior, Hot Paths).
- Search box for full-text search across setting titles and descriptions.
- Each setting rendered as a row with title, description, and appropriate control:
  - Boolean → checkbox.
  - Enum → dropdown.
  - Number → number input.
  - String → text input.
- Changes are saved immediately to `settings.toml`.

**Keybindings tab**:
- Table listing all commands with their current shortcut and `when` condition.
- Search/filter by command name, ID, or shortcut.
- Shortcuts rendered with platform-specific symbols (⌘ on macOS, Ctrl elsewhere).

**Commands tab**:
- List of user-defined commands showing title, run script (monospace code block), key, when condition, and terminal flag.
- Edit and Delete buttons per command.
- Edit form: title (text), run (textarea, monospace), key (text, placeholder "e.g. alt+z"), when (dropdown: Any/File/Directory/Selection), terminal (checkbox).
- Expandable template reference panel showing variables, filters, and functions with examples.

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

**`when` conditions**: Gate when a binding is active:
- (omitted or `"any"`) → Always available.
- `"pane_focused"` → Only when a file pane has focus.
- `"file"` → Only when focused item is a regular file.
- `"directory"` → Only when focused item is a directory.
- `"selection"` → Only when files are selected (or a non-`..` file is focused).

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
- Shell expansion (`~`, `$HOME/documents`).
- VFS display paths (`s3://bucket/path`, `sftp://host/path`).

Path resolution priority: First checks if any mounted VFS claims the path (e.g., `s3://` prefix), then falls back to shell expansion for local paths.

### Navigation History

Each pane maintains its own navigation history (path + focused filename):

- **Back** (Alt+Left): Return to the previous directory. The previously focused file is restored.
- **Forward** (Alt+Right): Go forward after going back.

History entries store both the path and the focused filename, so navigating back restores your exact cursor position.

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
| F6 | Move to other pane | Pane focused |
| F7 | Create directory | Pane focused |
| F8 | Delete selected | Pane focused |
| Shift+Delete | Delete selected (alternative) | Pane focused |
| Cmd+Backspace | Delete selected (macOS alternative) | Pane focused |
| Alt+Enter | Properties | Pane focused |

### Navigation

| Shortcut | Action | Context |
|----------|--------|---------|
| Enter | Open / enter directory | Pane focused |
| Backspace | Parent directory | Pane focused |
| Tab | Switch panes | Pane focused |
| Shift+Enter | Follow symlink | Pane focused |
| Mod+L | Navigate (Go To...) | Pane focused |
| Alt+Left | Navigate back | Pane focused |
| Alt+Right | Navigate forward | Pane focused |
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
| / | Enter filter mode | Pane focused |
| (any printable char) | Start quick search | Pane focused |
| Escape | Cancel / clear filter | Pane focused |

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
| Mod+Shift+P | Command palette | Any |
| F9 | User commands palette | Pane focused |
| Shift+F10 / Menu | Context menu | Pane focused |

### Window

| Shortcut | Action | Context |
|----------|--------|---------|
| Mod+N | New window | Any |
| Mod+W | Close window | Any |
| Mod+Shift+R | Connect remote | Any |
| Ctrl+= | Zoom in | Any |
| Ctrl+- | Zoom out | Any |
| Ctrl+0 | Reset zoom | Any |

All keybindings are fully customizable via the Settings dialog or `settings.toml`. `Mod` = Ctrl on Linux/Windows, Cmd on macOS.
