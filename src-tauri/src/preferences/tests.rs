use super::*;
use crate::preferences::commands::{CommandScope, default_commands};
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

#[test]
fn command_registry_invariants() {
    let defs = default_commands();
    let mut ids = std::collections::HashSet::new();
    for def in &defs {
        assert!(
            ids.insert(def.id.clone()),
            "duplicate command id {}",
            def.id
        );
        // Non-main commands are dispatched by their window's frontend keyed
        // on the scope context; a different `when` would never match.
        match def.scope {
            CommandScope::Viewer => assert_eq!(def.default_when.as_deref(), Some("viewer")),
            CommandScope::Editor => assert_eq!(def.default_when.as_deref(), Some("editor")),
            CommandScope::Main => assert!(!def.id.starts_with("viewer_")),
        }
    }
}

#[test]
fn settings_file_sections_cover_all_preference_groups() {
    // A group absent from SettingsFile/sections() is reported as not a
    // setting on load.
    let table = toml::Value::try_from(AppPreferences::default()).unwrap();
    let file = SettingsFile::default();
    let section_names: Vec<&str> = file.sections().iter().map(|(n, _)| *n).collect();
    for key in table.as_table().unwrap().keys() {
        assert!(
            section_names.contains(&key.as_str()),
            "AppPreferences group `{}` is not wired into SettingsFile::sections()",
            key
        );
    }
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
// apply_set_keybindings
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
    let out = apply_set_keybindings(
        "",
        "select_all",
        &["ctrl+shift+a".into()],
        Some("pane_focused".into()),
        &["ctrl+a".into()],
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
    let out = apply_set_keybindings(
        initial,
        "select_all",
        &["ctrl+a".into()],
        Some("pane_focused".into()),
        &["ctrl+a".into()],
        Some("pane_focused".into()),
    )
    .unwrap();
    assert!(parse_binds(&out).is_empty(), "got: {}", out);
}

#[test]
fn apply_set_keybinding_unbind_writes_only_disable() {
    let out = apply_set_keybindings(
        "",
        "select_all",
        &[],
        None,
        &["ctrl+a".into()],
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
    let out = apply_set_keybindings(
        "",
        "navigate_back",
        &["alt+x".into()],
        Some("pane_focused".into()),
        &[],
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
    let first = apply_set_keybindings(
        "",
        "select_all",
        &["ctrl+shift+a".into()],
        Some("pane_focused".into()),
        &["ctrl+a".into()],
        Some("pane_focused".into()),
    )
    .unwrap();
    let second = apply_set_keybindings(
        &first,
        "select_all",
        &["ctrl+shift+a".into()],
        Some("pane_focused".into()),
        &["ctrl+a".into()],
        Some("pane_focused".into()),
    )
    .unwrap();
    assert_eq!(parse_binds(&first), parse_binds(&second));
}

#[test]
fn apply_set_keybindings_add_secondary_keeps_default_implicit() {
    // delete_selected keeps f8+delete defaults and gains ctrl+d — only the
    // addition is written; the defaults stay implicit.
    let out = apply_set_keybindings(
        "",
        "delete_selected",
        &["f8".into(), "delete".into(), "ctrl+d".into()],
        Some("pane_focused".into()),
        &["f8".into(), "delete".into()],
        Some("pane_focused".into()),
    )
    .unwrap();
    let binds = parse_binds(&out);
    assert_eq!(
        binds,
        vec![(
            "ctrl+d".into(),
            "delete_selected".into(),
            Some("pane_focused".into())
        )]
    );
}

#[test]
fn apply_set_keybindings_drop_one_default_writes_single_disable() {
    let out = apply_set_keybindings(
        "",
        "delete_selected",
        &["f8".into()],
        Some("pane_focused".into()),
        &["f8".into(), "delete".into()],
        Some("pane_focused".into()),
    )
    .unwrap();
    let binds = parse_binds(&out);
    assert_eq!(
        binds,
        vec![("delete".into(), "-".into(), Some("pane_focused".into()))]
    );
}

#[test]
fn apply_set_keybindings_same_set_reordered_is_default() {
    // The default set in a different order is still the default — clean slate.
    let out = apply_set_keybindings(
        "",
        "delete_selected",
        &["delete".into(), "f8".into()],
        Some("pane_focused".into()),
        &["f8".into(), "delete".into()],
        Some("pane_focused".into()),
    )
    .unwrap();
    assert!(parse_binds(&out).is_empty(), "got: {}", out);
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
        &["ctrl+a".into()],
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
        &["ctrl+a".into()],
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
        &["ctrl+a".into()],
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
        &["ctrl+a".into()],
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
    let out =
        apply_reset_keybinding(initial, "navigate_back", &[], Some("pane_focused".into())).unwrap();
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

/// Every `AppPreferences` group round-trips through a settings file: a
/// group the loader doesn't know is lost on restart.
#[test]
fn loading_covers_every_group() {
    let loaded = load::parse(
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

[editor]
word_wrap = true

[viewer]
image_background = "checkerboard"
"#,
    )
    .unwrap();

    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    let merged = loaded.settings;
    assert!(!merged.appearance.folders_first);
    assert!(!merged.behavior.quick_search);
    assert!(!merged.enrichers.git_status);
    assert_eq!(merged.archives.zstd_level, 9);
    assert!(!merged.hot_paths.mounts);
    assert_eq!(merged.environment.extra_path, vec!["/opt/custom/bin"]);
    assert!(merged.editor.word_wrap);
    assert_eq!(
        merged.viewer.image_background,
        ImageBackground::Checkerboard
    );

    // Unset keys keep their defaults.
    assert_eq!(merged.behavior.history_retention, 200);
    assert_eq!(merged.archives.zip_level, 6);
}

// ---------------------------------------------------------------------------
// Bookmarks
// ---------------------------------------------------------------------------

/// `(path, name)` pairs of the `[[bookmark]]` entries, in file order.
fn parse_bookmarks(toml_str: &str) -> Vec<(String, Option<String>)> {
    let doc: toml_edit::DocumentMut = toml_str.parse().expect("valid toml");
    let arr = match doc.get("bookmark").and_then(|i| i.as_array_of_tables()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .map(|t| {
            (
                t.get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                t.get("name").and_then(|v| v.as_str()).map(String::from),
            )
        })
        .collect()
}

#[test]
fn apply_add_bookmark_prepends_new_entry() {
    let (out, was_bookmarked) = apply_add_bookmark("", "/home/u/src", Some("src")).unwrap();
    assert!(!was_bookmarked);
    assert_eq!(
        parse_bookmarks(&out),
        vec![("/home/u/src".to_string(), Some("src".to_string()))]
    );

    let (out, was_bookmarked) = apply_add_bookmark(&out, "/tmp", Some("tmp")).unwrap();
    assert!(!was_bookmarked);
    assert_eq!(
        parse_bookmarks(&out),
        vec![
            ("/tmp".to_string(), Some("tmp".to_string())),
            ("/home/u/src".to_string(), Some("src".to_string())),
        ]
    );
}

#[test]
fn apply_add_bookmark_bumps_existing_instead_of_duplicating() {
    let content = r#"
[[bookmark]]
path = "/a"
name = "a"

[[bookmark]]
path = "/b"
name = "b"

[[bookmark]]
path = "/c"
name = "c"
"#;

    let (out, was_bookmarked) = apply_add_bookmark(content, "/c", Some("c")).unwrap();
    assert!(was_bookmarked);
    assert_eq!(
        parse_bookmarks(&out),
        vec![
            ("/c".to_string(), Some("c".to_string())),
            ("/a".to_string(), Some("a".to_string())),
            ("/b".to_string(), Some("b".to_string())),
        ]
    );
}

#[test]
fn apply_add_bookmark_collapses_pre_existing_duplicates() {
    // A file written before dedup existed (or hand-edited) can hold several
    // entries for one path; re-bookmarking collapses them into one.
    let content = r#"
[[bookmark]]
path = "/dup"
name = "old"

[[bookmark]]
path = "/keep"

[[bookmark]]
path = "/dup"
name = "older"
"#;

    let (out, was_bookmarked) = apply_add_bookmark(content, "/dup", Some("new")).unwrap();
    assert!(was_bookmarked);
    assert_eq!(
        parse_bookmarks(&out),
        vec![
            ("/dup".to_string(), Some("new".to_string())),
            ("/keep".to_string(), None),
        ]
    );
}

#[test]
fn apply_add_bookmark_without_name_omits_the_key() {
    let (out, _) = apply_add_bookmark("", "/srv", None).unwrap();
    assert_eq!(parse_bookmarks(&out), vec![("/srv".to_string(), None)]);
}

#[test]
fn apply_remove_bookmark_removes_every_match() {
    let content = r#"
[[bookmark]]
path = "/dup"

[[bookmark]]
path = "/keep"

[[bookmark]]
path = "/dup"
name = "second"
"#;

    let out = apply_remove_bookmark(content, "/dup").unwrap();
    assert_eq!(parse_bookmarks(&out), vec![("/keep".to_string(), None)]);
}

#[test]
fn apply_remove_bookmark_drops_the_key_when_last_entry_goes() {
    let content = "[[bookmark]]\npath = \"/only\"\n";
    let out = apply_remove_bookmark(content, "/only").unwrap();
    assert!(parse_bookmarks(&out).is_empty());
    let doc: toml_edit::DocumentMut = out.parse().unwrap();
    assert!(!doc.contains_key("bookmark"));
}

#[test]
fn apply_restore_bookmarks_undoes_a_bump_not_just_the_add() {
    let before = r#"
[[bookmark]]
path = "/a"
name = "a"

[[bookmark]]
path = "/b"
name = "custom name for b"
"#;

    let (bumped, was_bookmarked) = apply_add_bookmark(before, "/b", Some("b")).unwrap();
    assert!(was_bookmarked);

    // Undo restores order *and* the original name, rather than deleting the
    // entry the bump displaced.
    let restored = apply_restore_bookmarks(&bumped, before).unwrap();
    assert_eq!(
        parse_bookmarks(&restored),
        vec![
            ("/a".to_string(), Some("a".to_string())),
            ("/b".to_string(), Some("custom name for b".to_string())),
        ]
    );
}

#[test]
fn apply_restore_bookmarks_of_an_add_clears_the_key() {
    let (out, _) = apply_add_bookmark("[appearance]\nshow_hidden = true\n", "/x", None).unwrap();
    let restored = apply_restore_bookmarks(&out, "[appearance]\nshow_hidden = true\n").unwrap();

    let doc: toml_edit::DocumentMut = restored.parse().unwrap();
    assert!(!doc.contains_key("bookmark"));
    assert_eq!(doc["appearance"]["show_hidden"].as_bool(), Some(true));
}

#[test]
fn apply_restore_bookmarks_keeps_unrelated_edits_made_since_the_snapshot() {
    let snapshot = "[[bookmark]]\npath = \"/a\"\n";
    let (added, _) = apply_add_bookmark(snapshot, "/b", None).unwrap();
    // Settings edited (and a keybinding added) while the bubble was up.
    let edited = format!(
        "{added}\n[appearance]\nshow_hidden = true\n\n[[bind]]\nkey = \"ctrl+g\"\ncommand = \"refresh\"\n"
    );

    let restored = apply_restore_bookmarks(&edited, snapshot).unwrap();
    assert_eq!(parse_bookmarks(&restored), vec![("/a".to_string(), None)]);

    let doc: toml_edit::DocumentMut = restored.parse().unwrap();
    assert_eq!(doc["appearance"]["show_hidden"].as_bool(), Some(true));
    assert_eq!(
        doc["bind"].as_array_of_tables().map(|a| a.len()),
        Some(1),
        "unrelated [[bind]] entry survived"
    );
}

// ---------------------------------------------------------------------------
// [[association]] edits
// ---------------------------------------------------------------------------

fn associations_in(content: &str) -> Vec<AssociationEntry> {
    toml::from_str::<SettingsFile>(content)
        .unwrap()
        .associations
}

fn set(content: &str, pattern: &str, change: AssociationChange) -> String {
    apply_set_association(content, pattern, AssociationKind::File, change).unwrap()
}

#[test]
fn a_change_creates_an_entry_for_its_pattern() {
    let out = set(
        "",
        "*.cb7",
        AssociationChange::BrowseAs {
            format: BrowseFormat::SevenZ,
        },
    );
    assert_eq!(
        out,
        "[[association]]\nmatch = \"*.cb7\"\nenter = \"browse\"\nformat = \"7z\"\n"
    );
    let dir = apply_set_association(
        "",
        "build",
        AssociationKind::Directory,
        AssociationChange::Enter {
            value: Some(crate::associations::EnterChoice::Command {
                command: "Build".into(),
            }),
        },
    )
    .unwrap();
    let entry = &associations_in(&dir)[0];
    assert_eq!(entry.kind, AssociationKind::Directory);
    assert_eq!(entry.enter, Some(EnterAction::Command));
    assert_eq!(entry.command.as_deref(), Some("Build"));
    // Resetting what isn't set writes nothing.
    assert_eq!(
        set("", "*.x", AssociationChange::Viewer { value: None }),
        ""
    );
}

#[test]
fn changes_edit_the_entry_where_it_stands() {
    let content = "[[association]]\n# mine\nmatch = \"*.LOG\"\nviewer = \"hex\"\n\n\
        [[association]]\nmatch = \"*.zip\"\nenter = \"open\"\n";
    let out = set(
        content,
        "*.log",
        AssociationChange::Language {
            value: Some("plaintext".into()),
        },
    );
    assert!(
        out.starts_with("[[association]]\n# mine\nmatch = \"*.LOG\"\nviewer = \"hex\""),
        "{out}"
    );
    assert_eq!(
        associations_in(&out)[0].language.as_deref(),
        Some("plaintext")
    );
    // Choosing an action that isn't a command drops the command.
    let out = set(
        "[[association]]\nmatch = \"*.py\"\nenter = \"command\"\ncommand = \"Run\"\n",
        "*.py",
        AssociationChange::Enter {
            value: Some(crate::associations::EnterChoice::View),
        },
    );
    assert_eq!(associations_in(&out)[0].command, None);
    assert_eq!(associations_in(&out)[0].enter, Some(EnterAction::View));
}

#[test]
fn an_entry_left_setting_nothing_goes() {
    let content = "[[association]]\nmatch = \"*.log\"\nviewer = \"hex\"\n";
    let out = set(content, "*.log", AssociationChange::Viewer { value: None });
    assert!(!out.contains("association"), "{out}");
    let out = set(content, "*.log", AssociationChange::Clear);
    assert!(!out.contains("association"), "{out}");
}

#[test]
fn clearing_a_shared_entry_takes_only_its_pattern() {
    let content =
        "[[association]]\nmatch = [\"*.nupkg\", \"*.vsix\", \"*.xpi\"]\nenter = \"browse\"\n";
    let out = set(content, "*.vsix", AssociationChange::Clear);
    assert_eq!(associations_in(&out)[0].patterns, ["*.nupkg", "*.xpi"]);
    let out = set(&out, "*.xpi", AssociationChange::Clear);
    assert!(out.contains("match = \"*.nupkg\""), "{out}");
}

#[test]
fn renaming_a_command_follows_into_associations() {
    let mut doc = "[[command]]\ntitle = \"Build\"\nrun = \"make\"\n\n\
        [[association]]\nmatch = \"build\"\nkind = \"directory\"\nenter = \"command\"\ncommand = \"Build\"\n\n\
        [[association]]\nmatch = \"*.x\"\ncommand = \"Other\"\n"
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    rename_association_command(&mut doc, "Build", "Make");
    let entries = associations_in(&doc.to_string());
    assert_eq!(entries[0].command.as_deref(), Some("Make"));
    assert_eq!(entries[1].command.as_deref(), Some("Other"));
}

// ---------------------------------------------------------------------------
// Lenient loading
// ---------------------------------------------------------------------------

fn loaded(content: &str) -> load::Loaded {
    load::parse(content).unwrap()
}

fn locations(loaded: &load::Loaded) -> Vec<(&str, Option<u32>)> {
    loaded
        .problems
        .iter()
        .map(|p| (p.location.as_str(), p.line))
        .collect()
}

#[test]
fn an_empty_file_is_the_defaults() {
    let loaded = loaded("");
    assert_eq!(loaded.settings, AppPreferences::default());
    assert!(loaded.problems.is_empty());
}

#[test]
fn settings_apply_one_by_one() {
    let loaded = loaded(
        "[appearance]\nshow_hidden = true\nfolders_first = \"no\"\n\n\
         [behavior]\nconfirm_delete = false\nhistory_retention = -3\n",
    );
    assert!(loaded.settings.appearance.show_hidden);
    assert!(!loaded.settings.behavior.confirm_delete);
    // The ones that don't read keep their defaults, and say where they are.
    assert!(loaded.settings.appearance.folders_first);
    assert_eq!(loaded.settings.behavior.history_retention, 200);
    assert_eq!(
        locations(&loaded),
        [
            ("appearance.folders_first", Some(3)),
            ("behavior.history_retention", Some(7)),
        ]
    );
}

#[test]
fn unknown_keys_are_reported() {
    let loaded = loaded(
        "profile = \"work\"\ntheme = \"dark\"\n[appearance]\nshow_hiden = true\n[apperance]\n",
    );
    assert_eq!(
        locations(&loaded),
        [
            ("profile", Some(1)),
            ("theme", Some(2)),
            ("appearance.show_hiden", Some(4)),
            ("apperance", Some(5)),
        ]
    );
    assert!(loaded.problems[0].message.contains("no longer supported"));
}

#[test]
fn a_file_that_is_not_toml_reads_as_nothing() {
    let problem = load::parse("[appearance]\nshow_hidden = \n").unwrap_err();
    assert_eq!(problem.location, "settings.toml");
    assert_eq!(problem.line, Some(2));
}

#[test]
fn entries_that_do_not_read_are_left_out() {
    let loaded = loaded(
        "[[command]]\ntitle = \"A\"\nrun = \"a\"\n\n\
         [[command]]\nrun = \"no title\"\n\n\
         [[command]]\ntitle = \"C\"\nrun = \"c\"\n\n\
         [[association]]\nmatch = \"*.log\"\nviewer = \"txt\"\n\n\
         [[association]]\nmatch = \"*.md\"\nviewer = \"text\"\n",
    );
    let titles: Vec<_> = loaded
        .file
        .commands
        .iter()
        .map(|c| c.title.as_str())
        .collect();
    assert_eq!(titles, ["A", "C"]);
    // Edits find "C" as the third [[command]], where it is in the file.
    assert_eq!(loaded.command_slots, [0, 2]);
    assert_eq!(loaded.file.associations.len(), 1);
    assert_eq!(
        locations(&loaded),
        [
            ("[[command]] #2", Some(5)),
            ("[[association]] #1", Some(12))
        ]
    );
}

#[test]
fn settings_that_cannot_work_are_reported() {
    let loaded = loaded(
        "[[bind]]\nkey = \"shift+ctrl+x\"\ncommand = \"copy\"\n\n\
         [[bind]]\nkey = \"f5\"\ncommand = \"no_such_command\"\n\n\
         [[command]]\ntitle = \"Run\"\nrun = \"x\"\napplies_to = \"files\"\n\n\
         [[association]]\nmatch = [\"*.py\", \"[\"]\nenter = \"command\"\ncommand = \"Gone\"\nlanguage = \"cobol\"\n",
    );
    let messages: Vec<_> = loaded.problems.iter().map(|p| p.message.as_str()).collect();
    assert!(messages[0].contains("never fires"), "{messages:?}");
    assert!(messages[1].contains("no_such_command"), "{messages:?}");
    assert!(messages[2].contains("applies_to"), "{messages:?}");
    assert!(messages[3].contains("not a pattern"), "{messages:?}");
    assert!(messages[4].contains("\"Gone\""), "{messages:?}");
    assert!(messages[5].contains("cobol"), "{messages:?}");
    // Reported, but kept: they read.
    assert_eq!(loaded.file.bindings.len(), 2);
    assert_eq!(loaded.file.associations.len(), 1);
}

#[test]
fn a_reload_that_is_not_toml_keeps_what_was_in_force() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, "[appearance]\nshow_hidden = true\n").unwrap();
    let mut good = PreferencesManager::load_and_resolve(dir.path(), None);
    assert!(good.settings.appearance.show_hidden);
    assert!(good.problems.is_empty());

    std::fs::write(&path, "[appearance\n").unwrap();
    let broken = PreferencesManager::load_and_resolve(dir.path(), Some(&good));
    assert!(broken.settings.appearance.show_hidden);
    assert_eq!(broken.problems.len(), 1);
    assert!(!broken.problems_dismissed);

    // Dismissed problems stay dismissed while they are the same ones.
    let mut dismissed = broken.clone();
    dismissed.problems_dismissed = true;
    let again = PreferencesManager::load_and_resolve(dir.path(), Some(&dismissed));
    assert!(again.problems_dismissed);
    std::fs::write(&path, "[appearance]\nshow_hidden = 1\n").unwrap();
    let different = PreferencesManager::load_and_resolve(dir.path(), Some(&dismissed));
    assert!(!different.problems_dismissed);

    // At startup there is nothing to keep.
    std::fs::write(&path, "[appearance\n").unwrap();
    good = PreferencesManager::load_and_resolve(dir.path(), None);
    assert_eq!(good.settings, AppPreferences::default());
}
