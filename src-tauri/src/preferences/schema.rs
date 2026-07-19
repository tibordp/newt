use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// User-facing preferences that can be set in settings.toml.
///
/// Serde defaults ensure every field has a compiled-in default. The JSON Schema
/// is derived via `schemars` so the frontend settings editor can be generated
/// automatically.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
#[serde(default)]
pub struct AppPreferences {
    #[serde(default)]
    #[schemars(title = "Appearance")]
    pub appearance: AppearancePreferences,
    #[serde(default)]
    #[schemars(title = "Behavior")]
    pub behavior: BehaviorPreferences,
    #[serde(default)]
    #[schemars(title = "Enrichers")]
    pub enrichers: EnricherPreferences,
    #[serde(default)]
    #[schemars(title = "Archives")]
    pub archives: ArchivePreferences,
    #[serde(default)]
    #[schemars(title = "Hot Paths")]
    pub hot_paths: HotPathsPreferences,
    #[serde(default)]
    #[schemars(title = "Environment")]
    pub environment: EnvironmentPreferences,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
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
    /// Show the top bar (path breadcrumbs, VFS selector, free space) on each pane.
    #[schemars(title = "Show Pane Header")]
    pub show_pane_header: bool,
    /// Show the status bar (file/directory counts, selection size) at the bottom of each pane.
    #[schemars(title = "Show Pane Status Bar")]
    pub show_pane_status: bool,
    /// Color theme: "system" follows OS preference, or force "light" / "dark".
    #[schemars(title = "Theme")]
    pub theme: ThemeMode,
    /// Visible columns and their order.
    #[schemars(title = "Columns")]
    pub columns: Vec<String>,
    /// strftime-style format for date columns (e.g. "%Y-%m-%d"). Empty uses the system locale.
    #[schemars(title = "Date Format")]
    pub date_format: String,
    /// strftime-style format for time columns (e.g. "%H:%M"). Empty uses the system locale.
    #[schemars(title = "Time Format")]
    pub time_format: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
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
            show_pane_header: true,
            show_pane_status: true,
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
            date_format: String::new(),
            time_format: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
#[serde(default)]
pub struct BehaviorPreferences {
    /// Ask for confirmation before deleting files.
    #[schemars(title = "Confirm Delete")]
    pub confirm_delete: bool,
    /// Move deleted files to the system Trash instead of deleting them
    /// permanently. Applies to filesystems that have a trash (local files,
    /// remote hosts, agent mounts); Delete Permanently always bypasses it.
    #[schemars(title = "Delete to Trash")]
    pub delete_to_trash: bool,
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
    /// Maximum number of entries kept in each pane's navigation history.
    /// `0` means unlimited (entries accumulate for the lifetime of the
    /// session). When the cap is reached, the oldest entries roll out as
    /// new ones are pushed.
    #[schemars(title = "History Retention", range(min = 0, max = 100000))]
    pub history_retention: u32,
}

impl Default for BehaviorPreferences {
    fn default() -> Self {
        Self {
            confirm_delete: true,
            delete_to_trash: true,
            keep_terminal_open: true,
            keep_finished_operations: false,
            quick_search: true,
            expose_local_fs: false,
            default_sort: DefaultSort::default(),
            history_retention: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
#[serde(default)]
pub struct EnricherPreferences {
    /// Show git status in file listings: per-row colors for
    /// modified/untracked/ignored entries and a branch badge in the pane
    /// header. Runs `git status` in the listed directory's repository
    /// (on the remote host in remote sessions).
    #[schemars(title = "Git Status")]
    pub git_status: bool,
}

impl EnricherPreferences {
    /// Enricher ids gated off by preferences. The pane's enrichment
    /// loop is generic over enrichers; the preference↔id mapping lives
    /// here with the user-facing toggles.
    pub fn disabled_enrichers(&self) -> Vec<String> {
        let mut disabled = Vec::new();
        if !self.git_status {
            disabled.push("git".to_string());
        }
        disabled
    }
}

impl Default for EnricherPreferences {
    fn default() -> Self {
        Self { git_status: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
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

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormatPref {
    Zip,
    Tar,
    TarGz,
    TarXz,
    #[default]
    TarZst,
}

impl From<ArchiveFormatPref> for newt_common::operation::ArchiveFormat {
    fn from(pref: ArchiveFormatPref) -> Self {
        use newt_common::operation::ArchiveFormat;
        match pref {
            ArchiveFormatPref::Zip => ArchiveFormat::Zip,
            ArchiveFormatPref::Tar => ArchiveFormat::Tar,
            ArchiveFormatPref::TarGz => ArchiveFormat::TarGz,
            ArchiveFormatPref::TarXz => ArchiveFormat::TarXz,
            ArchiveFormatPref::TarZst => ArchiveFormat::TarZst,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
#[serde(default)]
pub struct ArchivePreferences {
    /// Format preselected in the Pack to Archive dialog.
    #[schemars(title = "Default Format")]
    pub default_format: ArchiveFormatPref,
    /// Store symlinks as symlinks (off: follow them into the archive).
    #[schemars(title = "Preserve Symlinks")]
    pub preserve_symlinks: bool,
    /// Deflate level for zip archives; 0 stores entries uncompressed.
    #[schemars(title = "Zip Compression Level", range(min = 0, max = 9))]
    pub zip_level: i32,
    /// Compression level for tar.gz archives.
    #[schemars(title = "Gzip Compression Level", range(min = 0, max = 9))]
    pub gzip_level: i32,
    /// Compression level for tar.xz archives.
    #[schemars(title = "XZ Compression Level", range(min = 0, max = 9))]
    pub xz_level: i32,
    /// Compression level for tar.zst archives.
    #[schemars(title = "Zstd Compression Level", range(min = 1, max = 22))]
    pub zstd_level: i32,
}

impl Default for ArchivePreferences {
    fn default() -> Self {
        Self {
            default_format: ArchiveFormatPref::default(),
            preserve_symlinks: true,
            zip_level: 6,
            gzip_level: 6,
            xz_level: 6,
            zstd_level: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, specta::Type)]
#[serde(default)]
pub struct EnvironmentPreferences {
    /// Directories to prepend to `PATH` at startup. Useful on macOS / GNOME
    /// where GUI apps don't inherit the user's shell `PATH`, so subprocesses
    /// like `docker` / `kubectl` / `podman` can't be found at their usual
    /// install locations. Leading `~` expands to the user's home directory.
    /// Non-existent entries are silently skipped.
    #[schemars(title = "Extra PATH Entries")]
    pub extra_path: Vec<String>,
}

impl Default for EnvironmentPreferences {
    fn default() -> Self {
        // Per-platform defaults aimed at the well-known install locations
        // for common dev tools. Users can edit / extend in settings.toml.
        #[cfg(target_os = "macos")]
        let extra_path = vec!["/opt/homebrew/bin".into(), "/usr/local/bin".into()];
        #[cfg(target_os = "linux")]
        let extra_path = vec![
            "~/.local/bin".into(),
            "/snap/bin".into(),
            "/var/lib/flatpak/exports/bin".into(),
        ];
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let extra_path: Vec<String> = Vec::new();
        Self { extra_path }
    }
}

fn default_toml_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

/// Raw TOML file structure — settings plus optional profile name and keybinding
/// overrides.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct SettingsFile {
    /// Active profile name (loads `profiles/<name>.toml` on top).
    pub profile: Option<String>,

    #[serde(default = "default_toml_table")]
    #[specta(type = serde_json::Value)]
    pub appearance: toml::Value,
    #[serde(default = "default_toml_table")]
    #[specta(type = serde_json::Value)]
    pub behavior: toml::Value,
    #[serde(default = "default_toml_table")]
    #[specta(type = serde_json::Value)]
    pub enrichers: toml::Value,
    #[serde(default = "default_toml_table")]
    #[specta(type = serde_json::Value)]
    pub archives: toml::Value,
    #[serde(default = "default_toml_table")]
    #[specta(type = serde_json::Value)]
    pub hot_paths: toml::Value,
    #[serde(default = "default_toml_table")]
    #[specta(type = serde_json::Value)]
    pub environment: toml::Value,

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

impl SettingsFile {
    /// Every settings section as `(name, raw TOML table)`. The single
    /// source of truth for section-wise processing (merging onto
    /// defaults, modified-key detection) — a new `AppPreferences` group
    /// must be added here (and as a field above) or its TOML section is
    /// silently ignored on load.
    pub fn sections(&self) -> [(&'static str, &toml::Value); 6] {
        [
            ("appearance", &self.appearance),
            ("behavior", &self.behavior),
            ("enrichers", &self.enrichers),
            ("archives", &self.archives),
            ("hot_paths", &self.hot_paths),
            ("environment", &self.environment),
        ]
    }
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            profile: None,
            appearance: default_toml_table(),
            behavior: default_toml_table(),
            enrichers: default_toml_table(),
            archives: default_toml_table(),
            hot_paths: default_toml_table(),
            environment: default_toml_table(),
            bindings: Vec::new(),
            bookmarks: Vec::new(),
            commands: Vec::new(),
        }
    }
}

/// A single `[[bind]]` entry in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct KeybindingEntry {
    pub key: String,
    pub command: String,
    #[serde(default)]
    pub when: Option<String>,
}

/// A single `[[command]]` entry in the TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct UserCommandEntry {
    pub title: String,
    pub run: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub terminal: bool,
    /// Run-context filter — which file selection state allows this command
    /// to appear in the palette / be invoked. One of "file", "directory",
    /// "selection", or absent (= any). NOT to be confused with `[[bind]].when`,
    /// which is the keybinding *dispatch* context.
    #[serde(default)]
    pub applies_to: Option<String>,
}

/// A single `[[bookmark]]` entry in the TOML file.
#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct BookmarkEntry {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
}
