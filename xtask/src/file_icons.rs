//! Generator for the file-list icon set: `src/assets/seti.woff` and
//! `src/assets/file-icons.json`, from VS Code's `theme-seti` at a pinned
//! release.
//!
//! The theme associates icons three ways: by whole file name, by extension,
//! and by language id. Language ids mean nothing outside VS Code, so they are
//! resolved here through the `contributes.languages` of VS Code's built-in
//! extensions and folded into the name and extension tables. VS Code ranks
//! the theme's own name and extension rules above language rules, so those
//! win wherever both apply. Everything is keyed in lowercase, as VS Code
//! matches it.
//!
//! The font and the mapping must come from the same release: the font's
//! codepoints shift between releases.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;

use serde_json::{Map, Value, json};

const VSCODE_REPO: &str = "https://github.com/microsoft/vscode.git";
const VSCODE_TAG: &str = "1.142.0";
const THEME_DIR: &str = "extensions/theme-seti/icons";

/// seti-ui's palette, by the dark-theme value VS Code's theme carries. The
/// frontend resolves the names through per-theme `--icon-<name>` tokens.
const PALETTE: &[(&str, &str)] = &[
    ("#519aba", "blue"),
    ("#cc3e44", "red"),
    ("#a074c4", "purple"),
    ("#cbcb41", "yellow"),
    ("#8dc149", "green"),
    ("#4d5a5e", "grey"),
    ("#e37933", "orange"),
    ("#6d8086", "grey-light"),
    ("#d4d7d6", "white"),
    ("#f55385", "pink"),
    ("#41535b", "ignore"),
];

pub fn file_icons(root: &Path) -> Result<(), String> {
    let checkout = root.join("target").join("xtask-file-icons");
    fetch(&checkout)?;

    let theme = read_json(&checkout.join(THEME_DIR).join("vs-seti-icon-theme.json"))?;
    let languages = languages(&checkout)?;
    let mapping = mapping(&theme, &languages)?;

    let assets = root.join("src").join("assets");
    let font = assets.join("seti.woff");
    std::fs::copy(checkout.join(THEME_DIR).join("seti.woff"), &font)
        .map_err(|e| format!("copy {}: {e}", font.display()))?;
    let out = assets.join("file-icons.json");
    let text = serde_json::to_string_pretty(&mapping).unwrap() + "\n";
    std::fs::write(&out, text).map_err(|e| format!("write {}: {e}", out.display()))?;

    println!(
        "wrote {} and {} (VS Code {VSCODE_TAG})",
        font.display(),
        out.display()
    );
    Ok(())
}

/// A shallow, blobless, sparse checkout of just the theme and the extension
/// manifests — a few megabytes instead of the whole tree.
fn fetch(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| format!("rm -rf {}: {e}", dir.display()))?;
    }
    println!("→ fetching VS Code {VSCODE_TAG}");
    git(
        None,
        &[
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--branch",
            VSCODE_TAG,
            "--filter=blob:none",
            "--no-checkout",
            VSCODE_REPO,
            &dir.to_string_lossy(),
        ],
    )?;
    git(
        Some(dir),
        &[
            "sparse-checkout",
            "set",
            "--no-cone",
            "/extensions/*/package.json",
            &format!("/{THEME_DIR}/"),
        ],
    )?;
    git(Some(dir), &["checkout", "--quiet"])
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<(), String> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let status = command
        .args(args)
        .status()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if !status.success() {
        return Err(format!("git {} failed", args[0]));
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Extensions and file names each language id claims, across every built-in
/// extension that contributes to it.
#[derive(Default)]
struct Language {
    extensions: Vec<String>,
    filenames: Vec<String>,
}

fn languages(checkout: &Path) -> Result<HashMap<String, Language>, String> {
    let dir = checkout.join("extensions");
    let mut manifests: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("package.json"))
        .filter(|p| p.exists())
        .collect();
    manifests.sort();

    let mut out: HashMap<String, Language> = HashMap::new();
    for manifest in manifests {
        let manifest = read_json(&manifest)?;
        let Some(contributed) = manifest["contributes"]["languages"].as_array() else {
            continue;
        };
        for language in contributed {
            let entry = out
                .entry(language["id"].as_str().unwrap().to_owned())
                .or_default();
            for ext in strings(&language["extensions"]) {
                entry
                    .extensions
                    .push(ext.strip_prefix('.').unwrap_or(ext).to_lowercase());
            }
            for name in strings(&language["filenames"]) {
                entry.filenames.push(name.to_lowercase());
            }
        }
    }
    Ok(out)
}

fn strings(v: &Value) -> impl Iterator<Item = &str> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str)
}

fn lowercase_keys(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.to_lowercase(), icon_id(v.as_str().unwrap()).to_owned()))
        .collect()
}

/// `_typescript_1` → `typescript_1`.
fn icon_id(definition: &str) -> &str {
    definition.strip_prefix('_').unwrap_or(definition)
}

/// The icon the extension table gives a name, trying each dot-suffix from
/// longest to shortest — the lookup `src/lib/fileIcons.ts` performs.
fn extension_icon<'a>(extensions: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    name.match_indices('.')
        .find_map(|(dot, _)| extensions.get(&name[dot + 1..]))
        .map(String::as_str)
}

fn mapping(theme: &Value, languages: &HashMap<String, Language>) -> Result<Value, String> {
    let mut names = lowercase_keys(&theme["fileNames"]);
    let mut extensions = lowercase_keys(&theme["fileExtensions"]);
    let theme_extensions = extensions.clone();

    let mut by_language: Vec<(&String, &Value)> =
        theme["languageIds"].as_object().unwrap().iter().collect();
    by_language.sort_by_key(|(id, _)| *id);
    for (id, icon) in by_language {
        let icon = icon_id(icon.as_str().unwrap());
        let Some(language) = languages.get(id) else {
            // Languages VS Code doesn't ship (Kotlin, Vue, …) come from
            // marketplace extensions; the theme maps their extensions itself.
            continue;
        };
        for ext in &language.extensions {
            extensions
                .entry(ext.clone())
                .or_insert_with(|| icon.to_owned());
        }
        for name in &language.filenames {
            if extension_icon(&theme_extensions, name).is_none() {
                names.entry(name.clone()).or_insert_with(|| icon.to_owned());
            }
        }
    }

    let palette: HashMap<&str, &str> = PALETTE.iter().copied().collect();
    let mut icons = Map::new();
    for (key, definition) in theme["iconDefinitions"].as_object().unwrap() {
        if key.ends_with("_light") {
            continue;
        }
        let char = definition["fontCharacter"]
            .as_str()
            .unwrap()
            .trim_start_matches('\\');
        let color = match definition["fontColor"].as_str() {
            Some(hex) => Value::from(
                *palette
                    .get(hex.to_lowercase().as_str())
                    .ok_or_else(|| format!("{key}: colour {hex} is not in the seti palette"))?,
            ),
            None => Value::Null,
        };
        icons.insert(
            icon_id(key).to_owned(),
            json!({ "char": char, "color": color }),
        );
    }

    for icon in names.values().chain(extensions.values()) {
        if !icons.contains_key(icon) {
            return Err(format!("mapping refers to undefined icon {icon}"));
        }
    }

    Ok(json!({
        "file": icon_id(theme["file"].as_str().unwrap()),
        "fileNames": names,
        "fileExtensions": extensions,
        "icons": icons,
    }))
}
