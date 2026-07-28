pub mod commands;
pub mod schema;

#[cfg(test)]
mod tests;

use log::{info, warn};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use schema::{AppPreferences, BookmarkEntry, SettingsFile, UserCommandEntry};
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// The fully-resolved preferences state pushed to the frontend.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct ResolvedPreferences {
    pub settings: AppPreferences,
    pub schema: serde_json::Value,
    /// Dotted keys that are explicitly set in the user's settings file
    /// (i.e. not inherited from defaults or profile).
    pub modified_keys: Vec<String>,
    pub bindings: Vec<ResolvedBinding>,
    pub commands: Vec<CommandInfo>,
    pub bookmarks: Vec<BookmarkEntry>,
    pub user_commands: Vec<UserCommandEntry>,
    /// BCP-47 tag the frontend formats numbers and dates with: the
    /// `appearance.locale` preference when set, else the system's regional
    /// format. `None` leaves the choice to the webview — which on Windows
    /// is initialised from the *UI* language rather than the regional
    /// format, so it is not a good default to rely on.
    ///
    /// A preference value is passed through verbatim, *not* validated: the
    /// settings dialog writes on every keystroke, so this legitimately
    /// carries half-typed tags like `sl-`. The frontend checks
    /// well-formedness against `Intl` before use (`usableLocale`), which is
    /// the engine that would otherwise throw on them.
    pub locale: Option<String>,
}

/// Outcome of [`PreferencesManager::add_bookmark`].
pub struct BookmarkAdded {
    /// The settings.toml body from before the change — feed it back to
    /// [`PreferencesManager::restore_bookmarks`] to undo.
    pub snapshot: String,
    /// The path was already bookmarked and got bumped to the top rather
    /// than added.
    pub was_bookmarked: bool,
}

/// A resolved keybinding after `mod+` expansion and cascading.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct ResolvedBinding {
    pub key: String,
    pub command: String,
    pub when: Option<String>,
}

/// Command metadata for the command palette.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct CommandInfo {
    pub id: String,
    pub name: String,
    pub short_name: Option<String>,
    pub category: String,
    /// Primary resolved binding (first in resolution order) — what menus,
    /// tooltips and the command bar display.
    pub shortcut: Option<String>,
    pub shortcut_display: Vec<String>,
    /// Every resolved binding for this command, primary first.
    pub shortcuts: Vec<String>,
    pub needs_pane: bool,
    /// Keybinding *dispatch context* (`pane_focused` / `terminal_focused` /
    /// unset = global). For user commands this is hard-coded to
    /// `pane_focused`. For built-ins it reflects the resolved binding's
    /// `when`. Distinct from `applies_to`, which is the user-command
    /// run-filter.
    pub when: Option<String>,
    /// User-command run filter (`file` / `directory` / `selection` / unset =
    /// any). Only set for user commands; always `None` for built-ins.
    pub applies_to: Option<String>,
    /// The compiled-in default keys for this command (`mod` expanded).
    /// Useful for the keybindings editor to display "Default: …" hints and
    /// offer Reset.
    pub default_keys: Vec<String>,
    /// The compiled-in default dispatch context for this command, if any.
    pub default_when: Option<String>,
    /// True when the resolved keybinding for this command differs from its
    /// compiled-in default (either remapped by the user, disabled, or its
    /// default slot has been usurped by another command). Only meaningful
    /// for built-ins; always `false` for user commands.
    pub user_overridden: bool,
    /// Which window's dispatcher owns this command — the palette and each
    /// window's key handling filter by it.
    pub scope: crate::preferences::commands::CommandScope,
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
    config_dir: std::path::PathBuf,
    resolved: Arc<RwLock<ResolvedPreferences>>,
    handle: PreferencesHandle,
    notify_tx: tokio::sync::watch::Sender<()>,
    app_handle: tauri::AppHandle,
    _watcher: Option<RecommendedWatcher>,
}

impl PreferencesManager {
    pub fn new(
        app_handle: &tauri::AppHandle,
        config_dir_override: Option<std::path::PathBuf>,
    ) -> Self {
        let config_dir = config_dir_override.unwrap_or_else(|| {
            app_handle
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
        });

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
            notify_tx,
            app_handle: app_handle.clone(),
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

    pub fn config_dir(&self) -> &std::path::Path {
        &self.config_dir
    }

    pub fn settings_file_path(&self) -> std::path::PathBuf {
        self.config_dir.join("settings.toml")
    }

    /// Reload preferences from disk, update stored state, notify subscribers,
    /// and emit to the frontend.
    pub fn reload(&self) {
        let new_resolved = Self::load_and_resolve(&self.config_dir);
        {
            let mut guard = self.resolved.write();
            *guard = new_resolved.clone();
        }
        self.handle
            .settings
            .store(Arc::new(new_resolved.settings.clone()));
        let _ = self.notify_tx.send(());
        let _ = self.app_handle.emit("update:preferences", &new_resolved);
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

    /// Remove a preference key from settings.toml so it falls back to the default.
    pub fn reset_preference(&self, key: &str) -> Result<(), String> {
        let path = self.settings_file_path();
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        let parts: Vec<&str> = key.split('.').collect();
        if parts.is_empty() {
            return Err("empty key".into());
        }

        let (table_parts, leaf) = parts.split_at(parts.len() - 1);
        let mut table = doc.as_table_mut();
        for part in table_parts {
            match table.get_mut(part).and_then(|v| v.as_table_mut()) {
                Some(t) => table = t,
                None => return Ok(()), // key doesn't exist, nothing to reset
            }
        }

        table.remove(leaf[0]);

        std::fs::write(&path, doc.to_string())
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;

        Ok(())
    }

    /// Bookmark the path, moving it to the top if it was already bookmarked.
    /// Returns the pre-change file body (the undo snapshot for
    /// [`Self::restore_bookmarks`]) plus whether the path was already there.
    pub fn add_bookmark(&self, path: &str, name: Option<&str>) -> Result<BookmarkAdded, String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let (new_content, was_bookmarked) = apply_add_bookmark(&content, path, name)?;

        std::fs::write(&file_path, new_content)
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;
        self.reload();

        Ok(BookmarkAdded {
            snapshot: content,
            was_bookmarked,
        })
    }

    /// Remove every bookmark entry with this path from settings.toml.
    pub fn remove_bookmark(&self, path: &str) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();

        std::fs::write(&file_path, apply_remove_bookmark(&content, path)?)
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;
        self.reload();

        Ok(())
    }

    /// Undo primitive: put the `[[bookmark]]` array back the way `snapshot`
    /// had it, leaving every other key in the current file alone (so an
    /// unrelated settings edit made in the meantime survives).
    pub fn restore_bookmarks(&self, snapshot: &str) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();

        std::fs::write(&file_path, apply_restore_bookmarks(&content, snapshot)?)
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;
        self.reload();

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
        if entry.silent {
            table.insert("silent", toml_edit::value(true));
        }
        if entry.keep_terminal_open {
            table.insert("keep_terminal_open", toml_edit::value(true));
        }
        if let Some(ref applies) = entry.applies_to {
            table.insert("applies_to", toml_edit::value(applies.as_str()));
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
        if entry.silent {
            table.insert("silent", toml_edit::value(true));
        }
        if entry.keep_terminal_open {
            table.insert("keep_terminal_open", toml_edit::value(true));
        }
        if let Some(ref applies) = entry.applies_to
            && !applies.is_empty()
            && applies != "any"
        {
            table.insert("applies_to", toml_edit::value(applies.as_str()));
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

    /// Atomically set the keybinding for a built-in command. Removes any
    /// existing user `[[bind]]` entries that mention this command (or that
    /// disable its default via `command = "-"`), then optionally adds a
    /// disable-default entry and the new binding.
    ///
    /// `new_key` of `None` means "unbind" — the resulting state is that the
    /// command has no shortcut at all (default suppressed if any, no override).
    ///
    /// `new_when` is the keybinding *dispatch context* (`pane_focused` /
    /// `terminal_focused` / unset = global). For user commands the dispatch
    /// context is hard-coded to `pane_focused` and this parameter is ignored
    /// — only the `key` field is touched. The user command's `applies_to`
    /// run-filter is left alone.
    pub fn set_command_keybindings(
        &self,
        command_id: &str,
        new_keys: Vec<String>,
        new_when: Option<String>,
    ) -> Result<(), String> {
        // User commands store their key in the [[command]] entry, not [[bind]],
        // and carry at most one. Only the `key` field is updated — `applies_to`
        // is unrelated to keybindings and must not be touched here.
        if let Some(idx_str) = command_id.strip_prefix("user_command_") {
            let idx: usize = idx_str
                .parse()
                .map_err(|_| format!("Invalid user command id: {}", command_id))?;
            let _ = new_when; // dispatch context is implicit (pane_focused)
            return self.set_user_command_key(idx, new_keys.into_iter().next());
        }

        let defaults = commands::default_commands();
        let def = defaults
            .iter()
            .find(|d| d.id == command_id)
            .ok_or_else(|| format!("Unknown command: {}", command_id))?;
        let default_keys: Vec<String> = def.default_keys.iter().map(|k| expand_mod(k)).collect();
        let default_when = def.default_when.clone();

        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let new_content = apply_set_keybindings(
            &content,
            command_id,
            &new_keys,
            new_when,
            &default_keys,
            default_when,
        )?;
        std::fs::write(&file_path, new_content)
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;
        Ok(())
    }

    /// Reset a command's keybinding to its compiled-in default by removing
    /// any user `[[bind]]` entries referencing it (including disable entries
    /// that target its default key+when).
    pub fn reset_command_keybinding(&self, command_id: &str) -> Result<(), String> {
        if command_id.starts_with("user_command_") {
            // For user commands, "reset" means clear the key field.
            return self.set_command_keybindings(command_id, vec![], None);
        }

        let defaults = commands::default_commands();
        let def = defaults
            .iter()
            .find(|d| d.id == command_id)
            .ok_or_else(|| format!("Unknown command: {}", command_id))?;
        let default_keys: Vec<String> = def.default_keys.iter().map(|k| expand_mod(k)).collect();
        let default_when = def.default_when.clone();

        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let new_content =
            apply_reset_keybinding(&content, command_id, &default_keys, default_when)?;
        std::fs::write(&file_path, new_content)
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;
        Ok(())
    }

    /// Update only the `key` field on a user command entry. The `applies_to`
    /// run-filter and other fields are left intact. To edit those, use
    /// `update_user_command`.
    fn set_user_command_key(&self, index: usize, new_key: Option<String>) -> Result<(), String> {
        let file_path = self.settings_file_path();
        let content = std::fs::read_to_string(&file_path).unwrap_or_default();
        let mut doc = content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

        let arr = doc
            .get_mut("command")
            .and_then(|i| i.as_array_of_tables_mut())
            .ok_or_else(|| "No [[command]] array in settings.toml".to_string())?;
        let entry = arr
            .get_mut(index)
            .ok_or_else(|| format!("User command index {} out of range", index))?;

        match new_key {
            Some(k) if !k.is_empty() => {
                entry.insert("key", toml_edit::value(expand_mod(&k)));
            }
            _ => {
                entry.remove("key");
            }
        }

        std::fs::write(&file_path, doc.to_string())
            .map_err(|e| format!("Failed to write settings.toml: {}", e))?;
        Ok(())
    }

    fn setup_watcher(
        app_handle: &tauri::AppHandle,
        config_dir: &std::path::Path,
        resolved: Arc<RwLock<ResolvedPreferences>>,
        settings: Arc<arc_swap::ArcSwap<AppPreferences>>,
        notify_tx: tokio::sync::watch::Sender<()>,
    ) -> Option<RecommendedWatcher> {
        let config_dir_owned = config_dir.to_owned();
        let app_handle = app_handle.clone();

        // notify 8 has no built-in debouncing; debounce manually via a channel.
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
                        || guard.bookmarks != new_resolved.bookmarks
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

    fn load_and_resolve(config_dir: &std::path::Path) -> ResolvedPreferences {
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

    fn load_settings_file(path: &std::path::Path) -> SettingsFile {
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
        let command_defs = commands::default_commands();
        let mut bindings: Vec<(String, String, Option<String>)> = Vec::new();

        // 1. Default bindings from command defs. The first key of each
        // command is its primary (shown in menus, tooltips, the command
        // bar); the rest are equal-standing synonyms.
        for def in &command_defs {
            for key in &def.default_keys {
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

        // Build command info with resolved shortcuts. `shortcuts` carries
        // every resolved binding in resolution order (defaults first, user
        // additions after); the first one is the primary that menus,
        // tooltips and the command bar display.
        let mut commands: Vec<CommandInfo> = command_defs
            .iter()
            .map(|def| {
                let shortcuts: Vec<String> = resolved_bindings
                    .iter()
                    .filter(|b| b.command == def.id)
                    .map(|b| b.key.clone())
                    .collect();
                let resolved = resolved_bindings.iter().find(|b| b.command == def.id);
                let shortcut = shortcuts.first().cloned();
                let when = resolved.and_then(|b| b.when.clone());
                let shortcut_display = shortcut
                    .as_ref()
                    .map(|k| render_shortcut(k))
                    .unwrap_or_default();
                let default_keys: Vec<String> =
                    def.default_keys.iter().map(|k| expand_mod(k)).collect();
                // Overridden = the resolved key set differs from the default
                // set, or a resolved binding dispatches in a non-default
                // context. A command with no keys at all in both is pristine.
                let user_overridden = {
                    let mut resolved_sorted = shortcuts.clone();
                    resolved_sorted.sort();
                    let mut default_sorted = default_keys.clone();
                    default_sorted.sort();
                    resolved_sorted != default_sorted
                        || (!shortcuts.is_empty() && when.as_deref() != def.default_when.as_deref())
                };
                CommandInfo {
                    id: def.id.clone(),
                    name: def.name.clone(),
                    short_name: def.short_name.clone(),
                    category: def.category.clone(),
                    shortcut,
                    shortcut_display,
                    shortcuts,
                    needs_pane: def.needs_pane,
                    when,
                    applies_to: None,
                    default_keys,
                    default_when: def.default_when.clone(),
                    user_overridden,
                    scope: def.scope,
                }
            })
            .collect();

        // Add user commands as CommandInfo entries
        for (i, uc) in user_commands.iter().enumerate() {
            let cmd_id = format!("user_command_{}", i);
            let resolved = resolved_bindings.iter().find(|b| b.command == cmd_id);
            let shortcut = resolved.map(|b| b.key.clone());
            let shortcut_display = shortcut
                .as_ref()
                .map(|k| render_shortcut(k))
                .unwrap_or_default();

            commands.push(CommandInfo {
                id: cmd_id,
                name: uc.title.clone(),
                short_name: None,
                category: "User".to_string(),
                shortcuts: shortcut.iter().cloned().collect(),
                shortcut,
                shortcut_display,
                needs_pane: true,
                // User-command keybindings always dispatch in `pane_focused`
                // context; this is enforced in `resolve_bindings` above.
                when: resolved.and_then(|b| b.when.clone()),
                applies_to: uc.applies_to.clone(),
                default_keys: vec![],
                // Intrinsic dispatch context — the keybindings tab falls back
                // to this for the "When" column when no key is bound, so the
                // displayed context doesn't flip between "Global" and "Pane
                // focused" based on whether a shortcut exists.
                default_when: Some("pane_focused".to_string()),
                user_overridden: false,
                scope: commands::CommandScope::Main,
            });
        }

        // Cascade bookmarks: user + profile
        let mut bookmarks = user_file.bookmarks.clone();
        if let Some(pf) = &profile_file {
            bookmarks.extend(pf.bookmarks.iter().cloned());
        }

        // Determine which keys the user has explicitly set in their settings file
        let modified_keys = {
            let mut keys = Vec::new();
            for (section, value) in user_file.sections() {
                if let toml::Value::Table(table) = value {
                    for key in table.keys() {
                        keys.push(format!("{}.{}", section, key));
                    }
                }
            }
            keys
        };

        let schema = schemars::schema_for!(AppPreferences);
        let schema_json = serde_json::to_value(schema).unwrap_or_default();

        let locale = Some(settings.appearance.locale.clone())
            .filter(|l| !l.is_empty())
            .or_else(newt_common::locale::system_locale);

        ResolvedPreferences {
            settings,
            schema: schema_json,
            modified_keys,
            bindings: resolved_bindings,
            commands,
            bookmarks,
            user_commands,
            locale,
        }
    }

    /// Merge a `SettingsFile`'s raw TOML values on top of an existing `AppPreferences`.
    ///
    /// Serializes `base` to a TOML table, deep-merges only the keys present in
    /// `file` (so unset keys keep the base value), then deserializes back.
    fn merge_preferences(base: &AppPreferences, file: &SettingsFile) -> AppPreferences {
        let mut base_table =
            toml::Value::try_from(base).unwrap_or(toml::Value::Table(Default::default()));

        // Merge each section's raw TOML table on top of the serialized base
        if let toml::Value::Table(ref mut root) = base_table {
            for (section, value) in file.sections() {
                if let toml::Value::Table(t) = value {
                    deep_merge_table(
                        root.entry(section)
                            .or_insert(toml::Value::Table(Default::default())),
                        &toml::Value::Table(t.clone()),
                    );
                }
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

/// Pure transformation that powers `set_command_keybinding` for built-in
/// commands. Takes the existing `settings.toml` body, the command id, the
/// new `(key, when)` pair (None = unbind), and the compiled-in default
/// `(default_key, default_when)`. Returns the rewritten body. Doesn't touch
/// the filesystem — extracted so it's unit-testable.
fn apply_set_keybindings(
    content: &str,
    command_id: &str,
    new_keys: &[String],
    new_when: Option<String>,
    default_keys: &[String],
    default_when: Option<String>,
) -> Result<String, String> {
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

    // Rebuild [[bind]] dropping entries that mention this command or that
    // disable one of its defaults via `command = "-"` (we'll re-emit either
    // as needed below).
    let mut rebuilt: Vec<toml_edit::Table> = Vec::new();
    if let Some(arr) = doc.get_mut("bind").and_then(|i| i.as_array_of_tables_mut()) {
        for t in arr.iter() {
            let cmd = t.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let key = t
                .get("key")
                .and_then(|v| v.as_str())
                .map(expand_mod)
                .unwrap_or_default();
            let when = t
                .get("when")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mentions_self = cmd == command_id;
            let disables_our_default =
                cmd == "-" && default_keys.contains(&key) && when == default_when;
            if mentions_self || disables_our_default {
                continue;
            }
            rebuilt.push(t.clone());
        }
    }

    // Normalize and dedupe, preserving order (first key = primary).
    let mut seen = std::collections::HashSet::new();
    let new_keys: Vec<String> = new_keys
        .iter()
        .map(|k| expand_mod(k))
        .filter(|k| !k.is_empty() && seen.insert(k.clone()))
        .collect();

    // If the new key set + when matches the compiled-in defaults, no
    // [[bind]] entries are needed for this command — leave the slate clean.
    let mut new_sorted = new_keys.clone();
    new_sorted.sort();
    let mut default_sorted = default_keys.to_vec();
    default_sorted.sort();
    let is_back_to_default = new_sorted == default_sorted && new_when == default_when;

    if !is_back_to_default {
        // A dispatch-context change moves every binding: all defaults are
        // suppressed and every new key gets an explicit entry. Otherwise
        // defaults carried over in the new set stay implicit — suppress only
        // the dropped ones and add only the extra ones.
        let when_changed = new_when != default_when;
        for dk in default_keys {
            if when_changed || !new_keys.contains(dk) {
                let mut t = toml_edit::Table::new();
                t.insert("key", toml_edit::value(dk.as_str()));
                t.insert("command", toml_edit::value("-"));
                if let Some(w) = &default_when {
                    t.insert("when", toml_edit::value(w.as_str()));
                }
                t.set_implicit(true);
                rebuilt.push(t);
            }
        }

        for k in &new_keys {
            if when_changed || !default_keys.contains(k) {
                let mut t = toml_edit::Table::new();
                t.insert("key", toml_edit::value(k.as_str()));
                t.insert("command", toml_edit::value(command_id));
                if let Some(w) = &new_when {
                    t.insert("when", toml_edit::value(w.as_str()));
                }
                t.set_implicit(true);
                rebuilt.push(t);
            }
        }
    }

    doc.remove("bind");
    if !rebuilt.is_empty() {
        let mut arr = toml_edit::ArrayOfTables::new();
        for t in rebuilt {
            arr.push(t);
        }
        doc.insert("bind", toml_edit::Item::ArrayOfTables(arr));
    }

    Ok(doc.to_string())
}

/// Pure transformation that powers `reset_command_keybinding` for built-in
/// commands. Removes user `[[bind]]` entries mentioning the command or
/// occupying its default `(key, when)` slot, and clears the `key` field
/// of any `[[command]]` user-command entry currently squatting on that
/// slot. Symmetric reclamation — see callers for the rationale.
fn apply_reset_keybinding(
    content: &str,
    command_id: &str,
    default_keys: &[String],
    default_when: Option<String>,
) -> Result<String, String> {
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

    // Pass 1: rebuild [[bind]] dropping entries that mention this command,
    // or that occupy its default slot for any other command (including the
    // `-` disable marker).
    if let Some(arr) = doc.get_mut("bind").and_then(|i| i.as_array_of_tables_mut()) {
        let mut keep: Vec<toml_edit::Table> = Vec::new();
        for t in arr.iter() {
            let cmd = t.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let key = t
                .get("key")
                .and_then(|v| v.as_str())
                .map(expand_mod)
                .unwrap_or_default();
            let when = t
                .get("when")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let mentions_self = cmd == command_id;
            let occupies_default_slot = default_keys.contains(&key) && when == default_when;
            if mentions_self || occupies_default_slot {
                continue;
            }
            keep.push(t.clone());
        }
        doc.remove("bind");
        if !keep.is_empty() {
            let mut new_arr = toml_edit::ArrayOfTables::new();
            for t in keep {
                new_arr.push(t);
            }
            doc.insert("bind", toml_edit::Item::ArrayOfTables(new_arr));
        }
    }

    // Pass 2: clear the `key` field of any [[command]] entry currently bound
    // to one of the default keys. User-command keybindings always dispatch
    // in `pane_focused` context (see `resolve_bindings`), so this only
    // matters when our default's when is `pane_focused`.
    if default_when.as_deref() == Some("pane_focused")
        && !default_keys.is_empty()
        && let Some(arr) = doc
            .get_mut("command")
            .and_then(|i| i.as_array_of_tables_mut())
    {
        for t in arr.iter_mut() {
            let k = t
                .get("key")
                .and_then(|v| v.as_str())
                .map(expand_mod)
                .unwrap_or_default();
            if default_keys.contains(&k) {
                t.remove("key");
            }
        }
    }

    Ok(doc.to_string())
}

/// Replace the `[[bookmark]]` array-of-tables of `doc` with `entries`,
/// dropping the key entirely when the list is empty.
fn set_bookmark_array(doc: &mut toml_edit::DocumentMut, entries: Vec<toml_edit::Table>) {
    doc.remove("bookmark");
    if !entries.is_empty() {
        let mut arr = toml_edit::ArrayOfTables::new();
        for t in entries {
            arr.push(t);
        }
        doc.insert("bookmark", toml_edit::Item::ArrayOfTables(arr));
    }
}

fn bookmark_tables(doc: &toml_edit::DocumentMut) -> Vec<toml_edit::Table> {
    doc.get("bookmark")
        .and_then(|i| i.as_array_of_tables())
        .map(|arr| arr.iter().cloned().collect())
        .unwrap_or_default()
}

/// Pure transformation powering `add_bookmark`: prepend an entry for `path`,
/// dropping any entries that already point at it, so a re-bookmark bumps the
/// path to the top instead of accumulating duplicates. Returns the rewritten
/// body and whether the path was bookmarked before.
fn apply_add_bookmark(
    content: &str,
    path: &str,
    name: Option<&str>,
) -> Result<(String, bool), String> {
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

    let existing = bookmark_tables(&doc);
    let mut kept: Vec<toml_edit::Table> = Vec::with_capacity(existing.len() + 1);

    let mut entry = toml_edit::Table::new();
    entry.insert("path", toml_edit::value(path));
    if let Some(n) = name {
        entry.insert("name", toml_edit::value(n));
    }
    kept.push(entry);

    let before = existing.len();
    let retained: Vec<toml_edit::Table> = existing
        .into_iter()
        .filter(|t| t.get("path").and_then(|v| v.as_str()) != Some(path))
        .collect();
    let was_bookmarked = retained.len() != before;
    kept.extend(retained);

    set_bookmark_array(&mut doc, kept);

    Ok((doc.to_string(), was_bookmarked))
}

/// Pure transformation powering `remove_bookmark`. Removes *every* entry for
/// the path — a hand-edited file can still hold duplicates, and the hot-paths
/// dialog drops them all from its list on confirm.
fn apply_remove_bookmark(content: &str, path: &str) -> Result<String, String> {
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;

    let kept: Vec<toml_edit::Table> = bookmark_tables(&doc)
        .into_iter()
        .filter(|t| t.get("path").and_then(|v| v.as_str()) != Some(path))
        .collect();
    set_bookmark_array(&mut doc, kept);

    Ok(doc.to_string())
}

/// Pure transformation powering `restore_bookmarks`: graft `snapshot`'s
/// `[[bookmark]]` array onto the current body, verbatim.
fn apply_restore_bookmarks(content: &str, snapshot: &str) -> Result<String, String> {
    let mut doc = content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("Failed to parse settings.toml: {}", e))?;
    let snapshot_doc = snapshot
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("Failed to parse bookmark snapshot: {}", e))?;

    set_bookmark_array(&mut doc, bookmark_tables(&snapshot_doc));

    Ok(doc.to_string())
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
        serde_json::Value::Array(arr) => {
            let mut toml_arr = toml_edit::Array::new();
            for item in arr {
                toml_arr.push(json_to_toml_edit_value(item)?);
            }
            Ok(toml_edit::Value::Array(toml_arr))
        }
        serde_json::Value::Object(obj) => {
            let mut table = toml_edit::InlineTable::new();
            for (k, v) in obj {
                table.insert(k, json_to_toml_edit_value(v)?);
            }
            Ok(toml_edit::Value::InlineTable(table))
        }
        _ => Err(format!("unsupported value type for TOML: {:?}", value)),
    }
}
