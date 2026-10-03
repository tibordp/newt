//! Reading `settings.toml` leniently. A value or entry that can't be read
//! is left out — its default applies — and reported as a `ConfigProblem`
//! instead of costing the rest of the file; so are settings that read but
//! can't work (a binding to no command, a glob that doesn't compile). Only
//! a file that isn't TOML at all reads as nothing.

use serde::de::DeserializeOwned;

use super::schema::{AppPreferences, EnterAction, SettingsFile};

/// Something in `settings.toml` that was ignored or won't work.
#[derive(Debug, Clone, PartialEq, serde::Serialize, specta::Type)]
pub struct ConfigProblem {
    /// A setting (`behavior.history_retention`), an entry
    /// (`[[association]] #3`), or the file.
    pub location: String,
    /// 1-based line in the file, when known.
    pub line: Option<u32>,
    /// What's wrong there, as a phrase that follows the location.
    pub message: String,
}

/// What reading the file left: its settings, where each user command came
/// from, and what was wrong.
#[derive(Debug, Default)]
pub struct Loaded {
    pub file: SettingsFile,
    /// The defaults with every setting of the file that reads applied.
    pub settings: AppPreferences,
    /// The `[[command]]` each of `file.commands` is in the file — they
    /// differ once an entry before it was left out.
    pub command_slots: Vec<usize>,
    pub problems: Vec<ConfigProblem>,
}

const ARRAYS: [&str; 4] = ["bind", "bookmark", "command", "association"];

fn line_of(content: &str, offset: usize) -> u32 {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count() as u32
        + 1
}

/// Read `content`; `Err` with the syntax error when it isn't TOML.
pub fn parse(content: &str) -> Result<Loaded, ConfigProblem> {
    let syntax = |message: String, span: Option<std::ops::Range<usize>>| ConfigProblem {
        location: "settings.toml".into(),
        line: span.map(|s| line_of(content, s.start)),
        message,
    };
    let doc = toml_edit::Document::parse(content)
        .map_err(|e| syntax(e.message().to_string(), e.span()))?;
    let root: toml::Table =
        toml::from_str(content).map_err(|e| syntax(e.message().to_string(), e.span()))?;

    let line = |span: Option<std::ops::Range<usize>>| span.map(|s| line_of(content, s.start));
    let key_line = |key: &str| line(doc.as_table().key(key).and_then(|k| k.span()));
    let entry_line = |array: &str, index: usize| {
        let item = doc.get(array)?;
        let span = match item.as_array_of_tables() {
            Some(tables) => tables.get(index)?.span(),
            None => item.as_array()?.get(index)?.span(),
        };
        line(span)
    };

    let mut loaded = Loaded::default();
    let sections: Vec<&str> = loaded
        .file
        .sections()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    for (key, value) in root {
        if sections.contains(&key.as_str()) {
            if value.is_table() {
                *loaded.file.section_mut(&key) = value;
            } else {
                loaded.problems.push(ConfigProblem {
                    location: key.clone(),
                    line: key_line(&key),
                    message: "not a table".into(),
                });
            }
            continue;
        }
        if ARRAYS.contains(&key.as_str()) {
            let toml::Value::Array(entries) = value else {
                loaded.problems.push(ConfigProblem {
                    location: format!("[[{key}]]"),
                    line: key_line(&key),
                    message: "not a list of tables".into(),
                });
                continue;
            };
            for (index, entry) in entries.into_iter().enumerate() {
                let problem = |message: String| ConfigProblem {
                    location: format!("[[{key}]] #{}", index + 1),
                    line: entry_line(&key, index),
                    message,
                };
                let result = match key.as_str() {
                    "bind" => read(entry).map(|e| loaded.file.bindings.push(e)),
                    "bookmark" => read(entry).map(|e| loaded.file.bookmarks.push(e)),
                    "association" => read(entry).map(|e| loaded.file.associations.push(e)),
                    "command" => read(entry).map(|e| {
                        loaded.file.commands.push(e);
                        loaded.command_slots.push(index);
                    }),
                    _ => unreachable!(),
                };
                if let Err(message) = result {
                    loaded.problems.push(problem(message));
                }
            }
            continue;
        }
        loaded.problems.push(ConfigProblem {
            location: key.clone(),
            line: key_line(&key),
            message: if key == "profile" {
                "settings profiles are no longer supported; this does nothing".into()
            } else {
                "not a setting".into()
            },
        });
    }

    let setting_line = |section: &str, key: &str| {
        let table = doc.get(section)?.as_table()?;
        line(table.key(key)?.span())
    };
    let (settings, problems) = merge(&loaded.file, setting_line);
    loaded.settings = settings;
    loaded.problems.extend(problems);
    loaded.problems.extend(check(&loaded, &doc, content));
    loaded.problems.sort_by_key(|p| p.line.unwrap_or(u32::MAX));
    Ok(loaded)
}

fn read<T: DeserializeOwned>(value: toml::Value) -> Result<T, String> {
    value
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_string())
}

/// The defaults with each of `file`'s settings applied that reads as its
/// type, one at a time; the rest are reported and keep their defaults.
fn merge(
    file: &SettingsFile,
    line: impl Fn(&str, &str) -> Option<u32>,
) -> (AppPreferences, Vec<ConfigProblem>) {
    let mut problems = Vec::new();
    let mut merged =
        toml::Value::try_from(AppPreferences::default()).expect("the defaults serialize");
    for (section, value) in file.sections() {
        let toml::Value::Table(table) = value else {
            continue;
        };
        for (key, value) in table {
            let problem = |message: String| ConfigProblem {
                location: format!("{section}.{key}"),
                line: line(section, key),
                message,
            };
            if merged.get(section).and_then(|s| s.get(key)).is_none() {
                problems.push(problem("not a setting".into()));
                continue;
            }
            let mut candidate = merged.clone();
            super::deep_merge_table(&mut candidate[section][key.as_str()], value);
            match candidate.clone().try_into::<AppPreferences>() {
                Ok(_) => merged = candidate,
                Err(e) => problems.push(problem(e.message().to_string())),
            }
        }
    }
    let settings = merged
        .try_into()
        .expect("only settings that deserialize were applied");
    (settings, problems)
}

/// Settings that read but can't work.
fn check(loaded: &Loaded, doc: &toml_edit::Document<&str>, content: &str) -> Vec<ConfigProblem> {
    let mut problems = Vec::new();
    let entry_line = |array: &str, index: usize| {
        let tables = doc.get(array)?.as_array_of_tables()?;
        Some(line_of(content, tables.get(index)?.span()?.start))
    };
    let commands: Vec<String> = super::commands::default_commands()
        .into_iter()
        .map(|c| c.id)
        .collect();

    for (index, bind) in loaded.file.bindings.iter().enumerate() {
        let location = format!("[[bind]] #{}", index + 1);
        let line = entry_line("bind", index);
        if bind.command != "-" && !commands.contains(&bind.command) {
            problems.push(ConfigProblem {
                location: location.clone(),
                line,
                message: format!("there is no command `{}`", bind.command),
            });
        }
        if let Some(message) = key_problem(&bind.key) {
            problems.push(ConfigProblem {
                location,
                line,
                message,
            });
        }
    }

    for (slot, command) in loaded.command_slots.iter().zip(&loaded.file.commands) {
        let location = format!("[[command]] #{}", slot + 1);
        let line = entry_line("command", *slot);
        if let Some(message) = command.key.as_deref().and_then(key_problem) {
            problems.push(ConfigProblem {
                location: location.clone(),
                line,
                message,
            });
        }
        if let Some(applies) = &command.applies_to
            && !["file", "directory", "selection"].contains(&applies.as_str())
        {
            problems.push(ConfigProblem {
                location,
                line,
                message: format!(
                    "applies_to `{applies}` is none of \"file\", \"directory\", \"selection\""
                ),
            });
        }
    }

    let titles: Vec<&str> = loaded
        .file
        .commands
        .iter()
        .map(|c| c.title.as_str())
        .collect();
    for (index, association) in loaded.file.associations.iter().enumerate() {
        let location = format!("[[association]] #{}", index + 1);
        let line = entry_line("association", index);
        let mut report = |message: String| {
            problems.push(ConfigProblem {
                location: location.clone(),
                line,
                message,
            })
        };
        for pattern in &association.patterns {
            if let Err(e) = globset::Glob::new(pattern) {
                report(format!("`{pattern}` is not a pattern: {}", e.kind()));
            }
        }
        if association.enter == Some(EnterAction::Command) {
            match &association.command {
                None => report("enter = \"command\" needs a `command`".into()),
                Some(title) if !titles.contains(&title.as_str()) => {
                    report(format!("there is no user command titled \"{title}\""))
                }
                Some(_) => {}
            }
        }
        if let Some(language) = &association.language
            && !crate::associations::LANGUAGES
                .iter()
                .any(|(id, _)| id == language)
        {
            report(format!("the editor has no language `{language}`"));
        }
    }
    problems
}

/// Why a key binding can never fire, if it can't: bindings match keys
/// spelled with their modifiers in one order.
fn key_problem(key: &str) -> Option<String> {
    const ORDER: [&str; 4] = ["meta", "ctrl", "shift", "alt"];
    let parts: Vec<&str> = key.split('+').collect();
    let (last, modifiers) = parts.split_last()?;
    if last.is_empty() || ORDER.contains(last) || *last == "mod" {
        return Some(format!("`{key}` has no key besides its modifiers"));
    }
    let mut previous = None;
    for modifier in modifiers {
        let canonical = match *modifier {
            "mod" if cfg!(target_os = "macos") => "meta",
            "mod" => "ctrl",
            other => other,
        };
        let Some(position) = ORDER.iter().position(|m| *m == canonical) else {
            return Some(format!(
                "`{modifier}` in `{key}` is not a modifier (meta, ctrl, shift, alt or mod)"
            ));
        };
        if previous.is_some_and(|p| p >= position) {
            return Some(format!(
                "`{key}` never fires: modifiers go in the order meta, ctrl, shift, alt"
            ));
        }
        previous = Some(position);
    }
    None
}
