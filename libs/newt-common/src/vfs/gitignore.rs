//! `.gitignore` rules for a walk: those of the directories above its root
//! up to the top of the repository, then each directory's own as the walk
//! lists it.

use std::collections::HashMap;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::vfs::path::{Path, PathBuf};
use crate::vfs::{File, Vfs};

/// Larger ignore files are not read.
const MAX_IGNORE_FILE: u64 = 1 << 20;

#[derive(Default)]
pub struct GitIgnores {
    /// Rules by the directory they're in, by wire path.
    rules: HashMap<String, Gitignore>,
}

impl GitIgnores {
    /// The rules of the directories above `root` up to the top of the
    /// repository it's in, if it's in one.
    pub async fn above(vfs: &dyn Vfs, root: &Path) -> Self {
        let ancestors: Vec<&Path> = std::iter::successors(root.parent(), |p| p.parent()).collect();
        let tops = futures::future::join_all(
            ancestors
                .iter()
                .map(|dir| async move { vfs.file_info(&dir.join(".git")).await.is_ok() }),
        )
        .await;
        let mut rules = HashMap::new();
        let Some(top) = tops.iter().position(|&is_top| is_top) else {
            return Self { rules };
        };
        for (i, dir) in ancestors[..=top].iter().enumerate() {
            let mut files = vec![dir.join(".gitignore")];
            if i == top {
                files.push(dir.join(".git/info/exclude"));
            }
            if let Some(found) = read(vfs, &files).await {
                rules.insert(dir.as_wire_str().to_string(), found);
            }
        }
        Self { rules }
    }

    /// Take in the rules of a directory the walk has just listed.
    pub async fn learn(&mut self, vfs: &dyn Vfs, dir: &Path, children: &[File]) {
        let has = |name: &str| children.iter().any(|f| f.name == name);
        let mut files = Vec::new();
        if has(".gitignore") {
            files.push(dir.join(".gitignore"));
        }
        if has(".git") {
            files.push(dir.join(".git/info/exclude"));
        }
        if let Some(found) = read(vfs, &files).await {
            self.rules.insert(dir.as_wire_str().to_string(), found);
        }
    }

    /// Whether the nearest rule about `path` among its directories' says
    /// to ignore it.
    pub fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if let Some(rules) = self.rules.get(d.as_wire_str()) {
                let below = path.strip_prefix(d).unwrap_or_default();
                let found = rules.matched(below, is_dir);
                if found.is_ignore() {
                    return true;
                }
                if found.is_whitelist() {
                    return false;
                }
            }
            dir = d.parent();
        }
        false
    }
}

/// The rules in `files`, which are all relative to the same directory;
/// `None` when there are none.
async fn read(vfs: &dyn Vfs, files: &[PathBuf]) -> Option<Gitignore> {
    if files.is_empty() {
        return None;
    }
    let mut builder = GitignoreBuilder::new("");
    let mut any = false;
    for file in files {
        let Ok(chunk) = vfs.read_range(file, 0, MAX_IGNORE_FILE).await else {
            continue;
        };
        for line in String::from_utf8_lossy(&chunk.data).lines() {
            if builder.add_line(None, line).is_ok() {
                any = true;
            }
        }
    }
    any.then(|| builder.build().ok()).flatten()
}
