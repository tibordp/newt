use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// User-facing preferences that can be set in settings.toml.
///
/// Serde defaults ensure every field has a compiled-in default. The JSON Schema
/// is derived via `schemars` so the frontend settings editor can be generated
/// automatically.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct AppPreferences {
    #[serde(default)]
    pub appearance: AppearancePreferences,
    #[serde(default)]
    pub behavior: BehaviorPreferences,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            appearance: AppearancePreferences::default(),
            behavior: BehaviorPreferences::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct AppearancePreferences {
    /// Show hidden files by default when opening a new window.
    #[schemars(title = "Show Hidden Files")]
    pub show_hidden: bool,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self { show_hidden: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default)]
pub struct BehaviorPreferences {
    /// Ask for confirmation before deleting files.
    #[schemars(title = "Confirm Delete")]
    pub confirm_delete: bool,
}

impl Default for BehaviorPreferences {
    fn default() -> Self {
        Self {
            confirm_delete: true,
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

    /// Keybinding override entries.
    #[serde(default, rename = "bind")]
    pub bindings: Vec<KeybindingEntry>,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            profile: None,
            appearance: default_toml_table(),
            behavior: default_toml_table(),
            bindings: Vec::new(),
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
