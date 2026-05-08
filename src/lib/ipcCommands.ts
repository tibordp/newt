/// String constants for every Tauri IPC command name and every dialog kind.
///
/// The Rust backend's `cmd::create_handler` registry is the canonical source
/// of truth; this file mirrors it so call sites can use named constants
/// instead of string literals (which are typo-silent until runtime).
///
/// Keep these in sync with `src-tauri/src/cmd/mod.rs::create_handler` and
/// `src-tauri/src/cmd/dialog.rs::DialogKind`.

/// Names of every dialog the host can open. The string values match
/// `DialogKind`'s `serde(rename_all = "snake_case")` representation.
export const Dialog = {
  Navigate: "navigate",
  CreateDirectory: "create_directory",
  CreateFile: "create_file",
  CreateAndEdit: "create_and_edit",
  DirectoryProperties: "directory_properties",
  Properties: "properties",
  Rename: "rename",
  Copy: "copy",
  Move: "move",
  ConnectRemote: "connect_remote",
  MountSftp: "mount_sftp",
  MountS3: "mount_s3",
  MountK8s: "mount_k8s",
  QuickConnect: "quick_connect",
  SelectVfs: "select_vfs",
  HistoryBack: "history_back",
  HistoryForward: "history_forward",
  CommandPalette: "command_palette",
  UserCommands: "user_commands",
  HotPaths: "hot_paths",
  Settings: "settings",
  Debug: "debug",
  ConnectionLog: "connection_log",
  About: "about",
} as const;

export type DialogKind = (typeof Dialog)[keyof typeof Dialog];

/// IPC commands invoked by the frontend that aren't of the `cmd_*` variety.
/// (The `cmd_*` family is dispatched by id from `commands.ts::executeCommandById`
/// via the keybinding/command-palette path; this map is for direct call sites.)
export const Cmd = {
  // Lifecycle
  init: "init",
  ping: "ping",
  closeModal: "close_modal",
  askpassRespond: "askpass_respond",
  zoom: "zoom",
  closeWindow: "close_window",
  destroyWindow: "destroy_window",
  setWindowTitle: "set_window_title",
  // Modal
  dialog: "dialog",
  confirmAction: "confirm_action",
  // Pane
  cancel: "cancel",
  navigate: "navigate",
  enter: "enter",
  focus: "focus",
  setSorting: "set_sorting",
  toggleSelected: "toggle_selected",
  selectRange: "select_range",
  setSelection: "set_selection",
  setSelectionByIndices: "set_selection_by_indices",
  endDragSelection: "end_drag_selection",
  relativeJump: "relative_jump",
  setViewport: "set_viewport",
  setFilter: "set_filter",
  navigateHistory: "navigate_history",
  // File operations
  createDirectory: "create_directory",
  touchFile: "touch_file",
  rename: "rename",
  setMetadata: "set_metadata",
  startOperation: "start_operation",
  startCopyMove: "start_copy_move",
  cancelOperation: "cancel_operation",
  resolveIssue: "resolve_issue",
  dismissOperation: "dismiss_operation",
  backgroundOperation: "background_operation",
  foregroundOperation: "foreground_operation",
  // Viewer / editor
  fileDetails: "file_details",
  readFileRange: "read_file_range",
  readFile: "read_file",
  writeFile: "write_file",
  setViewerMode: "set_viewer_mode",
  pingViewer: "ping_viewer",
  copyViewerRange: "copy_viewer_range",
  findInViewer: "find_in_viewer",
  setEditorLanguage: "set_editor_language",
  setEditorWrap: "set_editor_wrap",
  pingEditor: "ping_editor",
  // Connection / VFS
  reconnect: "reconnect",
  connectRemote: "connect_remote",
  switchVfs: "switch_vfs",
  unmountVfs: "unmount_vfs",
  mountS3: "mount_s3",
  mountSftp: "mount_sftp",
  mountK8s: "mount_k8s",
  // Terminal
  terminalWrite: "terminal_write",
  terminalResize: "terminal_resize",
  terminalFocus: "terminal_focus",
  closeTerminal: "close_terminal",
  activateTerminal: "activate_terminal",
  // Drag & drop
  startDnd: "start_dnd",
  cancelDnd: "cancel_dnd",
  executeDnd: "execute_dnd",
  externalDrop: "external_drop",
  // Preferences
  getPreferences: "get_preferences",
  updatePreference: "update_preference",
  resetPreference: "reset_preference",
  getPreferencesSchema: "get_preferences_schema",
  setCommandKeybinding: "set_command_keybinding",
  resetCommandKeybinding: "reset_command_keybinding",
  openConfigFile: "open_config_file",
  // Hot paths / bookmarks
  getHotPaths: "get_hot_paths",
  addBookmark: "add_bookmark",
  removeBookmark: "remove_bookmark",
  // User commands
  runUserCommand: "run_user_command",
  executeUserCommand: "execute_user_command",
  addUserCommandEntry: "add_user_command_entry",
  removeUserCommandEntry: "remove_user_command_entry",
  updateUserCommandEntry: "update_user_command_entry",
  // Keychain
  keychainGet: "keychain_get",
  keychainSet: "keychain_set",
  keychainDelete: "keychain_delete",
  // Connections
  cmdListConnections: "cmd_list_connections",
  cmdSaveConnection: "cmd_save_connection",
  cmdDeleteConnection: "cmd_delete_connection",
  cmdGetConnectionSecret: "cmd_get_connection_secret",
  connectProfile: "connect_profile",
} as const;

export type CmdName = (typeof Cmd)[keyof typeof Cmd];
