//! Preference-like machinery for machine-written, ephemeral-ish UI state
//! (column widths and the like). Unlike `settings.toml` this is not
//! user-authored: plain JSON, no schema, no profiles, no file watcher.

use std::collections::BTreeMap;

use log::warn;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tauri::Emitter;

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
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            column_widths: BTreeMap::new(),
            zoom: 1.0,
        }
    }
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
        std::fs::write(
            &self.path,
            serde_json::to_string_pretty(&new_state).expect("RuntimeState serializes"),
        )
        .map_err(|e| format!("Failed to write {:?}: {}", self.path, e))?;
        let _ = self.app_handle.emit("update:runtime-state", &new_state);
        Ok(())
    }
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
