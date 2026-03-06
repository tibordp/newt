pub mod schema;

use log::{info, warn};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use schema::{AppPreferences, BookmarkEntry, SettingsFile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// The fully-resolved preferences state pushed to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvedPreferences {
    pub settings: AppPreferences,
    pub schema: serde_json::Value,
    pub bindings: Vec<ResolvedBinding>,
    pub commands: Vec<CommandInfo>,
    pub bookmarks: Vec<BookmarkEntry>,
}

/// A resolved keybinding after `mod+` expansion and cascading.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvedBinding {
    pub key: String,
    pub command: String,
    pub when: Option<String>,
}

/// Command metadata for the command palette.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandInfo {
    pub id: String,
    pub name: String,
    pub category: String,
    pub shortcut: Option<String>,
    pub shortcut_display: Vec<String>,
    pub needs_pane: bool,
}

pub struct PreferencesManager {
    config_dir: PathBuf,
    resolved: Arc<RwLock<ResolvedPreferences>>,
    _watcher: Option<RecommendedWatcher>,
}

impl PreferencesManager {
    pub fn new(app_handle: &tauri::AppHandle) -> Self {
        let config_dir = app_handle
            .path()
            .app_config_dir()
            .unwrap_or_else(|_| PathBuf::from("."));

        // Ensure config directory exists
        if let Err(e) = std::fs::create_dir_all(&config_dir) {
            warn!("Failed to create config dir {:?}: {}", config_dir, e);
        }

        let resolved = Arc::new(RwLock::new(Self::load_and_resolve(&config_dir)));

        // Set up file watcher
        let watcher = Self::setup_watcher(app_handle, &config_dir, resolved.clone());

        Self {
            config_dir,
            resolved,
            _watcher: watcher,
        }
    }

    pub fn resolved(&self) -> ResolvedPreferences {
        self.resolved.read().clone()
    }

    pub fn settings(&self) -> AppPreferences {
        self.resolved.read().settings.clone()
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn settings_file_path(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    /// Update a single preference value via dotted path (e.g. "appearance.show_hidden").
    /// Preserves existing comments by using toml_edit.
    pub fn update_preference(&self, key: &str, value: serde_json::Value) -> Result<(), String> {
        let path = self.settings_file_path();
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        let parts: Vec<&str> = key.split('.').collect();
        if parts.is_empty() {
            return Err("empty key".into());
        }

        // Navigate/create intermediate tables
        let (table_parts, leaf) = parts.split_at(parts.len() - 1);
        let mut table = doc.as_table_mut();
        for part in table_parts {
            if !table.contains_key(part) {
                table.insert(part, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = table[part]
                .as_table_mut()
                .ok_or_else(|| format!("{} is not a table", part))?;
        }

        let toml_value = json_to_toml_edit_value(&value)?;
        table.insert(leaf[0], toml_edit::value(toml_value));

        std::fs::write(&path, doc.to_string())
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;

        Ok(())
    }

    /// Add a bookmark entry to settings.toml. Preserves existing content.
    pub fn add_bookmark(&self, path: &str, name: Option<&str>) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        // Build the [[bookmark]] array-of-tables entry
        let mut entry = toml_edit::Table::new();
        entry.insert("path", toml_edit::value(path));
        if let Some(n) = name {
            entry.insert("name", toml_edit::value(n));
        }
        entry.set_implicit(true);

        // Get or create the [[bookmark]] array
        if !doc.contains_key("bookmark") {
            doc.insert(
                "bookmark",
                toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
            );
        }

        if let Some(arr) = doc["bookmark"].as_array_of_tables_mut() {
            arr.push(entry);
        } else {
            return Err("'bookmark' key exists but is not an array of tables".into());
        }

        std::fs::write(&file_path, doc.to_string())
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;

        Ok(())
    }

    /// Remove a bookmark entry from settings.toml by path.
    pub fn remove_bookmark(&self, path: &str) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        if let Some(arr) = doc["bookmark"].as_array_of_tables_mut() {
            // Find and remove the entry with matching path
            let mut idx_to_remove = None;
            for (i, table) in arr.iter().enumerate() {
                if let Some(p) = table.get("path").and_then(|v| v.as_str()) {
                    if p == path {
                        idx_to_remove = Some(i);
                        break;
                    }
                }
            }
            if let Some(idx) = idx_to_remove {
                arr.remove(idx);
            }
            // If the array is now empty, remove the key entirely
            if arr.is_empty() {
                doc.remove("bookmark");
            }
        }

        std::fs::write(&file_path, doc.to_string())
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;

        Ok(())
    }

    fn setup_watcher(
        app_handle: &tauri::AppHandle,
        config_dir: &Path,
        resolved: Arc<RwLock<ResolvedPreferences>>,
    ) -> Option<RecommendedWatcher> {
        let config_dir_owned = config_dir.to_owned();
        let app_handle = app_handle.clone();

        // Debounce: we'll use notify's built-in debouncing is not available in
        // notify 8, so we do manual debounce with a channel.
        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher = match RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
                            let _ = tx.send(());
                        }
                        _ => {}
                    }
                }
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                warn!("Failed to create file watcher: {}", e);
                return None;
            }
        };

        if let Err(e) = watcher.watch(config_dir, RecursiveMode::NonRecursive) {
            warn!("Failed to watch config dir: {}", e);
            return None;
        }

        // Also watch the profiles subdirectory if it exists
        let profiles_dir = config_dir.join("profiles");
        if profiles_dir.exists() {
            let _ = watcher.watch(&profiles_dir, RecursiveMode::NonRecursive);
        }

        // Spawn debounce thread
        std::thread::spawn(move || {
            while let Ok(()) = rx.recv() {
                // Drain any queued events within 200ms
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
                while let Ok(()) =
                    rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                {
                }

                info!("Config file changed, reloading preferences");
                let new_resolved = Self::load_and_resolve(&config_dir_owned);
                {
                    let mut guard = resolved.write();
                    if guard.settings != new_resolved.settings
                        || guard.bindings.len() != new_resolved.bindings.len()
                        || guard.bookmarks.len() != new_resolved.bookmarks.len()
                    {
                        *guard = new_resolved.clone();
                    } else {
                        continue;
                    }
                }
                let _ = app_handle.emit("update:preferences", &new_resolved);
            }
        });

        Some(watcher)
    }

    fn load_and_resolve(config_dir: &Path) -> ResolvedPreferences {
        let settings_path = config_dir.join("settings.toml");
        let user_file = Self::load_settings_file(&settings_path);

        // Load profile if specified
        let profile_file = user_file.profile.as_ref().and_then(|name| {
            let profile_path = config_dir.join("profiles").join(format!("{}.toml", name));
            if profile_path.exists() {
                Some(Self::load_settings_file(&profile_path))
            } else {
                warn!("Profile '{}' not found at {:?}", name, profile_path);
                None
            }
        });

        Self::resolve(user_file, profile_file)
    }

    fn load_settings_file(path: &Path) -> SettingsFile {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str(&content) {
                Ok(file) => file,
                Err(e) => {
                    warn!("Failed to parse {:?}: {}. Using defaults.", path, e);
                    SettingsFile::default()
                }
            },
            Err(_) => SettingsFile::default(),
        }
    }

    fn resolve(user_file: SettingsFile, profile_file: Option<SettingsFile>) -> ResolvedPreferences {
        // Cascade scalar settings: defaults → user → profile
        let defaults = AppPreferences::default();
        let user_prefs = Self::merge_preferences(&defaults, &user_file);
        let settings = match &profile_file {
            Some(pf) => Self::merge_preferences(&user_prefs, pf),
            None => user_prefs,
        };

        // Build command table and default bindings
        let command_defs = default_commands();
        let mut bindings: Vec<(String, String, Option<String>)> = Vec::new();

        // 1. Default bindings from command defs
        for def in &command_defs {
            if let Some(ref key) = def.default_key {
                bindings.push((key.clone(), def.id.clone(), def.default_when.clone()));
            }
        }

        // 2. User overrides
        for entry in &user_file.bindings {
            bindings.push((entry.key.clone(), entry.command.clone(), entry.when.clone()));
        }

        // 3. Profile overrides
        if let Some(pf) = &profile_file {
            for entry in &pf.bindings {
                bindings.push((entry.key.clone(), entry.command.clone(), entry.when.clone()));
            }
        }

        // Resolve: later entries override earlier ones for same key+when.
        // command = "-" removes the binding.
        let resolved_bindings = resolve_bindings(bindings);

        // Build command info with resolved shortcuts
        let commands: Vec<CommandInfo> = command_defs
            .iter()
            .map(|def| {
                let shortcut = resolved_bindings
                    .iter()
                    .find(|b| b.command == def.id)
                    .map(|b| b.key.clone());
                let shortcut_display = shortcut
                    .as_ref()
                    .map(|k| render_shortcut(k))
                    .unwrap_or_default();
                CommandInfo {
                    id: def.id.clone(),
                    name: def.name.clone(),
                    category: def.category.clone(),
                    shortcut,
                    shortcut_display,
                    needs_pane: def.needs_pane,
                }
            })
            .collect();

        // Cascade bookmarks: user + profile
        let mut bookmarks = user_file.bookmarks.clone();
        if let Some(pf) = &profile_file {
            bookmarks.extend(pf.bookmarks.iter().cloned());
        }

        let schema = schemars::schema_for!(AppPreferences);
        let schema_json = serde_json::to_value(schema).unwrap_or_default();

        ResolvedPreferences {
            settings,
            schema: schema_json,
            bindings: resolved_bindings,
            commands,
            bookmarks,
        }
    }

    /// Merge a `SettingsFile`'s raw TOML values on top of an existing `AppPreferences`.
    ///
    /// Serializes `base` to a TOML table, deep-merges only the keys present in
    /// `file` (so unset keys keep the base value), then deserializes back.
    fn merge_preferences(base: &AppPreferences, file: &SettingsFile) -> AppPreferences {
        let mut base_table =
            toml::Value::try_from(base).unwrap_or(toml::Value::Table(Default::default()));

        // Merge each category's raw TOML table on top of the serialized base
        if let toml::Value::Table(ref mut root) = base_table {
            if let toml::Value::Table(ref t) = file.appearance {
                deep_merge_table(
                    root.entry("appearance")
                        .or_insert(toml::Value::Table(Default::default())),
                    &toml::Value::Table(t.clone()),
                );
            }
            if let toml::Value::Table(ref t) = file.behavior {
                deep_merge_table(
                    root.entry("behavior")
                        .or_insert(toml::Value::Table(Default::default())),
                    &toml::Value::Table(t.clone()),
                );
            }
            if let toml::Value::Table(ref t) = file.hot_paths {
                deep_merge_table(
                    root.entry("hot_paths")
                        .or_insert(toml::Value::Table(Default::default())),
                    &toml::Value::Table(t.clone()),
                );
            }
        }

        base_table.try_into().unwrap_or_else(|e| {
            warn!(
                "Failed to deserialize merged preferences: {}. Using base.",
                e
            );
            base.clone()
        })
    }
}

/// Recursively merge `overlay` into `base`. Tables are merged key-by-key;
/// all other values (scalars, arrays) are replaced wholesale.
fn deep_merge_table(base: &mut toml::Value, overlay: &toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_t), toml::Value::Table(overlay_t)) => {
            for (k, v) in overlay_t {
                let entry = base_t
                    .entry(k.clone())
                    .or_insert(toml::Value::Table(Default::default()));
                deep_merge_table(entry, v);
            }
        }
        (base, overlay) => {
            *base = overlay.clone();
        }
    }
}

/// Expand `mod+` to the platform-native modifier key (meta on macOS, ctrl elsewhere).
fn expand_mod(key: &str) -> String {
    let replacement = if cfg!(target_os = "macos") {
        "meta"
    } else {
        "ctrl"
    };

    // Normalize to lowercase
    let key = key.to_lowercase();

    // Replace mod+ prefix and any mod+ in the middle
    let parts: Vec<&str> = key.split('+').collect();
    parts
        .iter()
        .map(|part| if *part == "mod" { replacement } else { part })
        .collect::<Vec<_>>()
        .join("+")
}

/// Resolve bindings with cascading. Later entries override earlier ones for the
/// same (key, when) pair. `command = "-"` removes a binding.
fn resolve_bindings(entries: Vec<(String, String, Option<String>)>) -> Vec<ResolvedBinding> {
    use std::collections::HashMap;

    let mut map: HashMap<(String, Option<String>), String> = HashMap::new();
    // Track insertion order
    let mut order: Vec<(String, Option<String>)> = Vec::new();

    for (key, command, when) in entries {
        let expanded_key = expand_mod(&key);
        let map_key = (expanded_key, when);
        if !map.contains_key(&map_key) {
            order.push(map_key.clone());
        }
        map.insert(map_key, command);
    }

    order
        .into_iter()
        .filter_map(|k| {
            let command = map.get(&k)?;
            if command == "-" {
                return None;
            }
            Some(ResolvedBinding {
                key: k.0,
                command: command.clone(),
                when: k.1,
            })
        })
        .collect()
}

/// Render a key string like "ctrl+shift+f5" into display parts ["Ctrl", "Shift", "F5"].
fn render_shortcut(key: &str) -> Vec<String> {
    let is_mac = cfg!(target_os = "macos");
    key.split('+')
        .map(|part| match part.to_lowercase().as_str() {
            "ctrl" => "Ctrl".to_string(),
            "meta" => {
                if is_mac {
                    "\u{2318}".to_string()
                } else {
                    "Super".to_string()
                }
            }
            "shift" => "Shift".to_string(),
            "alt" => {
                if is_mac {
                    "\u{2325}".to_string()
                } else {
                    "Alt".to_string()
                }
            }
            other => {
                // Capitalize first letter for display
                let mut c = other.chars();
                match c.next() {
                    Some(first) => {
                        let rest: String = c.collect();
                        format!("{}{}", first.to_uppercase(), rest)
                    }
                    None => String::new(),
                }
            }
        })
        .collect()
}

fn json_to_toml_edit_value(value: &serde_json::Value) -> Result<toml_edit::Value, String> {
    match value {
        serde_json::Value::Bool(b) => Ok(toml_edit::Value::from(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml_edit::Value::from(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml_edit::Value::from(f))
            } else {
                Err("unsupported number type".into())
            }
        }
        serde_json::Value::String(s) => Ok(toml_edit::Value::from(s.as_str())),
        _ => Err(format!("unsupported value type for TOML: {:?}", value)),
    }
}

/// Static command definitions — the source of truth for all commands.
#[derive(Debug, Clone)]
pub struct CommandDef {
    pub id: String,
    pub name: String,
    pub category: String,
    pub default_key: Option<String>,
    pub default_when: Option<String>,
    pub needs_pane: bool,
}

pub fn default_commands() -> Vec<CommandDef> {
    vec![
        CommandDef {
            id: "new_window".into(),
            name: "New Window".into(),
            category: "File".into(),
            default_key: Some("mod+n".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "copy_pane".into(),
            name: "As Other Pane".into(),
            category: "Navigation".into(),
            default_key: Some("mod+.".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "select_all".into(),
            name: "Select All".into(),
            category: "Selection".into(),
            default_key: Some("mod+a".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "deselect_all".into(),
            name: "Clear Selection".into(),
            category: "Selection".into(),
            default_key: Some("mod+d".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "view".into(),
            name: "View".into(),
            category: "File".into(),
            default_key: Some("f3".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "edit".into(),
            name: "Edit".into(),
            category: "File".into(),
            default_key: Some("f4".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "rename".into(),
            name: "Rename...".into(),
            category: "File".into(),
            default_key: Some("f2".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "properties".into(),
            name: "File Properties...".into(),
            category: "File".into(),
            default_key: Some("alt+enter".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "delete_selected".into(),
            name: "Delete Selected".into(),
            category: "File".into(),
            default_key: Some("shift+delete".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "create_directory".into(),
            name: "Create Directory...".into(),
            category: "File".into(),
            default_key: Some("f7".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "create_file".into(),
            name: "Create File...".into(),
            category: "File".into(),
            default_key: None,
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "create_and_edit".into(),
            name: "Create and Edit File...".into(),
            category: "File".into(),
            default_key: Some("shift+f4".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "navigate".into(),
            name: "Go To...".into(),
            category: "Navigation".into(),
            default_key: Some("mod+l".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "open".into(),
            name: "Open in Default App".into(),
            category: "File".into(),
            default_key: None,
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "send_to_terminal".into(),
            name: "Open in Terminal".into(),
            category: "Terminal".into(),
            default_key: Some("mod+enter".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "copy".into(),
            name: "Copy to Other Pane...".into(),
            category: "File".into(),
            default_key: Some("f5".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "move".into(),
            name: "Move to Other Pane...".into(),
            category: "File".into(),
            default_key: Some("f6".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "copy_to_clipboard".into(),
            name: "Copy Path to Clipboard".into(),
            category: "Edit".into(),
            default_key: Some("mod+c".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "paste_from_clipboard".into(),
            name: "Paste Path from Clipboard".into(),
            category: "Edit".into(),
            default_key: Some("mod+v".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "toggle_hidden".into(),
            name: "Toggle Hidden Files".into(),
            category: "View".into(),
            default_key: Some("mod+h".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "close_window".into(),
            name: "Close Window".into(),
            category: "File".into(),
            default_key: Some("mod+w".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "reload_window".into(),
            name: "Reload Window".into(),
            category: "View".into(),
            default_key: None,
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "connect_remote".into(),
            name: "Connect to Remote Host...".into(),
            category: "File".into(),
            default_key: Some("mod+shift+r".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "open_elevated".into(),
            name: "Open Elevated".into(),
            category: "File".into(),
            default_key: None,
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "select_vfs".into(),
            name: "Select Filesystem".into(),
            category: "Navigation".into(),
            default_key: Some("mod+shift+l".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "mount_s3".into(),
            name: "Mount S3".into(),
            category: "Navigation".into(),
            default_key: None,
            default_when: None,
            needs_pane: true,
        },
        CommandDef {
            id: "open_folder".into(),
            name: "Open Folder in Default File Manager".into(),
            category: "File".into(),
            default_key: Some("shift+f3".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "toggle_terminal_panel".into(),
            name: "Toggle Terminal".into(),
            category: "Terminal".into(),
            default_key: Some("ctrl+`".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "create_terminal".into(),
            name: "New Terminal".into(),
            category: "Terminal".into(),
            default_key: Some("ctrl+shift+~".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "next_terminal".into(),
            name: "Next Terminal".into(),
            category: "Terminal".into(),
            default_key: Some("ctrl+pagedown".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "prev_terminal".into(),
            name: "Previous Terminal".into(),
            category: "Terminal".into(),
            default_key: Some("ctrl+pageup".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "open_settings".into(),
            name: "Open Settings".into(),
            category: "File".into(),
            default_key: Some("mod+,".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "command_palette".into(),
            name: "Command Palette".into(),
            category: "View".into(),
            default_key: Some("mod+shift+p".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "hot_paths".into(),
            name: "Hot Paths".into(),
            category: "Navigation".into(),
            default_key: Some("mod+p".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "add_bookmark".into(),
            name: "Add Current Path to Bookmarks".into(),
            category: "Navigation".into(),
            default_key: Some("mod+b".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
    ]
}
