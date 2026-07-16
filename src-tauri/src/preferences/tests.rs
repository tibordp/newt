use super::*;
use crate::preferences::schema::*;

// ---------------------------------------------------------------------------
// expand_mod
// ---------------------------------------------------------------------------

#[test]
fn expand_mod_replaces_mod_prefix() {
    let result = expand_mod("mod+a");
    // On non-macOS, mod -> ctrl; on macOS, mod -> meta
    if cfg!(target_os = "macos") {
        assert_eq!(result, "meta+a");
    } else {
        assert_eq!(result, "ctrl+a");
    }
}

#[test]
fn expand_mod_replaces_mod_in_middle() {
    let result = expand_mod("shift+mod+a");
    if cfg!(target_os = "macos") {
        assert_eq!(result, "shift+meta+a");
    } else {
        assert_eq!(result, "shift+ctrl+a");
    }
}

#[test]
fn expand_mod_lowercases() {
    assert_eq!(expand_mod("Ctrl+Shift+A"), "ctrl+shift+a");
}

#[test]
fn expand_mod_no_mod() {
    assert_eq!(expand_mod("ctrl+shift+f5"), "ctrl+shift+f5");
}

// ---------------------------------------------------------------------------
// resolve_bindings
// ---------------------------------------------------------------------------

#[test]
fn resolve_bindings_basic() {
    let bindings = vec![
        ("ctrl+a".into(), "select_all".into(), None),
        ("ctrl+c".into(), "copy".into(), Some("pane_focused".into())),
    ];
    let resolved = resolve_bindings(bindings);
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].key, "ctrl+a");
    assert_eq!(resolved[0].command, "select_all");
    assert_eq!(resolved[1].command, "copy");
    assert_eq!(resolved[1].when, Some("pane_focused".into()));
}

#[test]
fn resolve_bindings_later_overrides_earlier() {
    let bindings = vec![
        ("ctrl+a".into(), "cmd1".into(), None),
        ("ctrl+a".into(), "cmd2".into(), None),
    ];
    let resolved = resolve_bindings(bindings);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].command, "cmd2");
}

#[test]
fn resolve_bindings_same_key_different_when() {
    let bindings = vec![
        ("ctrl+a".into(), "cmd1".into(), None),
        ("ctrl+a".into(), "cmd2".into(), Some("pane_focused".into())),
    ];
    let resolved = resolve_bindings(bindings);
    // Same key but different "when" = different entries
    assert_eq!(resolved.len(), 2);
}

#[test]
fn resolve_bindings_removal_with_dash() {
    let bindings = vec![
        ("ctrl+a".into(), "select_all".into(), None),
        ("ctrl+a".into(), "-".into(), None), // removal
    ];
    let resolved = resolve_bindings(bindings);
    assert!(resolved.is_empty());
}

#[test]
fn resolve_bindings_removal_then_readd() {
    let bindings = vec![
        ("ctrl+a".into(), "cmd1".into(), None),
        ("ctrl+a".into(), "-".into(), None),
    ];
    let resolved = resolve_bindings(bindings);
    assert!(resolved.is_empty());

    // A later entry on the same (key, when) overwrites the "-" disable:
    let bindings2 = vec![
        ("ctrl+a".into(), "cmd1".into(), None),
        ("ctrl+a".into(), "-".into(), None),
        ("ctrl+a".into(), "cmd3".into(), None),
    ];
    let resolved2 = resolve_bindings(bindings2);
    assert_eq!(resolved2.len(), 1);
    assert_eq!(resolved2[0].command, "cmd3");
}

#[test]
fn resolve_bindings_mod_expansion() {
    let bindings = vec![("Mod+A".into(), "select_all".into(), None)];
    let resolved = resolve_bindings(bindings);
    assert_eq!(resolved.len(), 1);
    if cfg!(target_os = "macos") {
        assert_eq!(resolved[0].key, "meta+a");
    } else {
        assert_eq!(resolved[0].key, "ctrl+a");
    }
}

#[test]
fn resolve_bindings_empty() {
    let resolved = resolve_bindings(vec![]);
    assert!(resolved.is_empty());
}

// ---------------------------------------------------------------------------
// render_shortcut
// ---------------------------------------------------------------------------

#[test]
fn render_shortcut_simple() {
    let parts = render_shortcut("ctrl+shift+f5");
    assert_eq!(parts, vec!["Ctrl", "Shift", "F5"]);
}

#[test]
fn render_shortcut_single_key() {
    let parts = render_shortcut("escape");
    assert_eq!(parts, vec!["Escape"]);
}

// ---------------------------------------------------------------------------
// merge_preferences
// ---------------------------------------------------------------------------

#[test]
fn merge_preferences_empty_file_returns_defaults() {
    let defaults = AppPreferences::default();
    let file = SettingsFile::default();
    let merged = PreferencesManager::merge_preferences(&defaults, &file);
    assert_eq!(merged, defaults);
}

#[test]
fn merge_preferences_overrides_single_field() {
    let defaults = AppPreferences::default();
    let mut file = SettingsFile::default();

    // Override show_hidden to true (default is false)
    let mut table = toml::map::Map::new();
    table.insert("show_hidden".into(), toml::Value::Boolean(true));
    file.appearance = toml::Value::Table(table);

    let merged = PreferencesManager::merge_preferences(&defaults, &file);
    assert!(merged.appearance.show_hidden);
    // Other fields should remain default
    assert!(merged.appearance.folders_first);
    assert!(merged.behavior.confirm_delete);
}

#[test]
fn merge_preferences_preserves_unset_fields() {
    let defaults = AppPreferences::default();
    let mut file = SettingsFile::default();

    // Only set confirm_delete
    let mut table = toml::map::Map::new();
    table.insert("confirm_delete".into(), toml::Value::Boolean(false));
    file.behavior = toml::Value::Table(table);

    let merged = PreferencesManager::merge_preferences(&defaults, &file);
    assert!(!merged.behavior.confirm_delete);
    // keep_terminal_open should still be the default (true)
    assert!(merged.behavior.keep_terminal_open);
}

#[test]
fn merge_preferences_wrong_type_falls_back() {
    let defaults = AppPreferences::default();
    let mut file = SettingsFile::default();

    // Set show_hidden to a string instead of bool — should fall back to defaults
    let mut table = toml::map::Map::new();
    table.insert(
        "show_hidden".into(),
        toml::Value::String("not_a_bool".into()),
    );
    file.appearance = toml::Value::Table(table);

    let merged = PreferencesManager::merge_preferences(&defaults, &file);
    // Should fall back to defaults since deserialization fails
    assert_eq!(merged, defaults);
}

#[test]
fn merge_preferences_cascading_user_then_profile() {
    let defaults = AppPreferences::default();

    // User file: show_hidden = true
    let mut user_file = SettingsFile::default();
    let mut table = toml::map::Map::new();
    table.insert("show_hidden".into(), toml::Value::Boolean(true));
    user_file.appearance = toml::Value::Table(table);

    let user_prefs = PreferencesManager::merge_preferences(&defaults, &user_file);
    assert!(user_prefs.appearance.show_hidden);

    // Profile: folders_first = false
    let mut profile_file = SettingsFile::default();
    let mut table2 = toml::map::Map::new();
    table2.insert("folders_first".into(), toml::Value::Boolean(false));
    profile_file.appearance = toml::Value::Table(table2);

    let final_prefs = PreferencesManager::merge_preferences(&user_prefs, &profile_file);
    // Both overrides should be present
    assert!(final_prefs.appearance.show_hidden); // from user
    assert!(!final_prefs.appearance.folders_first); // from profile
}

// ---------------------------------------------------------------------------
// deep_merge_table
// ---------------------------------------------------------------------------

#[test]
fn deep_merge_table_replaces_scalars() {
    let mut base = toml::Value::Boolean(false);
    let overlay = toml::Value::Boolean(true);
    deep_merge_table(&mut base, &overlay);
    assert_eq!(base, toml::Value::Boolean(true));
}

#[test]
fn deep_merge_table_merges_tables() {
    let mut base = toml::Value::Table({
        let mut t = toml::map::Map::new();
        t.insert("a".into(), toml::Value::Integer(1));
        t.insert("b".into(), toml::Value::Integer(2));
        t
    });

    let overlay = toml::Value::Table({
        let mut t = toml::map::Map::new();
        t.insert("b".into(), toml::Value::Integer(20)); // override
        t.insert("c".into(), toml::Value::Integer(30)); // new
        t
    });

    deep_merge_table(&mut base, &overlay);

    let t = base.as_table().unwrap();
    assert_eq!(t["a"].as_integer(), Some(1)); // preserved
    assert_eq!(t["b"].as_integer(), Some(20)); // overridden
    assert_eq!(t["c"].as_integer(), Some(30)); // added
}

#[test]
fn deep_merge_table_nested() {
    let mut base = toml::Value::Table({
        let mut outer = toml::map::Map::new();
        let mut inner = toml::map::Map::new();
        inner.insert("x".into(), toml::Value::Integer(1));
        inner.insert("y".into(), toml::Value::Integer(2));
        outer.insert("inner".into(), toml::Value::Table(inner));
        outer
    });

    let overlay = toml::Value::Table({
        let mut outer = toml::map::Map::new();
        let mut inner = toml::map::Map::new();
        inner.insert("y".into(), toml::Value::Integer(20));
        outer.insert("inner".into(), toml::Value::Table(inner));
        outer
    });

    deep_merge_table(&mut base, &overlay);

    let inner = base.as_table().unwrap()["inner"].as_table().unwrap();
    assert_eq!(inner["x"].as_integer(), Some(1)); // preserved
    assert_eq!(inner["y"].as_integer(), Some(20)); // overridden
}

// ---------------------------------------------------------------------------
// apply_set_keybinding
// ---------------------------------------------------------------------------

fn parse_binds(toml_str: &str) -> Vec<(String, String, Option<String>)> {
    let doc: toml_edit::DocumentMut = toml_str.parse().expect("valid toml");
    let arr = match doc.get("bind").and_then(|i| i.as_array_of_tables()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .map(|t| {
            (
                t.get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                t.get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                t.get("when").and_then(|v| v.as_str()).map(String::from),
            )
        })
        .collect()
}

#[test]
fn apply_set_keybinding_remap_writes_disable_and_new() {
    // Built-in `select_all` defaults to ctrl+a / pane_focused. Remap to ctrl+shift+a.
    let out = apply_set_keybinding(
        "",
        "select_all",
        Some("ctrl+shift+a".into()),
        Some("pane_focused".into()),
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    let binds = parse_binds(&out);
    assert_eq!(
        binds,
        vec![
            ("ctrl+a".into(), "-".into(), Some("pane_focused".into())),
            (
                "ctrl+shift+a".into(),
                "select_all".into(),
                Some("pane_focused".into())
            ),
        ]
    );
}

#[test]
fn apply_set_keybinding_back_to_default_clears_overrides() {
    // Existing override in file. Setting back to default should leave file empty.
    let initial = r#"
[[bind]]
key = "ctrl+a"
command = "-"
when = "pane_focused"

[[bind]]
key = "ctrl+shift+a"
command = "select_all"
when = "pane_focused"
"#;
    let out = apply_set_keybinding(
        initial,
        "select_all",
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    assert!(parse_binds(&out).is_empty(), "got: {}", out);
}

#[test]
fn apply_set_keybinding_unbind_writes_only_disable() {
    let out = apply_set_keybinding(
        "",
        "select_all",
        None,
        None,
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    let binds = parse_binds(&out);
    assert_eq!(
        binds,
        vec![("ctrl+a".into(), "-".into(), Some("pane_focused".into()))]
    );
}

#[test]
fn apply_set_keybinding_no_default_is_pure_add() {
    // navigate_back has no default key. Bind to alt+x.
    let out = apply_set_keybinding(
        "",
        "navigate_back",
        Some("alt+x".into()),
        Some("pane_focused".into()),
        None,
        Some("pane_focused".into()),
    )
    .unwrap();
    let binds = parse_binds(&out);
    assert_eq!(
        binds,
        vec![(
            "alt+x".into(),
            "navigate_back".into(),
            Some("pane_focused".into())
        )]
    );
}

#[test]
fn apply_set_keybinding_idempotent_on_repeated_remap() {
    // Repeated remap to the same key should produce identical output.
    let first = apply_set_keybinding(
        "",
        "select_all",
        Some("ctrl+shift+a".into()),
        Some("pane_focused".into()),
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    let second = apply_set_keybinding(
        &first,
        "select_all",
        Some("ctrl+shift+a".into()),
        Some("pane_focused".into()),
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    assert_eq!(parse_binds(&first), parse_binds(&second));
}

// ---------------------------------------------------------------------------
// apply_reset_keybinding
// ---------------------------------------------------------------------------

#[test]
fn apply_reset_keybinding_removes_overrides() {
    let initial = r#"
[[bind]]
key = "ctrl+a"
command = "-"
when = "pane_focused"

[[bind]]
key = "ctrl+shift+a"
command = "select_all"
when = "pane_focused"
"#;
    let out = apply_reset_keybinding(
        initial,
        "select_all",
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    assert!(parse_binds(&out).is_empty());
}

#[test]
fn apply_reset_keybinding_evicts_squatter_in_bind() {
    // Another command holds select_all's default slot via [[bind]]. Reset
    // should reclaim it.
    let initial = r#"
[[bind]]
key = "ctrl+a"
command = "some_other_cmd"
when = "pane_focused"
"#;
    let out = apply_reset_keybinding(
        initial,
        "select_all",
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    assert!(parse_binds(&out).is_empty(), "got: {}", out);
}

#[test]
fn apply_reset_keybinding_evicts_user_command_squatter() {
    // A user command has stolen select_all's default key. Reset should
    // clear the user command's `key` field.
    let initial = r#"
[[command]]
title = "My Cmd"
run = "echo hi"
key = "ctrl+a"
applies_to = "file"
"#;
    let out = apply_reset_keybinding(
        initial,
        "select_all",
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    let doc: toml_edit::DocumentMut = out.parse().unwrap();
    let arr = doc["command"].as_array_of_tables().unwrap();
    let cmd = arr.get(0).unwrap();
    assert!(
        cmd.get("key").is_none(),
        "key should be cleared. got: {}",
        out
    );
    // applies_to must be preserved.
    assert_eq!(cmd.get("applies_to").and_then(|v| v.as_str()), Some("file"));
    // title and run preserved too.
    assert_eq!(cmd.get("title").and_then(|v| v.as_str()), Some("My Cmd"));
}

#[test]
fn apply_reset_keybinding_preserves_unrelated_binds() {
    let initial = r#"
[[bind]]
key = "ctrl+a"
command = "-"
when = "pane_focused"

[[bind]]
key = "ctrl+b"
command = "delete_selected"
when = "pane_focused"
"#;
    let out = apply_reset_keybinding(
        initial,
        "select_all",
        Some("ctrl+a".into()),
        Some("pane_focused".into()),
    )
    .unwrap();
    let binds = parse_binds(&out);
    assert_eq!(
        binds,
        vec![(
            "ctrl+b".into(),
            "delete_selected".into(),
            Some("pane_focused".into())
        )]
    );
}

#[test]
fn apply_reset_keybinding_no_default_is_noop_for_squatter_check() {
    // Command with no default — reset should still drop entries mentioning
    // it but not touch unrelated bindings.
    let initial = r#"
[[bind]]
key = "alt+x"
command = "navigate_back"
when = "pane_focused"

[[bind]]
key = "ctrl+b"
command = "delete_selected"
when = "pane_focused"
"#;
    let out = apply_reset_keybinding(initial, "navigate_back", None, Some("pane_focused".into()))
        .unwrap();
    let binds = parse_binds(&out);
    assert_eq!(
        binds,
        vec![(
            "ctrl+b".into(),
            "delete_selected".into(),
            Some("pane_focused".into())
        )]
    );
}

// ---------------------------------------------------------------------------
// merge_preferences
// ---------------------------------------------------------------------------

/// Regression: `[archives]` and `[environment]` used to be silently
/// dropped on load — `SettingsFile` didn't deserialize them and
/// `merge_preferences` didn't merge them, so the settings editor
/// persisted values a restart discarded. Every `AppPreferences` group
/// must round-trip through a settings file.
#[test]
fn merge_preferences_covers_every_group() {
    let file: SettingsFile = toml::from_str(
        r#"
[appearance]
folders_first = false

[behavior]
quick_search = false

[enrichers]
git_status = false

[archives]
zstd_level = 9

[hot_paths]
mounts = false

[environment]
extra_path = ["/opt/custom/bin"]
"#,
    )
    .unwrap();

    let merged = PreferencesManager::merge_preferences(&AppPreferences::default(), &file);
    assert!(!merged.appearance.folders_first);
    assert!(!merged.behavior.quick_search);
    assert!(!merged.enrichers.git_status);
    assert_eq!(merged.archives.zstd_level, 9);
    assert!(!merged.hot_paths.mounts);
    assert_eq!(merged.environment.extra_path, vec!["/opt/custom/bin"]);

    // Unset keys keep their defaults.
    assert_eq!(merged.behavior.history_retention, 200);
    assert_eq!(merged.archives.zip_level, 6);
}
