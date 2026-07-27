//! Generator for `THIRD-PARTY-NOTICES.md`.
//!
//! Walks the *normal* dependency edges out of the two shipped binaries
//! (`newt`, `newt-agent`) — dev- and build-only crates are not distributed
//! and are left out — plus the npm production set from `package-lock.json`,
//! and harvests each dependency's copyright notices and licence text from the
//! files it ships. The hand-written asset attributions live in
//! `notices_assets.md`.
//!
//! The licence bodies are deduplicated per SPDX id: MIT texts differ only in
//! their copyright line, so the notices carry every dependency's own
//! copyright plus one canonical copy of each permission notice.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// A dependency to be credited, from either ecosystem.
struct Dep {
    name: String,
    version: String,
    /// SPDX expression as declared by the package.
    license: String,
    /// Copyright lines harvested from the package's own licence files.
    copyrights: Vec<String>,
    /// Licence texts the package ships, for the canonical-body vote.
    texts: Vec<String>,
}

impl Dep {
    fn sort_key(&self) -> (String, String) {
        (self.name.to_lowercase(), self.version.clone())
    }
}

pub fn notices(root: &Path, check: bool) -> Result<(), String> {
    let mut rust = rust_deps(root)?;
    let mut npm = npm_deps(root)?;
    rust.sort_by_key(Dep::sort_key);
    npm.sort_by_key(Dep::sort_key);

    let bodies = canonical_bodies(rust.iter().chain(npm.iter()));

    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str(include_str!("notices_assets.md"));

    out.push_str("\n## Rust crates\n\n");
    out.push_str(&format!(
        "{} crates, the superset across every target platform and feature.\n\n",
        rust.len()
    ));
    for d in &rust {
        push_dep(&mut out, d);
    }

    out.push_str("\n## npm packages\n\n");
    out.push_str(&format!(
        "{} packages from the production dependency tree.\n\n",
        npm.len()
    ));
    for d in &npm {
        push_dep(&mut out, d);
    }

    out.push_str("\n## Licence texts\n");
    out.push_str(
        "\nOne copy of each licence named in the two lists above, as shipped \
         by the dependencies themselves. Licences that appear only as one \
         option of a multi-licence choice, or whose text no dependency ships, \
         are listed by name only.\n",
    );
    let named: BTreeSet<&str> = rust
        .iter()
        .chain(npm.iter())
        .flat_map(|d| spdx_ids(&d.license))
        .collect();
    for id in &named {
        out.push_str(&format!("\n### {id}\n\n"));
        match bodies.get(*id) {
            Some(text) => {
                out.push_str("```\n");
                out.push_str(text.trim_end());
                out.push_str("\n```\n");
            }
            None => out.push_str(&format!(
                "No dependency ships a copy of this licence. See \
                 <https://spdx.org/licenses/{id}.html>.\n"
            )),
        }
    }

    let path = root.join("THIRD-PARTY-NOTICES.md");
    std::fs::write(&path, &out).map_err(|e| format!("failed to write {}: {e}", path.display()))?;

    if check {
        let diff = Command::new("git")
            .current_dir(root)
            .args(["diff", "--exit-code", "--", "THIRD-PARTY-NOTICES.md"])
            .status()
            .map_err(|e| format!("failed to spawn git: {e}"))?;
        if !diff.success() {
            return Err(
                "THIRD-PARTY-NOTICES.md is out of date — run `cargo xtask notices` and commit"
                    .into(),
            );
        }
    }
    println!(
        "wrote {} ({} crates, {} npm packages)",
        path.display(),
        rust.len(),
        npm.len()
    );
    Ok(())
}

const HEADER: &str = "\
# Third-party notices

Newt is licensed under the GNU GPL v3.0 or later; see `LICENSE`. It is built
and distributed with the third-party components listed below, each under its
own licence. Every licence here is compatible with the GPL.

";

fn push_dep(out: &mut String, d: &Dep) {
    out.push_str(&format!("  {} {} — {}\n", d.name, d.version, d.license));
    for c in &d.copyrights {
        out.push_str(&format!("      {c}\n"));
    }
}

/// Split an SPDX expression into the individual licence ids it names.
fn spdx_ids(expr: &str) -> Vec<&str> {
    expr.split(['(', ')', '/'])
        .flat_map(str::split_whitespace)
        .filter(|t| !matches!(*t, "OR" | "AND" | "WITH"))
        // `Apache-2.0 WITH LLVM-exception` names an exception, not a licence.
        .filter(|t| !t.ends_with("-exception"))
        .collect()
}

/// Pick one canonical body per licence id: the most common text among the
/// dependencies that ship one, voted on with whitespace collapsed so that
/// differing line wrapping does not split the vote. The per-package
/// copyright notice is stripped — those are listed against each dependency,
/// and one package's notice must not appear to cover the rest.
///
/// The tally is ordered so that ties break the same way on every run: the
/// generated file has to be reproducible or the CI drift check flaps.
fn canonical_bodies<'a, I: Iterator<Item = &'a Dep>>(deps: I) -> HashMap<&'static str, String> {
    let mut votes: HashMap<&'static str, BTreeMap<String, (usize, String)>> = HashMap::new();
    for d in deps {
        for text in &d.texts {
            let Some(id) = classify(text) else { continue };
            let body = strip_leading_copyrights(text);
            let entry = votes
                .entry(id)
                .or_default()
                .entry(collapse(&body))
                .or_insert_with(|| (0, body.clone()));
            entry.0 += 1;
        }
    }
    votes
        .into_iter()
        .filter_map(|(id, texts)| {
            let best = texts.into_values().max_by_key(|(n, _)| *n)?;
            Some((id, best.1))
        })
        .collect()
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Per-package copyright notices sit in the first few lines of a licence
/// file, above the licence body. Bounding the window to that preamble is
/// what keeps the body's own prose — Apache-2.0's definition of "Licensor",
/// its `Copyright [yyyy]` appendix — from being mistaken for a notice.
const PREAMBLE_LINES: usize = 15;

/// A copyright notice, as opposed to licence prose that merely opens with
/// the word — BSD's "copyright notice, this list of conditions…" wraps onto
/// its own line. A real notice carries a holder, so demand a year or a `(c)`
/// and reject Apache-2.0's `[yyyy]` appendix placeholder.
fn is_copyright(line: &str) -> bool {
    let l = line.trim().to_lowercase();
    if !(l.starts_with("copyright") || l.starts_with('©')) || l.contains('[') || l.len() <= 12 {
        return false;
    }
    let has_year = l
        .as_bytes()
        .windows(4)
        .any(|w| w.iter().all(u8::is_ascii_digit));
    has_year || l.contains("(c)")
}

fn preamble_copyrights(text: &str) -> Vec<String> {
    text.lines()
        .take(PREAMBLE_LINES)
        .map(str::trim)
        .filter(|l| is_copyright(l))
        .map(str::to_string)
        .collect()
}

fn strip_leading_copyrights(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    let cut = lines
        .iter()
        .take(PREAMBLE_LINES)
        .rposition(|l| is_copyright(l))
        .map_or(0, |i| i + 1);
    lines.drain(..cut);
    // "All rights reserved." trails the notice, not the licence body.
    while lines.first().is_some_and(|l| {
        l.trim().is_empty() || l.trim().eq_ignore_ascii_case("all rights reserved.")
    }) {
        lines.remove(0);
    }
    lines.join("\n")
}

/// Identify a licence text by its distinctive wording. Matching runs against
/// the text with whitespace collapsed, so a clause split across lines still
/// matches. Only licences whose body is unambiguous are recognised; anything
/// else is skipped and the notices fall back to naming the licence.
fn classify(text: &str) -> Option<&'static str> {
    let t = collapse(text).to_lowercase();
    let has = |needle: &str| t.contains(needle);

    if has("apache license") && has("version 2.0") {
        return Some("Apache-2.0");
    }
    if has("mozilla public license") && has("version 2.0") {
        return Some("MPL-2.0");
    }
    if has("boost software license") {
        return Some("BSL-1.0");
    }
    if has("this is free and unencumbered software released into the public domain") {
        return Some("Unlicense");
    }
    if has("cc0 1.0 universal") || has("creative commons zero") {
        return Some("CC0-1.0");
    }
    if has("unicode license") {
        return Some("Unicode-3.0");
    }
    if has("sil open font license") {
        return Some("OFL-1.1");
    }
    if has("permission is hereby granted, free of charge") {
        return Some("MIT");
    }
    // ISC and 0BSD share this opening; ISC keeps the copyright-retention
    // clause that 0BSD drops.
    if has("permission to use, copy, modify, and/or distribute this software") {
        return if has("copyright notice and this permission notice appear in all copies") {
            Some("ISC")
        } else {
            Some("0BSD")
        };
    }
    if has("redistribution and use in source and binary forms") {
        if has("neither the name") {
            return Some("BSD-3-Clause");
        }
        return Some("BSD-2-Clause");
    }
    if has("altered source versions must be plainly marked as such") {
        return Some("Zlib");
    }
    None
}

/// Licence files a package ships, as (filename, contents).
fn license_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_uppercase();
            ["LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE"]
                .iter()
                .any(|pre| n.starts_with(pre))
        })
        .collect();
    names.sort();
    names
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

fn harvest(dir: &Path) -> (Vec<String>, Vec<String>) {
    let texts = license_files(dir);
    let mut copyrights: Vec<String> = Vec::new();
    for c in texts.iter().flat_map(|t| preamble_copyrights(t)) {
        if !copyrights.contains(&c) {
            copyrights.push(c);
        }
    }
    (copyrights, texts)
}

/// Every crate reachable from the shipped binaries over normal (non-dev,
/// non-build) dependency edges, workspace members excluded.
fn rust_deps(root: &Path) -> Result<Vec<Dep>, String> {
    let out = Command::new("cargo")
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--quiet",
        ])
        .output()
        .map_err(|e| format!("failed to spawn cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("bad cargo metadata: {e}"))?;

    let packages = meta["packages"]
        .as_array()
        .ok_or("cargo metadata: no packages")?;
    let by_id: HashMap<&str, &Value> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let members: HashSet<&str> = meta["workspace_members"]
        .as_array()
        .ok_or("cargo metadata: no workspace_members")?
        .iter()
        .filter_map(|m| m.as_str())
        .collect();

    // Normal-kind adjacency from the resolve graph.
    let mut normal: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in meta["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata: no resolve graph")?
    {
        let id = node["id"].as_str().ok_or("resolve node without id")?;
        let mut edges = Vec::new();
        for dep in node["deps"].as_array().into_iter().flatten() {
            let pkg = dep["pkg"].as_str().ok_or("resolve dep without pkg")?;
            let is_normal = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null());
            if is_normal {
                edges.push(pkg);
            }
        }
        normal.insert(id, edges);
    }

    // The two artifacts we ship. `xtask` is a build tool and is not one.
    let roots: Vec<&str> = members
        .iter()
        .copied()
        .filter(|id| {
            by_id
                .get(id)
                .and_then(|p| p["name"].as_str())
                .is_some_and(|n| n == "newt" || n == "newt-agent")
        })
        .collect();
    if roots.len() != 2 {
        return Err(format!(
            "expected to find `newt` and `newt-agent` in the workspace, found {}",
            roots.len()
        ));
    }

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack = roots;
    while let Some(id) = stack.pop() {
        for next in normal.get(id).into_iter().flatten() {
            if seen.insert(next) {
                stack.push(next);
            }
        }
    }

    let mut deps = Vec::new();
    for id in seen {
        if members.contains(id) {
            continue;
        }
        let p = by_id
            .get(id)
            .ok_or_else(|| format!("unknown package {id}"))?;
        let name = p["name"].as_str().unwrap_or_default().to_string();
        let version = p["version"].as_str().unwrap_or_default().to_string();
        let license = match (p["license"].as_str(), p["license_file"].as_str()) {
            (Some(l), _) => l.to_string(),
            (None, Some(f)) => format!("see {f}"),
            (None, None) => {
                return Err(format!("{name} {version} declares no licence"));
            }
        };
        let dir = Path::new(p["manifest_path"].as_str().unwrap_or_default())
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let (copyrights, texts) = harvest(&dir);
        deps.push(Dep {
            name,
            version,
            license,
            copyrights,
            texts,
        });
    }
    Ok(deps)
}

/// The npm production set, enumerated from `package-lock.json` rather than
/// from `node_modules`. The lockfile is committed and declares a licence for
/// every package, including the prebuilt binaries for platforms other than
/// this one — of which `npm install` only ever unpacks the host's, so a
/// listing taken off disk would differ per generating platform.
///
/// Copyright notices still have to be read from the unpacked packages, so
/// they are harvested only for packages no `os`/`cpu`/`libc` constraint can
/// exclude — the ones any host is guaranteed to have. A platform-gated
/// package missing from disk is therefore expected; a plain one missing means
/// `node_modules` is stale, and that is an error rather than a silently
/// uncredited dependency.
fn npm_deps(root: &Path) -> Result<Vec<Dep>, String> {
    let path = root.join("package-lock.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let lock: Value =
        serde_json::from_str(&raw).map_err(|e| format!("bad {}: {e}", path.display()))?;
    let packages = lock["packages"]
        .as_object()
        .ok_or("package-lock.json has no `packages` map")?;

    let mut deps = Vec::new();
    for (key, p) in packages {
        // The root project itself is keyed by the empty string.
        let Some(rel) = key.strip_prefix("node_modules/") else {
            continue;
        };
        if p["dev"].as_bool() == Some(true) || p["devOptional"].as_bool() == Some(true) {
            continue;
        }
        // Nested paths key by the full chain; the package name is the tail.
        let name = p["name"]
            .as_str()
            .unwrap_or_else(|| rel.rsplit("node_modules/").next().unwrap_or(rel))
            .to_string();
        let version = p["version"].as_str().unwrap_or_default().to_string();
        let license = p["license"]
            .as_str()
            .ok_or_else(|| format!("{name} {version} declares no licence in package-lock.json"))?
            .to_string();

        let gated = !p["os"].is_null() || !p["cpu"].is_null() || !p["libc"].is_null();
        let dir = root.join(key);
        let (copyrights, texts) = if dir.is_dir() {
            harvest(&dir)
        } else if gated {
            (Vec::new(), Vec::new())
        } else {
            return Err(format!(
                "{} is in package-lock.json but not installed — run `npm install`",
                key
            ));
        };
        deps.push(Dep {
            name,
            version,
            license,
            copyrights,
            texts,
        });
    }
    // Hoisting lists the same package under several paths.
    let mut unique: BTreeMap<(String, String), Dep> = BTreeMap::new();
    for d in deps {
        unique
            .entry((d.name.clone(), d.version.clone()))
            .or_insert(d);
    }
    Ok(unique.into_values().collect())
}
