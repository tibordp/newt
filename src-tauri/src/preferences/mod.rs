pub mod schema;

use log::{info, warn};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use schema::{AppPreferences, BookmarkEntry, SettingsFile, UserCommandEntry};
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
    pub user_commands: Vec<UserCommandEntry>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    pub category: String,
    pub shortcut: Option<String>,
    pub shortcut_display: Vec<String>,
    pub needs_pane: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
}

/// A cheaply-cloneable handle for reading the current `AppPreferences`.
///
/// Reads are wait-free (`ArcSwap::load`). Subscribe to the watch channel
/// to be notified when preferences change.
#[derive(Clone)]
pub struct PreferencesHandle {
    settings: Arc<arc_swap::ArcSwap<AppPreferences>>,
    notify: tokio::sync::watch::Receiver<()>,
}

impl PreferencesHandle {
    pub fn load(&self) -> arc_swap::Guard<Arc<AppPreferences>> {
        self.settings.load()
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.notify.clone()
    }
}

pub struct PreferencesManager {
    config_dir: PathBuf,
    resolved: Arc<RwLock<ResolvedPreferences>>,
    handle: PreferencesHandle,
    _notify_tx: tokio::sync::watch::Sender<()>,
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

        let initial = Self::load_and_resolve(&config_dir);
        let settings = Arc::new(arc_swap::ArcSwap::from_pointee(initial.settings.clone()));
        let (notify_tx, notify_rx) = tokio::sync::watch::channel(());
        let resolved = Arc::new(RwLock::new(initial));

        let handle = PreferencesHandle {
            settings: settings.clone(),
            notify: notify_rx,
        };

        // Set up file watcher
        let watcher = Self::setup_watcher(
            app_handle,
            &config_dir,
            resolved.clone(),
            settings,
            notify_tx.clone(),
        );

        Self {
            config_dir,
            resolved,
            handle,
            _notify_tx: notify_tx,
            _watcher: watcher,
        }
    }

    pub fn resolved(&self) -> ResolvedPreferences {
        self.resolved.read().clone()
    }

    pub fn settings(&self) -> AppPreferences {
        self.resolved.read().settings.clone()
    }

    pub fn handle(&self) -> PreferencesHandle {
        self.handle.clone()
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
                if let Some(p) = table.get("path").and_then(|v| v.as_str())
                    && p == path
                {
                    idx_to_remove = Some(i);
                    break;
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

    /// Add a user command entry to settings.toml.
    pub fn add_user_command(&self, entry: &UserCommandEntry) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        let mut table = toml_edit::Table::new();
        table.insert("title", toml_edit::value(&entry.title));
        table.insert("run", toml_edit::value(&entry.run));
        if let Some(ref key) = entry.key {
            table.insert("key", toml_edit::value(key.as_str()));
        }
        if entry.terminal {
            table.insert("terminal", toml_edit::value(true));
        }
        if let Some(ref when) = entry.when {
            table.insert("when", toml_edit::value(when.as_str()));
        }
        table.set_implicit(true);

        if !doc.contains_key("command") {
            doc.insert(
                "command",
                toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
            );
        }

        if let Some(arr) = doc["command"].as_array_of_tables_mut() {
            arr.push(table);
        } else {
            return Err("'command' key exists but is not an array of tables".into());
        }

        std::fs::write(&file_path, doc.to_string())
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;

        Ok(())
    }

    /// Remove a user command entry from settings.toml by index.
    pub fn remove_user_command(&self, index: usize) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        if let Some(arr) = doc["command"].as_array_of_tables_mut() {
            if index < arr.len() {
                arr.remove(index);
            }
            if arr.is_empty() {
                doc.remove("command");
            }
        }

        std::fs::write(&file_path, doc.to_string())
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;

        Ok(())
    }

    /// Update a user command entry in settings.toml by index.
    pub fn update_user_command(
        &self,
        index: usize,
        entry: &UserCommandEntry,
    ) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        let arr = doc["command"]
            .as_array_of_tables_mut()
            .ok_or_else(|| "No [[command]] array in settings.toml".to_string())?;

        if index >= arr.len() {
            return Err(format!("Command index {} out of range", index));
        }

        // Remove old entry and build replacement
        arr.remove(index);

        let mut table = toml_edit::Table::new();
        table.insert("title", toml_edit::value(&entry.title));
        table.insert("run", toml_edit::value(&entry.run));
        if let Some(ref key) = entry.key
            && !key.is_empty()
        {
            table.insert("key", toml_edit::value(key.as_str()));
        }
        if entry.terminal {
            table.insert("terminal", toml_edit::value(true));
        }
        if let Some(ref when) = entry.when
            && !when.is_empty()
            && when != "any"
        {
            table.insert("when", toml_edit::value(when.as_str()));
        }
        table.set_implicit(true);

        // Re-insert at the same position by pushing and then rotating
        arr.push(table);
        // Rotate the last element into position: repeatedly swap adjacent elements
        let len = arr.len();
        // toml_edit ArrayOfTables doesn't have swap/insert-at, so rebuild if needed
        if index < len - 1 {
            // Rebuild the array with the new entry at the correct position
            let mut tables: Vec<toml_edit::Table> = Vec::new();
            // Drain all entries
            while !arr.is_empty() {
                let t = arr.get(0).unwrap().clone();
                arr.remove(0);
                tables.push(t);
            }
            // The new entry is at the end of `tables`, move it to `index`
            let new_entry = tables.pop().unwrap();
            tables.insert(index, new_entry);
            for t in tables {
                arr.push(t);
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
        settings: Arc<arc_swap::ArcSwap<AppPreferences>>,
        notify_tx: tokio::sync::watch::Sender<()>,
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
                        || guard.user_commands != new_resolved.user_commands
                        || guard.commands.len() != new_resolved.commands.len()
                    {
                        *guard = new_resolved.clone();
                    } else {
                        continue;
                    }
                }
                settings.store(Arc::new(new_resolved.settings.clone()));
                let _ = notify_tx.send(());
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

        // Extra default aliases (commands with multiple default keys)
        bindings.push((
            "shift+delete".into(),
            "delete_selected".into(),
            Some("pane_focused".into()),
        ));
        if cfg!(target_os = "macos") {
            bindings.push((
                "meta+backspace".into(),
                "delete_selected".into(),
                Some("pane_focused".into()),
            ));
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

        // Merge user commands from user + profile
        let mut user_commands = user_file.commands.clone();
        if let Some(pf) = &profile_file {
            user_commands.extend(pf.commands.iter().cloned());
        }

        // Add user command keybindings before resolution
        for (i, uc) in user_commands.iter().enumerate() {
            if let Some(ref key) = uc.key {
                bindings.push((
                    key.clone(),
                    format!("user_command_{}", i),
                    Some("pane_focused".to_string()),
                ));
            }
        }

        // Resolve: later entries override earlier ones for same key+when.
        // command = "-" removes the binding.
        let resolved_bindings = resolve_bindings(bindings);

        // Build command info with resolved shortcuts
        let mut commands: Vec<CommandInfo> = command_defs
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
                    short_name: def.short_name.clone(),
                    category: def.category.clone(),
                    shortcut,
                    shortcut_display,
                    needs_pane: def.needs_pane,
                    when: None,
                }
            })
            .collect();

        // Add user commands as CommandInfo entries
        for (i, uc) in user_commands.iter().enumerate() {
            let cmd_id = format!("user_command_{}", i);
            let shortcut = resolved_bindings
                .iter()
                .find(|b| b.command == cmd_id)
                .map(|b| b.key.clone());
            let shortcut_display = shortcut
                .as_ref()
                .map(|k| render_shortcut(k))
                .unwrap_or_default();

            commands.push(CommandInfo {
                id: cmd_id,
                name: uc.title.clone(),
                short_name: None,
                category: "User".to_string(),
                shortcut,
                shortcut_display,
                needs_pane: true,
                when: uc.when.clone(),
            });
        }

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
            user_commands,
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
    pub short_name: Option<String>,
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
            short_name: None,
            category: "File".into(),
            default_key: Some("mod+n".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "copy_pane".into(),
            name: "As Other Pane".into(),
            short_name: None,
            category: "Navigation".into(),
            default_key: Some("mod+.".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "select_all".into(),
            name: "Select All".into(),
            short_name: None,
            category: "Selection".into(),
            default_key: Some("mod+a".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "deselect_all".into(),
            name: "Clear Selection".into(),
            short_name: None,
            category: "Selection".into(),
            default_key: Some("mod+d".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "view".into(),
            name: "View".into(),
            short_name: None,
            category: "File".into(),
            default_key: Some("f3".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "edit".into(),
            name: "Edit".into(),
            short_name: None,
            category: "File".into(),
            default_key: Some("f4".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "rename".into(),
            name: "Rename...".into(),
            short_name: Some("Rename".into()),
            category: "File".into(),
            default_key: Some("f2".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "properties".into(),
            name: "File Properties...".into(),
            short_name: Some("Props".into()),
            category: "File".into(),
            default_key: Some("alt+enter".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "delete_selected".into(),
            name: "Delete Selected".into(),
            short_name: Some("Delete".into()),
            category: "File".into(),
            default_key: Some("f8".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "create_directory".into(),
            name: "Create Directory...".into(),
            short_name: Some("MkDir".into()),
            category: "File".into(),
            default_key: Some("f7".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "create_file".into(),
            name: "Create File...".into(),
            short_name: Some("MkFile".into()),
            category: "File".into(),
            default_key: None,
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "create_and_edit".into(),
            name: "Create and Edit File...".into(),
            short_name: Some("New+Edit".into()),
            category: "File".into(),
            default_key: Some("shift+f4".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "navigate".into(),
            name: "Go To...".into(),
            short_name: Some("Go To".into()),
            category: "Navigation".into(),
            default_key: Some("mod+l".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "open".into(),
            name: "Open in Default App".into(),
            short_name: Some("Open".into()),
            category: "File".into(),
            default_key: None,
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "follow_symlink".into(),
            name: "Follow Symlink".into(),
            short_name: None,
            category: "Navigation".into(),
            default_key: Some("shift+enter".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "navigate_back".into(),
            name: "Navigate Back".into(),
            short_name: Some("Back".into()),
            category: "Navigation".into(),
            default_key: Some("alt+left".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "navigate_forward".into(),
            name: "Navigate Forward".into(),
            short_name: Some("Forward".into()),
            category: "Navigation".into(),
            default_key: Some("alt+right".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "send_to_terminal".into(),
            name: "Open in Terminal".into(),
            short_name: Some("Terminal".into()),
            category: "Terminal".into(),
            default_key: Some("mod+enter".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "copy".into(),
            name: "Copy to Other Pane...".into(),
            short_name: Some("Copy".into()),
            category: "File".into(),
            default_key: Some("f5".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "move".into(),
            name: "Move to Other Pane...".into(),
            short_name: Some("Move".into()),
            category: "File".into(),
            default_key: Some("f6".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "copy_to_clipboard".into(),
            name: "Copy Path to Clipboard".into(),
            short_name: Some("CopyPath".into()),
            category: "Edit".into(),
            default_key: Some("mod+c".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "paste_from_clipboard".into(),
            name: "Paste Path from Clipboard".into(),
            short_name: Some("PastePath".into()),
            category: "Edit".into(),
            default_key: Some("mod+v".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "toggle_hidden".into(),
            name: "Toggle Hidden Files".into(),
            short_name: Some("Hidden".into()),
            category: "View".into(),
            default_key: Some("mod+h".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "close_window".into(),
            name: "Close Window".into(),
            short_name: Some("Close".into()),
            category: "File".into(),
            default_key: Some("mod+w".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "reload_window".into(),
            name: "Reload Window".into(),
            short_name: Some("Reload".into()),
            category: "View".into(),
            default_key: None,
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "connect_remote".into(),
            name: "Connect to Remote Host...".into(),
            short_name: Some("Remote".into()),
            category: "File".into(),
            default_key: Some("mod+shift+r".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "open_elevated".into(),
            name: "Open Elevated".into(),
            short_name: None,
            category: "File".into(),
            default_key: None,
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "select_vfs".into(),
            name: "Select Filesystem".into(),
            short_name: Some("VFS".into()),
            category: "Navigation".into(),
            default_key: Some("mod+shift+l".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "mount_s3".into(),
            name: "Mount S3".into(),
            short_name: None,
            category: "Navigation".into(),
            default_key: None,
            default_when: None,
            needs_pane: true,
        },
        CommandDef {
            id: "mount_sftp".into(),
            name: "Mount SFTP...".into(),
            short_name: Some("SFTP".into()),
            category: "Navigation".into(),
            default_key: None,
            default_when: None,
            needs_pane: true,
        },
        CommandDef {
            id: "unmount_vfs".into(),
            name: "Disconnect VFS".into(),
            short_name: None,
            category: "Navigation".into(),
            default_key: None,
            default_when: None,
            needs_pane: true,
        },
        CommandDef {
            id: "open_folder".into(),
            name: "Open Folder in Default File Manager".into(),
            short_name: Some("Reveal".into()),
            category: "File".into(),
            default_key: Some("shift+f3".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "toggle_terminal_panel".into(),
            name: "Toggle Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_key: Some("ctrl+`".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "focus_panes".into(),
            name: "Focus File Panes".into(),
            short_name: Some("Panes".into()),
            category: "Navigation".into(),
            default_key: Some("alt+up".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "focus_terminal".into(),
            name: "Focus Terminal".into(),
            short_name: Some("Terminal".into()),
            category: "Navigation".into(),
            default_key: Some("alt+down".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "create_terminal".into(),
            name: "New Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_key: Some("ctrl+shift+~".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "next_terminal".into(),
            name: "Next Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_key: Some("ctrl+pagedown".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "prev_terminal".into(),
            name: "Previous Terminal".into(),
            short_name: None,
            category: "Terminal".into(),
            default_key: Some("ctrl+pageup".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "open_settings".into(),
            name: "Open Settings".into(),
            short_name: Some("Settings".into()),
            category: "File".into(),
            default_key: Some("mod+,".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "command_palette".into(),
            name: "Command Palette".into(),
            short_name: Some("CmdPalette".into()),
            category: "View".into(),
            default_key: Some("mod+shift+p".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "hot_paths".into(),
            name: "Hot Paths".into(),
            short_name: None,
            category: "Navigation".into(),
            default_key: Some("mod+p".into()),
            default_when: None,
            needs_pane: false,
        },
        CommandDef {
            id: "user_commands".into(),
            name: "User Commands".into(),
            short_name: None,
            category: "View".into(),
            default_key: Some("f9".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
        CommandDef {
            id: "add_bookmark".into(),
            name: "Add Current Path to Bookmarks".into(),
            short_name: Some("Bookmark".into()),
            category: "Navigation".into(),
            default_key: Some("mod+b".into()),
            default_when: Some("pane_focused".into()),
            needs_pane: true,
        },
    ]
}
