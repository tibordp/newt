use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// User-facing preferences that can be set in settings.toml.
///
/// Serde defaults ensure every field has a compiled-in default. The JSON Schema
/// is derived via `schemars` so the frontend settings editor can be generated
/// automatically.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct AppPreferences {
    #[serde(default)]
    #[schemars(title = "Appearance")]
    pub appearance: AppearancePreferences,
    #[serde(default)]
    #[schemars(title = "Behavior")]
    pub behavior: BehaviorPreferences,
    #[serde(default)]
    #[schemars(title = "Hot Paths")]
    pub hot_paths: HotPathsPreferences,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct AppearancePreferences {
    /// Show hidden files by default when opening a new window.
    #[schemars(title = "Show Hidden Files")]
    pub show_hidden: bool,
    /// Always show folders before files regardless of sort order.
    #[schemars(title = "Folders First")]
    pub folders_first: bool,
    /// Show the F-key command bar at the bottom of the window.
    #[schemars(title = "Show Command Bar")]
    pub show_command_bar: bool,
    /// Color theme: "system" follows OS preference, or force "light" / "dark".
    #[schemars(title = "Theme")]
    pub theme: ThemeMode,
    /// Visible columns and their order.
    #[schemars(title = "Columns")]
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub fn to_tauri_theme(&self) -> Option<tauri::Theme> {
        match self {
            ThemeMode::System => None,
            ThemeMode::Light => Some(tauri::Theme::Light),
            ThemeMode::Dark => Some(tauri::Theme::Dark),
        }
    }
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            show_hidden: false,
            folders_first: true,
            show_command_bar: true,
            theme: ThemeMode::default(),
            columns: vec![
                "name".into(),
                "size".into(),
                "modified_date".into(),
                "modified_time".into(),
                "user".into(),
                "group".into(),
                "mode".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct DefaultSort {
    pub key: DefaultSortKey,
    pub ascending: bool,
}

impl Default for DefaultSort {
    fn default() -> Self {
        Self {
            key: DefaultSortKey::default(),
            ascending: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSortKey {
    #[default]
    Name,
    Extension,
    Size,
    Modified,
    Accessed,
    Created,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct BehaviorPreferences {
    /// Ask for confirmation before deleting files.
    #[schemars(title = "Confirm Delete")]
    pub confirm_delete: bool,
    /// Keep terminal tab open after the shell process exits.
    #[schemars(title = "Keep Terminal Open After Exit")]
    pub keep_terminal_open: bool,
    /// Keep completed/cancelled operations visible in the operations panel.
    #[schemars(title = "Keep Finished Operations")]
    pub keep_finished_operations: bool,
    /// Use incremental quick-search when typing in a file pane. When disabled,
    /// typing opens the regex filter directly.
    #[schemars(title = "Quick Search")]
    pub quick_search: bool,
    /// In remote (SSH) sessions, expose the local filesystem to the remote
    /// host so it can be browsed alongside remote files. Disable this if the
    /// remote host is untrusted.
    #[schemars(title = "Expose Local Filesystem in Remote Sessions")]
    pub expose_local_fs: bool,
    /// Default sort order for new panes.
    #[schemars(title = "Default Sort")]
    pub default_sort: DefaultSort,
}

impl Default for BehaviorPreferences {
    fn default() -> Self {
        Self {
            confirm_delete: true,
            keep_terminal_open: true,
            keep_finished_operations: false,
            quick_search: true,
            expose_local_fs: false,
            default_sort: DefaultSort::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct HotPathsPreferences {
    /// Show standard folders (Home, Downloads, Documents, etc.)
    #[schemars(title = "Standard Folders")]
    pub standard_folders: bool,
    /// Show system bookmarks (GTK bookmarks on Linux)
    #[schemars(title = "System Bookmarks")]
    pub system_bookmarks: bool,
    /// Show mounted volumes and removable media
    #[schemars(title = "Mounted Volumes")]
    pub mounts: bool,
    /// Show recently visited folders
    #[schemars(title = "Recent Folders")]
    pub recent_folders: bool,
}

impl Default for HotPathsPreferences {
    fn default() -> Self {
        Self {
            standard_folders: true,
            system_bookmarks: true,
            mounts: true,
            recent_folders: true,
        }
    }
}

fn default_toml_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

/// Raw TOML file structure — settings plus optional profile name and keybinding
/// overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsFile {
    /// Active profile name (loads `profiles/<name>.toml` on top).
    pub profile: Option<String>,

    #[serde(default = "default_toml_table")]
    pub appearance: toml::Value,
    #[serde(default = "default_toml_table")]
    pub behavior: toml::Value,
    #[serde(default = "default_toml_table")]
    pub hot_paths: toml::Value,

    /// Keybinding override entries.
    #[serde(default, rename = "bind")]
    pub bindings: Vec<KeybindingEntry>,

    /// User-defined bookmark entries.
    #[serde(default, rename = "bookmark")]
    pub bookmarks: Vec<BookmarkEntry>,

    /// User-defined command entries.
    #[serde(default, rename = "command")]
    pub commands: Vec<UserCommandEntry>,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            profile: None,
            appearance: default_toml_table(),
            behavior: default_toml_table(),
            hot_paths: default_toml_table(),
            bindings: Vec::new(),
            bookmarks: Vec::new(),
            commands: Vec::new(),
        }
    }
}

/// A single `[[bind]]` entry in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeybindingEntry {
    pub key: String,
    pub command: String,
    #[serde(default)]
    pub when: Option<String>,
}

/// A single `[[command]]` entry in the TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserCommandEntry {
    pub title: String,
    pub run: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default)]
    pub when: Option<String>,
}

/// A single `[[bookmark]]` entry in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookmarkEntry {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
}
