//! Preference-like machinery for machine-written, ephemeral-ish UI state
//! (column widths and the like). Unlike `settings.toml` this is not
//! user-authored: plain JSON, no schema, no profiles, no file watcher.

use std::collections::BTreeMap;

use log::warn;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::connections::{ConnectionKind, OpenIn};

/// Cap on remembered ad-hoc connections; oldest roll off.
const RECENT_CONNECTIONS_MAX: usize = 12;

/// App-wide runtime state persisted to `state.json` in the config dir.
/// Every field must default so old files keep deserializing as the
/// struct grows. Unknown fields are rejected so `update_key` can't
/// silently write garbage; a newer file read by an older binary just
/// falls back to defaults, which is fine for ephemeral state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeState {
    /// Pane handle ("0"/"1") → column key → width in px.
    pub column_widths: BTreeMap<String, BTreeMap<String, f64>>,
    /// Webview zoom factor, applied to every window.
    pub zoom: f64,
    /// Persisted panel layout sizes.
    pub layout: LayoutState,
    /// Sticky last-used Copy/Move option toggles.
    pub copy_move: CopyMoveDefaults,
    /// Sticky last-used Search option toggles.
    pub search: SearchDefaults,
    /// Ad-hoc (unsaved) connections, most-recent first. Saved profiles are
    /// never stored here (they live in connections.toml); the Quick Connect
    /// palette filters any recent that matches a saved profile.
    pub recent_connections: Vec<RecentConnection>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            column_widths: BTreeMap::new(),
            zoom: 1.0,
            layout: LayoutState::default(),
            copy_move: CopyMoveDefaults::default(),
            search: SearchDefaults::default(),
            recent_connections: Vec::new(),
        }
    }
}

/// A remembered ad-hoc connection target. Carries only the secret-free
/// `ConnectionKind` (S3 keys, SSH auth, etc. never land here) plus how it
/// opened, so it can be re-launched exactly like the connect dialog would.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct RecentConnection {
    #[serde(default)]
    pub open_in: OpenIn,
    #[serde(flatten)]
    pub kind: ConnectionKind,
}

/// Persisted sizes for resizable panels. The file-pane split is intentionally
/// absent — it always opens 50/50.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutState {
    /// Terminal panel height in px. `None` uses the built-in default.
    pub terminal_height: Option<f64>,
}

/// Last-used Copy/Move toggles, re-seeded into the dialog on open.
/// `create_symlink` is deliberately not here — a sticky "create symlink"
/// would silently change what Copy does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(default, deny_unknown_fields)]
pub struct CopyMoveDefaults {
    pub preserve_timestamps: bool,
    pub preserve_owner: bool,
    pub preserve_group: bool,
}

/// Last-used Search toggles, re-seeded into fresh searches (the refine flow
/// still restores from the live search VFS instead).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(default, deny_unknown_fields)]
pub struct SearchDefaults {
    pub case_sensitive: bool,
    pub content_is_regex: bool,
    pub follow_symlinks: bool,
}

pub struct RuntimeStateManager {
    path: std::path::PathBuf,
    state: RwLock<RuntimeState>,
    app_handle: tauri::AppHandle,
}

impl RuntimeStateManager {
    pub fn new(app_handle: &tauri::AppHandle, config_dir: &std::path::Path) -> Self {
        let path = config_dir.join("state.json");
        let state = match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(state) => state,
                Err(e) => {
                    warn!("Failed to parse {:?}: {}. Using defaults.", path, e);
                    RuntimeState::default()
                }
            },
            Err(_) => RuntimeState::default(),
        };
        Self {
            path,
            state: RwLock::new(state),
            app_handle: app_handle.clone(),
        }
    }

    pub fn state(&self) -> RuntimeState {
        self.state.read().clone()
    }

    /// Set a single value by dotted path (e.g. "column_widths.0.name"),
    /// persist to disk and broadcast the new state to every window.
    pub fn update_key(&self, key: &str, value: serde_json::Value) -> Result<(), String> {
        let new_state = {
            let mut guard = self.state.write();
            let mut json = serde_json::to_value(&*guard).expect("RuntimeState serializes");
            set_dotted_path(&mut json, key, value)?;
            let new_state: RuntimeState = serde_json::from_value(json)
                .map_err(|e| format!("Invalid runtime state key {}: {}", key, e))?;
            *guard = new_state.clone();
            new_state
        };
        self.persist_and_broadcast(&new_state)
    }

    /// Record an ad-hoc connection at the front of the MRU list, de-duped by
    /// target identity and capped. Best-effort — a persistence failure only
    /// warns, since the connection itself already succeeded.
    pub fn record_recent_connection(&self, kind: ConnectionKind, open_in: OpenIn) {
        let new_state = {
            let mut guard = self.state.write();
            push_recent(&mut guard.recent_connections, kind, open_in);
            guard.clone()
        };
        if let Err(e) = self.persist_and_broadcast(&new_state) {
            warn!("failed to persist recent connection: {}", e);
        }
    }

    /// Drop the recent connection with the given target identity.
    pub fn forget_recent_connection(&self, identity: &str) -> Result<(), String> {
        let new_state = {
            let mut guard = self.state.write();
            guard
                .recent_connections
                .retain(|r| r.kind.identity() != identity);
            guard.clone()
        };
        self.persist_and_broadcast(&new_state)
    }

    fn persist_and_broadcast(&self, new_state: &RuntimeState) -> Result<(), String> {
        std::fs::write(
            &self.path,
            serde_json::to_string_pretty(new_state).expect("RuntimeState serializes"),
        )
        .map_err(|e| format!("Failed to write {:?}: {}", self.path, e))?;
        let _ = self.app_handle.emit("update:runtime-state", new_state);
        Ok(())
    }
}

/// Prepend an ad-hoc connection, de-duped by target identity (an existing
/// entry for the same target is removed first, so the list stays MRU) and
/// capped at `RECENT_CONNECTIONS_MAX`.
fn push_recent(list: &mut Vec<RecentConnection>, kind: ConnectionKind, open_in: OpenIn) {
    let identity = kind.identity();
    list.retain(|r| r.kind.identity() != identity);
    list.insert(0, RecentConnection { open_in, kind });
    list.truncate(RECENT_CONNECTIONS_MAX);
}

/// Set `value` at a dotted path inside a JSON object tree, creating
/// intermediate objects as needed.
fn set_dotted_path(
    root: &mut serde_json::Value,
    key: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(format!("invalid key: {}", key));
    }
    let (table_parts, leaf) = parts.split_at(parts.len() - 1);
    let mut node = root;
    for part in table_parts {
        let obj = node
            .as_object_mut()
            .ok_or_else(|| format!("{} is not an object", part))?;
        node = obj
            .entry(part.to_string())
            .or_insert(serde_json::Value::Object(Default::default()));
    }
    node.as_object_mut()
        .ok_or_else(|| format!("{} is not an object", leaf[0]))?
        .insert(leaf[0].to_string(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_dotted_path_creates_intermediates() {
        let mut root = json!({});
        set_dotted_path(&mut root, "column_widths.0.name", json!(250.0)).unwrap();
        set_dotted_path(&mut root, "column_widths.0.size", json!(100.0)).unwrap();
        set_dotted_path(&mut root, "column_widths.1.name", json!(300.0)).unwrap();
        assert_eq!(
            root,
            json!({"column_widths": {"0": {"name": 250.0, "size": 100.0}, "1": {"name": 300.0}}})
        );
    }

    #[test]
    fn set_dotted_path_overwrites() {
        let mut root = json!({"column_widths": {"0": {"name": 250.0}}});
        set_dotted_path(&mut root, "column_widths.0.name", json!(80.0)).unwrap();
        assert_eq!(root, json!({"column_widths": {"0": {"name": 80.0}}}));
    }

    #[test]
    fn set_dotted_path_rejects_bad_keys() {
        let mut root = json!({});
        assert!(set_dotted_path(&mut root, "", json!(1)).is_err());
        assert!(set_dotted_path(&mut root, "a..b", json!(1)).is_err());
    }

    #[test]
    fn update_validates_against_schema() {
        // An unknown top-level key must fail RuntimeState deserialization.
        let mut json = serde_json::to_value(RuntimeState::default()).unwrap();
        set_dotted_path(&mut json, "bogus_field.x", serde_json::json!(1)).unwrap();
        assert!(serde_json::from_value::<RuntimeState>(json).is_err());
    }

    fn ssh(host: &str) -> ConnectionKind {
        ConnectionKind::Ssh {
            host: host.to_string(),
            forward_agent: false,
            login_shell: true,
        }
    }

    #[test]
    fn push_recent_dedups_and_bumps_to_front() {
        let mut list = Vec::new();
        push_recent(&mut list, ssh("a"), OpenIn::Window);
        push_recent(&mut list, ssh("b"), OpenIn::Window);
        // Re-connecting to `a` moves it to the front without duplicating.
        push_recent(&mut list, ssh("a"), OpenIn::Pane);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].kind.identity(), "ssh:a");
        assert_eq!(list[0].open_in, OpenIn::Pane);
        assert_eq!(list[1].kind.identity(), "ssh:b");
    }

    #[test]
    fn push_recent_caps_oldest_out() {
        let mut list = Vec::new();
        for i in 0..(RECENT_CONNECTIONS_MAX + 3) {
            push_recent(&mut list, ssh(&format!("h{i}")), OpenIn::Window);
        }
        assert_eq!(list.len(), RECENT_CONNECTIONS_MAX);
        // Newest first, oldest three rolled off.
        assert_eq!(
            list[0].kind.identity(),
            format!("ssh:h{}", RECENT_CONNECTIONS_MAX + 2)
        );
    }

    #[test]
    fn state_roundtrips_through_json() {
        let mut state = RuntimeState::default();
        state
            .column_widths
            .entry("0".into())
            .or_default()
            .insert("name".into(), 250.0);
        let text = serde_json::to_string_pretty(&state).unwrap();
        let back: RuntimeState = serde_json::from_str(&text).unwrap();
        assert_eq!(state, back);
        // A file from an older version (missing fields) must default:
        let old: RuntimeState = serde_json::from_str("{}").unwrap();
        assert_eq!(old, RuntimeState::default());
    }
}
