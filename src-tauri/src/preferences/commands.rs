/// Which window's dispatcher owns a command. Main-window commands dispatch
/// as `cmd_<id>` Tauri commands; viewer/editor commands are handled locally
/// by the owning window's frontend — the registry only supplies identity,
/// default keys, and rebindability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum CommandScope {
    Main,
    Viewer,
    Editor,
}

/// Static command definitions — the source of truth for all commands.
#[derive(Debug, Clone)]
pub struct CommandDef {
    pub id: String,
    pub name: String,
    pub short_name: Option<String>,
    pub category: String,
    pub default_keys: Vec<String>,
    pub default_when: Option<String>,
    pub needs_pane: bool,
    pub scope: CommandScope,
}

/// Like `vec![]` but supports `@if(expr) { ... }` blocks for
/// runtime-conditional items and `@cfg(pred) { ... }` blocks for
/// compile-time conditional items.
macro_rules! conditional_vec {
    (@inner $v:ident,) => {};
    // @cfg(pred) { items... }, rest...
    (@inner $v:ident, @cfg($pred:meta) { $($item:expr),* $(,)? } $($rest:tt)*) => {
        $(
            #[cfg($pred)]
            $v.push($item);
        )*
        conditional_vec!(@inner $v, $($rest)*);
    };
    // @if(expr) { items... }, rest...
    (@inner $v:ident, @if($cond:expr) { $($item:expr),* $(,)? } $($rest:tt)*) => {
        if $cond {
            $( $v.push($item); )*
        }
        conditional_vec!(@inner $v, $($rest)*);
    };
    // regular item, rest...
    (@inner $v:ident, $item:expr, $($rest:tt)*) => {
        $v.push($item);
        conditional_vec!(@inner $v, $($rest)*);
    };
    // entry point
    ($($body:tt)*) => {{
        let mut v: Vec<_> = Vec::with_capacity(64);
        conditional_vec!(@inner v, $($body)*);
        v
    }};
}

pub fn default_commands() -> Vec<CommandDef> {
    conditional_vec![
        CommandDef {
            id: "new_window".into(),
            name: "New Window".into(),
            short_name: None,
            category: "File".into(),
            default_keys: vec!["mod+n".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "as_other_pane".into(),
            name: "As Other Pane".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec!["mod+.".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "swap_panes".into(),
            name: "Swap Panes".into(),
            short_name: Some("Swap".into()),
            category: "Navigation".into(),
            default_keys: vec!["mod+u".into()],
            // Ctrl+U is kill-line in every readline-derived shell.
            default_when: Some("pane_focused".into()),
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "open_in_left_pane".into(),
            name: "Open in Left Pane".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec!["mod+left".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "open_in_right_pane".into(),
            name: "Open in Right Pane".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec!["mod+right".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "select_all".into(),
            name: "Select All".into(),
            short_name: None,
            category: "Selection".into(),
            default_keys: vec!["mod+a".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "deselect_all".into(),
            name: "Clear Selection".into(),
            short_name: None,
            category: "Selection".into(),
            default_keys: vec!["mod+d".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "invert_selection".into(),
            name: "Invert Selection".into(),
            short_name: None,
            category: "Selection".into(),
            default_keys: vec!["numpad_multiply".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        // Numpad operators only (Norton/TC convention): the main-row
        // `+`/`-`/`*` stay printable and go to quick search.
        CommandDef {
            id: "select_by_pattern".into(),
            name: "Select by Pattern...".into(),
            short_name: None,
            category: "Selection".into(),
            default_keys: vec!["numpad_add".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "deselect_by_pattern".into(),
            name: "Deselect by Pattern...".into(),
            short_name: None,
            category: "Selection".into(),
            default_keys: vec!["numpad_subtract".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "select_same_extension".into(),
            name: "Select Same Extension".into(),
            short_name: None,
            category: "Selection".into(),
            default_keys: vec![],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "view".into(),
            name: "View".into(),
            short_name: None,
            category: "File".into(),
            default_keys: vec!["f3".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "edit".into(),
            name: "Edit".into(),
            short_name: None,
            category: "File".into(),
            default_keys: vec!["f4".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "rename".into(),
            name: "Rename...".into(),
            short_name: Some("Rename".into()),
            category: "File".into(),
            default_keys: vec!["f2".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "properties".into(),
            name: "File Properties...".into(),
            short_name: Some("Props".into()),
            category: "File".into(),
            default_keys: vec!["alt+enter".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "delete_selected".into(),
            name: "Delete Selected".into(),
            short_name: Some("Delete".into()),
            category: "File".into(),
            // Finder conventions on macOS: ⌘⌫ = Move to Trash.
            default_keys: if cfg!(target_os = "macos") {
                vec!["f8".into(), "delete".into(), "meta+backspace".into()]
            } else {
                vec!["f8".into(), "delete".into()]
            },
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "delete_permanent".into(),
            name: "Delete Permanently".into(),
            short_name: None,
            category: "File".into(),
            // Finder conventions on macOS: ⌥⌘⌫ = Delete Immediately.
            default_keys: if cfg!(target_os = "macos") {
                vec!["shift+delete".into(), "meta+alt+backspace".into()]
            } else {
                vec!["shift+delete".into()]
            },
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "create_directory".into(),
            name: "Create Directory...".into(),
            short_name: Some("MkDir".into()),
            category: "File".into(),
            default_keys: vec!["f7".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "create_file".into(),
            name: "Create File...".into(),
            short_name: Some("MkFile".into()),
            category: "File".into(),
            default_keys: vec![],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "create_and_edit".into(),
            name: "Create and Edit File...".into(),
            short_name: Some("New+Edit".into()),
            category: "File".into(),
            default_keys: vec!["shift+f4".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "navigate".into(),
            name: "Go To...".into(),
            short_name: Some("Go To".into()),
            category: "Navigation".into(),
            default_keys: vec!["mod+l".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "open".into(),
            name: "Open in Default App".into(),
            short_name: Some("Open".into()),
            category: "File".into(),
            default_keys: vec![],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "follow_symlink".into(),
            // Also follows the alias for synthetic-VFS entries (search
            // results) — i.e. reveals the underlying file in the source
            // VFS. Same key, same intent: "take me to where this points
            // to".
            name: "Follow Symlink / Reveal Source".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec!["shift+enter".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "navigate_back".into(),
            name: "Go Back".into(),
            short_name: Some("Back".into()),
            category: "Navigation".into(),
            // Default keybinding lives on history_back, which opens the
            // overlay; this command remains available via the command palette
            // and the mouse back button (XButton1) for instant single-step nav.
            default_keys: vec![],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "navigate_forward".into(),
            name: "Go Forward".into(),
            short_name: Some("Forward".into()),
            category: "Navigation".into(),
            default_keys: vec![],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "history_back".into(),
            name: "Back...".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec!["alt+left".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "history_forward".into(),
            name: "Forward...".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec!["alt+right".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "history".into(),
            name: "Show History...".into(),
            short_name: Some("History".into()),
            category: "Navigation".into(),
            default_keys: vec!["mod+y".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "send_to_terminal".into(),
            name: "Open in Terminal".into(),
            short_name: Some("Terminal".into()),
            category: "Terminal".into(),
            default_keys: vec!["mod+enter".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "copy".into(),
            name: "Copy to Other Pane...".into(),
            short_name: Some("Copy".into()),
            category: "File".into(),
            default_keys: vec!["f5".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "move".into(),
            name: "Move to Other Pane...".into(),
            short_name: Some("Move".into()),
            category: "File".into(),
            default_keys: vec!["f6".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "create_archive".into(),
            name: "Pack to Archive...".into(),
            short_name: Some("Pack".into()),
            category: "File".into(),
            default_keys: vec!["alt+f5".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "copy_to_clipboard".into(),
            name: "Copy Path to Clipboard".into(),
            short_name: Some("CopyPath".into()),
            category: "Edit".into(),
            // Ctrl+Ins is the CUA-era clipboard alias; macOS has no Insert key.
            default_keys: if cfg!(target_os = "macos") {
                vec!["mod+c".into()]
            } else {
                vec!["mod+c".into(), "ctrl+insert".into()]
            },
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "paste_from_clipboard".into(),
            name: "Paste Path from Clipboard".into(),
            short_name: Some("PastePath".into()),
            category: "Edit".into(),
            default_keys: if cfg!(target_os = "macos") {
                vec!["mod+v".into()]
            } else {
                vec!["mod+v".into(), "shift+insert".into()]
            },
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "show_next_operation".into(),
            name: "Show Next Operation".into(),
            short_name: Some("Next Op".into()),
            category: "View".into(),
            default_keys: vec!["f10".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "toggle_hidden".into(),
            name: "Toggle Hidden Files".into(),
            short_name: Some("Hidden".into()),
            category: "View".into(),
            default_keys: vec!["mod+h".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "sort".into(),
            name: "Sort...".into(),
            short_name: Some("Sort".into()),
            category: "View".into(),
            default_keys: vec!["mod+shift+s".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "close_window".into(),
            name: "Close Window".into(),
            short_name: Some("Close".into()),
            category: "File".into(),
            // Shifted on every platform (gnome-terminal convention): a
            // session window is too heavy to close on a stray mod+w.
            default_keys: vec!["mod+shift+w".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "quit".into(),
            name: "Quit Newt".into(),
            short_name: None,
            category: "File".into(),
            default_keys: vec!["mod+q".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "refresh".into(),
            name: "Refresh File List".into(),
            short_name: Some("Refresh".into()),
            category: "View".into(),
            default_keys: vec![],
            default_when: None,
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "compute_size".into(),
            name: "Calculate Size".into(),
            short_name: Some("Size".into()),
            category: "View".into(),
            default_keys: vec!["mod+shift+enter".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "compute_all_sizes".into(),
            name: "Calculate All Sizes".into(),
            short_name: None,
            category: "View".into(),
            // Canonical modifier order is meta,ctrl,shift,alt — an
            // "alt+shift+…" spelling never matches a real keypress.
            default_keys: vec!["shift+alt+enter".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "quick_connect".into(),
            name: "Quick Connect...".into(),
            short_name: Some("Connect".into()),
            category: "File".into(),
            default_keys: vec!["ctrl+r".into()],
            default_when: None,
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "connect_remote".into(),
            name: "Connect to Remote Host...".into(),
            short_name: Some("Remote".into()),
            category: "File".into(),
            default_keys: vec!["mod+shift+r".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        @cfg(any(target_os = "linux", windows)) {
            CommandDef {
                id: "open_elevated".into(),
                name: "Open Elevated".into(),
                short_name: None,
                category: "File".into(),
                default_keys: vec![],
                default_when: None,
                needs_pane: false,
            scope: CommandScope::Main,
            },
        }
        @cfg(windows) {
            CommandDef {
                id: "connect_wsl".into(),
                name: "Connect to WSL Distribution...".into(),
                short_name: Some("WSL".into()),
                category: "File".into(),
                default_keys: vec![],
                default_when: None,
                needs_pane: false,
            scope: CommandScope::Main,
            },
            CommandDef {
                id: "map_network_drive".into(),
                name: "Map Network Drive...".into(),
                short_name: None,
                category: "Navigation".into(),
                default_keys: vec!["f11".into()],
                default_when: None,
                needs_pane: false,
            scope: CommandScope::Main,
            },
            CommandDef {
                id: "unmap_network_drive".into(),
                name: "Unmap Network Drive...".into(),
                short_name: None,
                category: "Navigation".into(),
                default_keys: vec!["alt+f11".into()],
                default_when: Some("pane_focused".into()),
                needs_pane: true,
            scope: CommandScope::Main,
            },
        }
        CommandDef {
            id: "select_vfs".into(),
            name: "Select Filesystem...".into(),
            short_name: Some("VFS".into()),
            category: "Navigation".into(),
            default_keys: vec!["mod+shift+l".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "mount_s3".into(),
            name: "Mount S3...".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec![],
            default_when: None,
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "mount_sftp".into(),
            name: "Mount SFTP...".into(),
            short_name: Some("SFTP".into()),
            category: "Navigation".into(),
            default_keys: vec![],
            default_when: None,
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "navigate_root".into(),
            name: "Go to Root".into(),
            short_name: Some("Root".into()),
            category: "Navigation".into(),
            // Unbound on macOS: ⌘⌫ is aliased to `delete_selected` there
            // (Finder's Move to Trash — see `resolve_bindings`).
            default_keys: if cfg!(target_os = "macos") {
                vec![]
            } else {
                vec!["mod+backspace".into()]
            },
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "start_filter".into(),
            name: "Filter Files".into(),
            short_name: Some("Filter".into()),
            category: "View".into(),
            default_keys: vec!["/".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "start_search".into(),
            name: "Find in Folder...".into(),
            short_name: Some("Find".into()),
            category: "Navigation".into(),
            default_keys: vec!["mod+f".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "unmount_vfs".into(),
            name: "Disconnect VFS".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec![],
            default_when: None,
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "open_folder".into(),
            name: "Open Folder in Default File Manager".into(),
            short_name: Some("Reveal".into()),
            category: "File".into(),
            default_keys: vec!["shift+f3".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "toggle_terminal_panel".into(),
            name: "Toggle Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_keys: vec!["ctrl+`".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "focus_panes".into(),
            name: "Focus File Panes".into(),
            short_name: Some("Panes".into()),
            category: "Navigation".into(),
            default_keys: vec!["alt+up".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "focus_terminal".into(),
            name: "Focus Terminal".into(),
            short_name: Some("Terminal".into()),
            category: "Navigation".into(),
            default_keys: vec!["alt+down".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "create_terminal".into(),
            name: "New Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_keys: vec!["ctrl+shift+~".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "next_terminal".into(),
            name: "Next Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_keys: vec!["ctrl+pagedown".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "prev_terminal".into(),
            name: "Previous Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_keys: vec!["ctrl+pageup".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "open_settings".into(),
            name: "Settings...".into(),
            short_name: Some("Settings".into()),
            category: "File".into(),
            default_keys: vec!["mod+,".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "command_palette".into(),
            name: "Command Palette...".into(),
            short_name: Some("CmdPalette".into()),
            category: "View".into(),
            default_keys: vec!["f1".into(), "mod+shift+p".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "hot_paths".into(),
            name: "Hot Paths...".into(),
            short_name: None,
            category: "Navigation".into(),
            default_keys: vec!["mod+p".into()],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "user_commands".into(),
            name: "User Commands...".into(),
            short_name: None,
            category: "View".into(),
            default_keys: vec!["f9".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "add_bookmark".into(),
            name: "Add Current Path to Bookmarks".into(),
            short_name: Some("Bookmark".into()),
            category: "Navigation".into(),
            default_keys: vec!["mod+b".into()],
            default_when: Some("pane_focused".into()),
            needs_pane: true,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "connection_log".into(),
            name: "Connection Log...".into(),
            short_name: None,
            category: "View".into(),
            default_keys: vec![],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        @if(cfg!(debug_assertions)) {
            CommandDef {
                id: "debug".into(),
                name: "Debug...".into(),
                short_name: None,
                category: "View".into(),
                default_keys: vec!["f12".into()],
                default_when: None,
                needs_pane: false,
            scope: CommandScope::Main,
            },
        }
        CommandDef {
            id: "documentation".into(),
            name: "Documentation...".into(),
            short_name: Some("Docs".into()),
            category: "Help".into(),
            default_keys: vec![],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        CommandDef {
            id: "about".into(),
            name: "About Newt...".into(),
            short_name: Some("About".into()),
            category: "Help".into(),
            default_keys: vec![],
            default_when: None,
            needs_pane: false,
            scope: CommandScope::Main,
        },
        // Viewer (F3) window commands — dispatched by the viewer frontend,
        // not cmd_*. Fundamental keys (Escape to close, arrow/PgUp/PgDn
        // panning and scrolling) are intentionally not commands and cannot
        // be rebound. A command listed here fires only in the modes whose
        // component registers a handler for it.
        viewer_command("viewer_toggle_hex", "Viewer: Toggle Hex View", &["f3"]),
        viewer_command("viewer_copy", "Viewer: Copy", &["mod+c"]),
        viewer_command("viewer_select_all", "Viewer: Select All", &["mod+a"]),
        viewer_command("viewer_find", "Viewer: Find", &["mod+f"]),
        viewer_command("viewer_goto", "Viewer: Go to Line/Offset", &["mod+g"]),
        // "+" is shift+= on US-like layouts and a bare "+" on numpads;
        // shift+- yields "_".
        viewer_command("viewer_zoom_in", "Viewer: Zoom In", &["=", "shift+=", "+"]),
        viewer_command("viewer_zoom_out", "Viewer: Zoom Out", &["-", "shift+_"]),
        viewer_command("viewer_zoom_fit", "Viewer: Zoom to Fit", &["0"]),
        viewer_command("viewer_zoom_actual", "Viewer: Actual Size", &["1"]),
        viewer_command("viewer_rotate_cw", "Viewer: Rotate Clockwise", &["r"]),
        viewer_command(
            "viewer_rotate_ccw",
            "Viewer: Rotate Counter-Clockwise",
            &["shift+r"],
        ),
        viewer_command("viewer_flip_horizontal", "Viewer: Flip Horizontal", &["h"]),
        viewer_command("viewer_flip_vertical", "Viewer: Flip Vertical", &["v"]),
        viewer_command("viewer_cycle_background", "Viewer: Cycle Background", &["b"]),
        viewer_command("viewer_toggle_info", "Viewer: Image Info", &["i"]),
        CommandDef {
            id: "editor_save".into(),
            name: "Editor: Save".into(),
            short_name: None,
            category: "Editor".into(),
            default_keys: vec!["mod+s".into()],
            default_when: Some("editor".into()),
            needs_pane: false,
            scope: CommandScope::Editor,
        },
    ]
}

fn viewer_command(id: &str, name: &str, default_keys: &[&str]) -> CommandDef {
    CommandDef {
        id: id.into(),
        name: name.into(),
        short_name: None,
        category: "Viewer".into(),
        default_keys: default_keys.iter().map(|&k| k.into()).collect(),
        default_when: Some("viewer".into()),
        needs_pane: false,
        scope: CommandScope::Viewer,
    }
}
