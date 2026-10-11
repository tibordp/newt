//! Batch Rename: the selection's snapshot and its preview, owned by the
//! dialog's modal. Every new name is computed here; the dialog is sent
//! counts and the rows in view.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use newt_common::batch_rename::plan::{self, Problem};
use newt_common::batch_rename::rule::{CompiledRule, Rule};
use newt_common::operation::BatchRenameFolder;
use newt_common::vfs::{PathStyle, VfsPath};
use parking_lot::Mutex;

use super::pane::Pane;
use super::{MainWindowContext, ModalData, ModalDataKind};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// Rows sent at most, however tall the list.
const MAX_WINDOW: usize = 200;

/// The renames by folder, and those in the pane's own folder as (old, new).
pub type Renames = (Vec<BatchRenameFolder>, Vec<(String, String)>);

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type,
)]
#[serde(rename_all = "snake_case")]
pub enum BatchRenameFilter {
    #[default]
    All,
    Changed,
    Problems,
}

/// What the dialog shows.
#[derive(Debug, Clone, Default, serde::Serialize, specta::Type)]
pub struct BatchRenamePreview {
    pub total: u32,
    pub renamed: u32,
    pub errors: u32,
    pub warnings: u32,
    /// Why the find pattern doesn't compile.
    pub rule_error: Option<String>,
    /// The filesystem without a native rename, by name, when the batch is
    /// on one: each rename there copies and deletes.
    pub copies: Option<String>,
    pub filter: BatchRenameFilter,
    /// Rows passing the filter.
    pub shown: u32,
    /// Position of `window`'s first row among those.
    pub offset: u32,
    pub window: Vec<BatchRenameRow>,
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct BatchRenameRow {
    pub name: String,
    pub is_dir: bool,
    /// `None` when the rule leaves the name alone.
    pub new_name: Option<String>,
    /// UTF-16 ranges of the matches in `name` and of their replacements in
    /// `new_name`, for highlighting.
    pub matched: Vec<(u32, u32)>,
    pub replaced: Vec<(u32, u32)>,
    pub problem: Option<BatchRenameProblem>,
    /// The folder, when the selection spans several (search results).
    pub folder: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct BatchRenameProblem {
    pub error: bool,
    pub message: String,
}

/// The selection as it was when the dialog opened, and its preview. Lives
/// in the modal's data: closing or replacing the modal drops it.
#[derive(Clone)]
pub struct BatchRenameSession(Arc<Session>);

struct Session {
    id: u64,
    snapshot: Arc<Snapshot>,
    /// Bumped per rule; a computation for an older rule is thrown away.
    generation: AtomicU64,
    state: Mutex<State>,
}

struct Snapshot {
    rows: Vec<Row>,
    folders: Vec<Folder>,
    /// The pane, to show the renamed entries in once it's done.
    pane_dir: VfsPath,
}

struct Row {
    folder: usize,
    name: String,
    is_dir: bool,
}

struct Folder {
    dir: VfsPath,
    display: String,
    /// Its rows, in order.
    rows: Vec<usize>,
    others: Option<HashSet<String>>,
    style: PathStyle,
    exact_names: bool,
    /// The filesystem's name, when it has no native rename.
    copies: Option<String>,
}

#[derive(Default)]
struct State {
    /// The rule the outcomes are for, as given and compiled.
    given: Rule,
    rule: Option<Arc<CompiledRule>>,
    rule_error: Option<String>,
    outcomes: Vec<Outcome>,
    filter: BatchRenameFilter,
    /// Rows passing the filter.
    shown: Vec<usize>,
    viewport: (usize, usize),
}

#[derive(Clone, Default)]
struct Outcome {
    new_name: Option<String>,
    /// With its folder-local `other` index, as the planner gives it.
    problem: Option<Problem>,
}

impl BatchRenameSession {
    /// The pane's effective selection; `None` when there's nothing to
    /// rename.
    pub fn open(ctx: &MainWindowContext, pane: &Pane) -> Option<(Self, String)> {
        let (files, others) = pane.effective_selection_files();
        if files.is_empty() {
            return None;
        }
        let pane_dir = pane.path();
        let vfs_info = ctx.vfs_info().ok();
        let mut folders: Vec<Folder> = Vec::new();
        let mut by_dir: HashMap<VfsPath, usize> = HashMap::new();
        let mut rows = Vec::with_capacity(files.len());
        let mut pane_others = Some(others);
        for file in files {
            // A search hit is renamed where it is.
            let (dir, name) = match &file.source {
                Some(source) => match (source.parent(), source.path.file_name()) {
                    (Some(dir), Some(name)) => (dir, name.to_string()),
                    _ => continue,
                },
                None => (pane_dir.clone(), file.name.clone()),
            };
            let folder = *by_dir.entry(dir.clone()).or_insert_with(|| {
                let (exact_names, style, copies) = vfs_info
                    .as_ref()
                    .and_then(|vi| {
                        let (desc, meta) = vi.descriptor(dir.vfs_id)?;
                        let copies = (!desc.can_rename())
                            .then(|| vi.display_name(dir.vfs_id).unwrap_or_default());
                        Some((
                            desc.exact_names(),
                            PathStyle::from_mount_meta(&meta),
                            copies,
                        ))
                    })
                    .unwrap_or((false, PathStyle::Unix, None));
                folders.push(Folder {
                    display: ctx.format_vfs_path(&dir),
                    others: if dir == pane_dir {
                        pane_others.take()
                    } else {
                        None
                    },
                    dir,
                    rows: Vec::new(),
                    style,
                    exact_names,
                    copies,
                });
                folders.len() - 1
            });
            folders[folder].rows.push(rows.len());
            rows.push(Row {
                folder,
                name,
                is_dir: file.is_dir,
            });
        }
        if rows.is_empty() {
            return None;
        }
        let items = if rows.len() == 1 {
            "1 item".to_string()
        } else {
            format!("{} items", rows.len())
        };
        let title = match folders.as_slice() {
            [only] => format!("{items} in {}", only.display),
            _ => format!("{items} in {} folders", folders.len()),
        };
        let snapshot = Arc::new(Snapshot {
            rows,
            folders,
            pane_dir,
        });
        let outcomes = compute(&snapshot, None);
        let session = Self(Arc::new(Session {
            id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            snapshot,
            generation: AtomicU64::new(0),
            state: Mutex::new(State {
                outcomes,
                ..Default::default()
            }),
        }));
        session.refilter();
        Some((session, title))
    }

    pub fn preview(&self) -> BatchRenamePreview {
        let snapshot = &self.0.snapshot;
        let state = self.0.state.lock();
        let mut preview = BatchRenamePreview {
            total: snapshot.rows.len() as u32,
            rule_error: state.rule_error.clone(),
            copies: snapshot.folders.iter().find_map(|f| f.copies.clone()),
            filter: state.filter,
            shown: state.shown.len() as u32,
            ..Default::default()
        };
        for outcome in &state.outcomes {
            preview.renamed += u32::from(outcome.new_name.is_some());
            match &outcome.problem {
                Some(p) if p.is_error() => preview.errors += 1,
                Some(_) => preview.warnings += 1,
                None => {}
            }
        }
        let (first, count) = state.viewport;
        let first = first.min(state.shown.len());
        let end = (first + count.min(MAX_WINDOW)).min(state.shown.len());
        preview.offset = first as u32;
        let several = snapshot.folders.len() > 1;
        preview.window = state.shown[first..end]
            .iter()
            .map(|&r| {
                let row = &snapshot.rows[r];
                let outcome = &state.outcomes[r];
                let applied = outcome
                    .new_name
                    .as_ref()
                    .and(state.rule.as_ref())
                    .and_then(|rule| rule.apply(&row.name));
                let spans = |text: &str, ranges: &[std::ops::Range<usize>]| {
                    ranges
                        .iter()
                        .map(|r| (utf16_len(&text[..r.start]), utf16_len(&text[..r.end])))
                        .collect()
                };
                BatchRenameRow {
                    matched: applied
                        .as_ref()
                        .map(|a| spans(&row.name, &a.matched))
                        .unwrap_or_default(),
                    replaced: applied
                        .as_ref()
                        .map(|a| spans(&a.name, &a.replaced))
                        .unwrap_or_default(),
                    name: row.name.clone(),
                    is_dir: row.is_dir,
                    new_name: outcome.new_name.clone(),
                    problem: outcome.problem.as_ref().map(|p| BatchRenameProblem {
                        error: p.is_error(),
                        message: describe(p, &snapshot.folders[row.folder], &snapshot.rows),
                    }),
                    folder: several.then(|| snapshot.folders[row.folder].display.clone()),
                }
            })
            .collect();
        preview
    }

    /// Compute the new names under `rule` and put the preview in the
    /// modal, in the background.
    pub fn set_rule(&self, ctx: &MainWindowContext, rule: Rule) {
        let session = self.clone();
        let ctx = ctx.clone();
        tauri::async_runtime::spawn(async move { session.apply(&ctx, rule).await });
    }

    /// Compute the new names under `rule`, off the async runtime — unless a
    /// newer rule has come meanwhile — and show them.
    async fn apply(&self, ctx: &MainWindowContext, rule: Rule) {
        let generation = self.0.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (compiled, rule_error) = match CompiledRule::new(&rule) {
            Ok(compiled) => (compiled.map(Arc::new), None),
            Err(message) => (None, Some(message)),
        };
        let snapshot = self.0.snapshot.clone();
        let for_compute = compiled.clone();
        let Ok(outcomes) =
            tokio::task::spawn_blocking(move || compute(&snapshot, for_compute.as_deref())).await
        else {
            return;
        };
        if self.0.generation.load(Ordering::Relaxed) != generation {
            return;
        }
        {
            let mut state = self.0.state.lock();
            state.given = rule;
            state.rule = compiled;
            state.rule_error = rule_error;
            state.outcomes = outcomes;
        }
        self.refilter();
        self.show(ctx);
    }

    /// The renames under `rule`, computing them first if the preview is
    /// still on another one; `None` while something blocks them. The
    /// second part is those in the pane's own folder.
    pub async fn request(&self, ctx: &MainWindowContext, rule: Rule) -> Option<Renames> {
        if self.0.state.lock().given != rule {
            self.apply(ctx, rule.clone()).await;
        }
        if self.0.state.lock().given != rule {
            return None;
        }
        self.renames()
    }

    pub fn set_viewport(&self, ctx: &MainWindowContext, first: usize, count: usize) {
        self.0.state.lock().viewport = (first, count);
        self.show(ctx);
    }

    pub fn set_filter(&self, ctx: &MainWindowContext, filter: BatchRenameFilter) {
        self.0.state.lock().filter = filter;
        self.refilter();
        self.show(ctx);
    }

    /// The next row with a problem after (or before) position `from` among
    /// the shown rows, wrapping around.
    pub fn find_problem(&self, from: Option<u32>, forward: bool) -> Option<u32> {
        let state = self.0.state.lock();
        let n = state.shown.len();
        if n == 0 {
            return None;
        }
        let has = |pos: usize| state.outcomes[state.shown[pos]].problem.is_some();
        let start = from.map_or(if forward { n - 1 } else { 0 }, |f| f as usize % n);
        (1..=n)
            .map(|step| {
                if forward {
                    (start + step) % n
                } else {
                    (start + n - step % n) % n
                }
            })
            .find(|&pos| has(pos))
            .map(|pos| pos as u32)
    }

    fn renames(&self) -> Option<Renames> {
        let snapshot = &self.0.snapshot;
        let state = self.0.state.lock();
        if state.rule_error.is_some()
            || state
                .outcomes
                .iter()
                .any(|o| o.problem.as_ref().is_some_and(Problem::is_error))
        {
            return None;
        }
        let mut in_pane = Vec::new();
        let folders: Vec<BatchRenameFolder> = snapshot
            .folders
            .iter()
            .filter_map(|folder| {
                let renames: Vec<(String, String)> = folder
                    .rows
                    .iter()
                    .filter_map(|&r| {
                        let new = state.outcomes[r].new_name.clone()?;
                        Some((snapshot.rows[r].name.clone(), new))
                    })
                    .collect();
                if folder.dir == snapshot.pane_dir {
                    in_pane.extend(renames.iter().cloned());
                }
                (!renames.is_empty()).then(|| BatchRenameFolder {
                    dir: folder.dir.clone(),
                    renames,
                })
            })
            .collect();
        (!folders.is_empty()).then_some((folders, in_pane))
    }

    fn refilter(&self) {
        let mut state = self.0.state.lock();
        let shown = (0..self.0.snapshot.rows.len())
            .filter(|&r| match state.filter {
                BatchRenameFilter::All => true,
                BatchRenameFilter::Changed => state.outcomes[r].new_name.is_some(),
                BatchRenameFilter::Problems => state.outcomes[r].problem.is_some(),
            })
            .collect();
        state.shown = shown;
    }

    /// Put the preview in the modal, if the modal is still this session's.
    fn show(&self, ctx: &MainWindowContext) {
        let preview = self.preview();
        let _ = ctx.with_update(|gs| {
            if let Some(ModalData {
                kind:
                    ModalDataKind::BatchRename {
                        session,
                        preview: shown,
                        ..
                    },
                ..
            }) = gs.modal.0.write().as_mut()
                && session.0.id == self.0.id
            {
                *shown = preview;
            }
            Ok(())
        });
    }
}

/// New names and their problems for every row.
fn compute(snapshot: &Snapshot, rule: Option<&CompiledRule>) -> Vec<Outcome> {
    let mut outcomes: Vec<Outcome> = snapshot
        .rows
        .iter()
        .map(|row| Outcome {
            new_name: rule
                .and_then(|rule| rule.apply(&row.name))
                .map(|applied| applied.name)
                .filter(|name| *name != row.name),
            problem: None,
        })
        .collect();
    for folder in &snapshot.folders {
        let entries: Vec<(String, String)> = folder
            .rows
            .iter()
            .map(|&r| {
                let name = &snapshot.rows[r].name;
                let new = outcomes[r].new_name.as_ref().unwrap_or(name);
                (name.clone(), new.clone())
            })
            .collect();
        let problems = plan::check(&plan::Folder {
            entries: &entries,
            others: folder.others.as_ref(),
            style: folder.style,
            exact_names: folder.exact_names,
        });
        for (k, problem) in problems.into_iter().enumerate() {
            outcomes[folder.rows[k]].problem = problem;
        }
    }
    outcomes
}

fn describe(problem: &Problem, folder: &Folder, rows: &[Row]) -> String {
    let name_of = |other: usize| rows[folder.rows[other]].name.as_str();
    match problem {
        Problem::Empty => "The new name is empty".into(),
        Problem::DotName => "“.” and “..” aren't names".into(),
        Problem::Separator => "The new name has a path separator".into(),
        Problem::InvalidOnWindows(why) => format!("The new name {why}"),
        Problem::Duplicate { other } => format!("{} gets the same name", name_of(*other)),
        Problem::Replaces => "Replaces an existing entry".into(),
        Problem::MayReplace => "May replace an existing entry where case is ignored".into(),
        Problem::MayCollide { other } => {
            format!(
                "{} gets the same name where case is ignored",
                name_of(*other)
            )
        }
    }
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use newt_common::vfs::VfsId;
    use newt_common::vfs::path::PathBuf;

    /// Two folders, the second's rows after the first's: messages must
    /// name the other entry of the *same* folder.
    fn session() -> BatchRenameSession {
        let dir = |p: &str| VfsPath::new(VfsId::ROOT, PathBuf::from_wire_str(p));
        let names = ["a1", "b1", "a2", "x", "y2", "z2"];
        let rows = names
            .iter()
            .enumerate()
            .map(|(i, name)| Row {
                folder: usize::from(i >= 3),
                name: name.to_string(),
                is_dir: false,
            })
            .collect();
        let folder = |path: &str, rows: Vec<usize>| Folder {
            dir: dir(path),
            display: path.to_string(),
            rows,
            others: None,
            style: PathStyle::Unix,
            exact_names: true,
            copies: None,
        };
        let snapshot = Arc::new(Snapshot {
            rows,
            folders: vec![folder("/one", vec![0, 1, 2]), folder("/two", vec![3, 4, 5])],
            pane_dir: dir("/one"),
        });
        let rule = CompiledRule::new(&Rule {
            find: r"\d".into(),
            replace: String::new(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
        let outcomes = compute(&snapshot, Some(&rule));
        let session = BatchRenameSession(Arc::new(Session {
            id: 1,
            snapshot,
            generation: AtomicU64::new(0),
            state: Mutex::new(State {
                rule: Some(Arc::new(rule)),
                outcomes,
                viewport: (0, 100),
                ..Default::default()
            }),
        }));
        session.refilter();
        session
    }

    #[test]
    fn problems_name_the_other_entry_of_their_folder() {
        let preview = session().preview();
        let messages: Vec<Option<String>> = preview
            .window
            .iter()
            .map(|row| row.problem.as_ref().map(|p| p.message.clone()))
            .collect();
        assert_eq!(
            messages,
            [
                Some("a2 gets the same name".to_string()),
                None,
                Some("a1 gets the same name".to_string()),
                None,
                None,
                None,
            ]
        );
        assert_eq!((preview.renamed, preview.errors), (5, 2));
        assert_eq!(preview.window[0].folder.as_deref(), Some("/one"));
    }

    #[test]
    fn problems_are_found_both_ways_around() {
        let session = session();
        assert_eq!(session.find_problem(None, true), Some(0));
        assert_eq!(session.find_problem(Some(0), true), Some(2));
        assert_eq!(session.find_problem(Some(2), true), Some(0));
        assert_eq!(session.find_problem(Some(0), false), Some(2));
        assert!(session.renames().is_none(), "errors block the rename");
    }
}
