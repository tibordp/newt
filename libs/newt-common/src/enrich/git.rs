//! Git status enricher.
//!
//! Shells out to the `git` binary on the machine that owns the files
//! (the agent in remote sessions) — no libgit2/gitoxide dependency, and
//! environments without git simply don't get git-aware listings. A
//! `.git` walk-up guards the spawn so non-repo directories never pay
//! for one.
//!
//! One `git status --porcelain=v2 -z --branch` run at the repo root per
//! enrichment yields the branch badge (name, ahead/behind, dirty) and
//! repo-wide per-file statuses, which makes directory rollups free: an
//! entry that is a directory takes the highest-precedence status of
//! anything beneath it. `--ignored=matching` folds ignored entries into
//! the same pass (fully-ignored directories arrive collapsed).

use std::path::{Path as StdPath, PathBuf as StdPathBuf};
use std::process::Stdio;

use tokio::process::Command;

use super::{
    Annotation, ContextBadge, EnrichScope, EnrichSink, Enricher, EnricherDescriptor,
    GitEntryStatus, RegisteredEnricher,
};
use crate::Error;
use crate::proc::NoConsoleWindow;
use crate::shell::resolve_program;
use crate::vfs::{VfsDescriptor, VfsPath, VfsRegistry, local::to_native};

pub struct GitEnricherDescriptor;

pub static GIT_ENRICHER_DESCRIPTOR: GitEnricherDescriptor = GitEnricherDescriptor;
inventory::submit! { RegisteredEnricher(&GIT_ENRICHER_DESCRIPTOR) }

impl EnricherDescriptor for GitEnricherDescriptor {
    fn id(&self) -> &'static str {
        "git"
    }

    fn activity(&self) -> &'static str {
        "git status"
    }

    fn automatic(&self) -> bool {
        true
    }

    fn applies_to_vfs(&self, vfs: &dyn VfsDescriptor) -> bool {
        vfs.type_name() == "local"
    }
}

pub struct GitEnricher {
    extra_path: Vec<String>,
}

impl GitEnricher {
    pub fn new(extra_path: Vec<String>) -> Self {
        Self { extra_path }
    }
}

/// Walk up from `dir` looking for a `.git` entry (a directory in an
/// ordinary repo, a file in worktrees/submodules).
async fn find_repo_root(dir: &StdPath) -> Option<StdPathBuf> {
    for ancestor in dir.ancestors() {
        if tokio::fs::try_exists(ancestor.join(".git"))
            .await
            .unwrap_or(false)
        {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

/// The listed directory as a repo-root-relative `/`-separated prefix
/// ("" when the listed directory is the root itself). Git reports all
/// paths in this form regardless of host OS.
fn repo_relative_prefix(root: &StdPath, dir: &StdPath) -> Option<String> {
    let rel = dir.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

#[async_trait::async_trait]
impl Enricher for GitEnricher {
    fn descriptor(&self) -> &'static dyn EnricherDescriptor {
        &GIT_ENRICHER_DESCRIPTOR
    }

    async fn enrich(
        &self,
        _registry: &VfsRegistry,
        path: &VfsPath,
        _scope: &EnrichScope,
        sink: &EnrichSink,
    ) -> Result<(), Error> {
        let dir = to_native(&path.path);
        // Not inside a repo: nothing to emit — the sink's final empty
        // reset batch still clears any previous generation (e.g. the
        // repo's .git vanished between visits).
        let Some(root) = find_repo_root(&dir).await else {
            return Ok(());
        };
        let Some(prefix) = repo_relative_prefix(&root, &dir) else {
            return Ok(());
        };

        let mut cmd = Command::new(resolve_program("git", &self.extra_path));
        cmd.arg("-C")
            .arg(&root)
            // Never take the index lock or refresh the index: a status
            // run must not mutate .git and re-trigger the pane watcher.
            .arg("--no-optional-locks")
            .args([
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--untracked-files=normal",
                "--ignored=matching",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Cancellation is by drop — take git down with the future.
            .kill_on_drop(true);
        let out = cmd.no_console_window().output().await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(Error::custom(format!(
                "git status exited with {:?}: {}",
                out.status.code(),
                stderr
            )));
        }

        let status = parse_porcelain_v2(&out.stdout);

        for (entry, entry_status) in dir_statuses(&status.entries, &prefix) {
            sink.emit_entry(entry, Annotation::Git(entry_status));
        }

        let dirty = status
            .entries
            .iter()
            .any(|(_, s)| *s != GitEntryStatus::Ignored);
        let (name, detached) = match status.branch_head {
            Some(head) if head != "(detached)" => (head, false),
            _ => {
                // Detached (or initial with no head line): show a short
                // commit id when we have one.
                let oid = status.branch_oid.unwrap_or_default();
                (oid.chars().take(8).collect(), true)
            }
        };
        sink.emit_badge(ContextBadge::GitBranch {
            name,
            detached,
            ahead: status.ahead,
            behind: status.behind,
            dirty,
        });

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Porcelain v2 parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
pub(super) struct RepoStatus {
    pub branch_oid: Option<String>,
    pub branch_head: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    /// Repo-root-relative `/`-separated paths (directories from
    /// collapsed untracked/ignored entries keep a trailing `/`).
    pub entries: Vec<(String, GitEntryStatus)>,
}

/// Ordinary changed entry: map the XY field (index + worktree) to a
/// display status. Anything not more specific (M/D/T/C in either
/// column) renders as Modified.
fn xy_status(xy: &str) -> GitEntryStatus {
    if xy.contains('R') {
        GitEntryStatus::Renamed
    } else if xy.contains('A') {
        GitEntryStatus::Added
    } else {
        GitEntryStatus::Modified
    }
}

/// Parse `git status --porcelain=v2 -z --branch` output. NUL-separated
/// records; rename records (`2 ...`) are followed by an extra
/// NUL-separated token holding the original path, which we skip.
pub(super) fn parse_porcelain_v2(bytes: &[u8]) -> RepoStatus {
    let mut status = RepoStatus::default();
    let mut records = bytes
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .map(String::from_utf8_lossy);

    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            if let Some(oid) = header.strip_prefix("branch.oid ") {
                status.branch_oid = Some(oid.to_string());
            } else if let Some(head) = header.strip_prefix("branch.head ") {
                status.branch_head = Some(head.to_string());
            } else if let Some(ab) = header.strip_prefix("branch.ab ") {
                for part in ab.split(' ') {
                    if let Some(n) = part.strip_prefix('+') {
                        status.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix('-') {
                        status.behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }

        let entry = match record.chars().next() {
            Some('1') => record
                .splitn(9, ' ')
                .nth(8)
                .map(|path| (path.to_string(), xy_status(&record[2..4]))),
            Some('2') => {
                // The next NUL token is the rename's original path.
                let entry = record
                    .splitn(10, ' ')
                    .nth(9)
                    .map(|path| (path.to_string(), xy_status(&record[2..4])));
                let _ = records.next();
                entry
            }
            Some('u') => record
                .splitn(11, ' ')
                .nth(10)
                .map(|path| (path.to_string(), GitEntryStatus::Conflicted)),
            Some('?') => record
                .strip_prefix("? ")
                .map(|path| (path.to_string(), GitEntryStatus::Untracked)),
            Some('!') => record
                .strip_prefix("! ")
                .map(|path| (path.to_string(), GitEntryStatus::Ignored)),
            _ => None,
        };
        if let Some(entry) = entry {
            status.entries.push(entry);
        }
    }

    status
}

/// Fold repo-wide status entries into statuses for the direct children
/// of the directory identified by `prefix` (repo-root-relative,
/// `/`-separated, "" for the root itself).
///
/// A direct child takes its own status; anything deeper rolls up to the
/// child directory it lives under, keeping the highest-precedence
/// status (the `GitEntryStatus` variant order). `Ignored` never rolls
/// up — a directory isn't "ignored-ish" for containing ignored files;
/// fully-ignored directories arrive as collapsed direct entries.
pub(super) fn dir_statuses(
    entries: &[(String, GitEntryStatus)],
    prefix: &str,
) -> std::collections::HashMap<String, GitEntryStatus> {
    let mut result: std::collections::HashMap<String, GitEntryStatus> = Default::default();
    for (path, status) in entries {
        let path = path.strip_suffix('/').unwrap_or(path);
        let rel = if prefix.is_empty() {
            path
        } else {
            match path
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('/'))
            {
                Some(rel) => rel,
                None => continue,
            }
        };
        let (entry, direct) = match rel.split_once('/') {
            Some((first, _)) => (first, false),
            None => (rel, true),
        };
        if entry.is_empty() || (!direct && *status == GitEntryStatus::Ignored) {
            continue;
        }
        result
            .entry(entry.to_string())
            .and_modify(|existing| *existing = (*existing).max(*status))
            .or_insert(*status);
    }
    result
}
