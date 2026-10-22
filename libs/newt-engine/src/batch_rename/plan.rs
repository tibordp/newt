//! What is wrong with a folder's new names, and the order to rename them
//! in. Pure: the preview runs it on the host, the operation again next to
//! the filesystem.

use std::collections::{HashMap, HashSet};

use crate::vfs::PathStyle;

use super::fold;

/// One folder of a batch.
pub struct Folder<'a> {
    /// Every selected entry: its name and its new name, the same when the
    /// rule leaves it alone.
    pub entries: &'a [(String, String)],
    /// The names in the folder that aren't selected, when known.
    pub others: Option<&'a HashSet<String>>,
    pub style: PathStyle,
    /// Names are one entry exactly when their bytes are equal
    /// (`VfsDescriptor::exact_names`); otherwise they're compared under
    /// [`fold`] wherever "might be the same" is what matters.
    pub exact_names: bool,
}

impl Folder<'_> {
    fn changed(&self, i: usize) -> bool {
        self.entries[i].0 != self.entries[i].1
    }

    fn key(&self, name: &str) -> String {
        if self.exact_names {
            name.to_string()
        } else {
            fold(name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    Empty,
    /// `.` or `..`.
    DotName,
    /// A separator or NUL: a path, not a name.
    Separator,
    /// Not a name Windows accepts.
    InvalidOnWindows(&'static str),
    /// Another entry of the folder ends up with the same bytes.
    Duplicate {
        other: usize,
    },
    /// An entry outside the selection has the new name.
    Replaces,
    /// An entry outside the selection has the new name, give or take case
    /// or Unicode normalization.
    MayReplace,
    /// Another entry ends up with this name, give or take case or Unicode
    /// normalization.
    MayCollide {
        other: usize,
    },
}

impl Problem {
    /// Errors block the rename; the rest are warnings.
    pub fn is_error(&self) -> bool {
        !matches!(
            self,
            Self::Replaces | Self::MayReplace | Self::MayCollide { .. }
        )
    }
}

/// The first problem of each entry, errors ahead of warnings; `None` for
/// entries without one, and always for those the rule leaves alone.
pub fn check(folder: &Folder) -> Vec<Option<Problem>> {
    let entries = folder.entries;
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (_, new)) in entries.iter().enumerate() {
        by_name.entry(new).or_default().push(i);
    }
    // New names and other names under the fold, where names might fold.
    struct Folded {
        by_key: HashMap<String, Vec<usize>>,
        others: HashSet<String>,
    }
    let folded = (!folder.exact_names).then(|| {
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (_, new)) in entries.iter().enumerate() {
            by_key.entry(fold(new)).or_default().push(i);
        }
        let others = folder
            .others
            .map(|others| others.iter().map(|n| fold(n)).collect())
            .unwrap_or_default();
        Folded { by_key, others }
    });

    (0..entries.len())
        .map(|i| {
            if !folder.changed(i) {
                return None;
            }
            let new = entries[i].1.as_str();
            if let Some(problem) = invalid(new, folder.style) {
                return Some(problem);
            }
            if let Some(&other) = by_name[new].iter().find(|&&j| j != i) {
                return Some(Problem::Duplicate { other });
            }
            if folder.others.is_some_and(|others| others.contains(new)) {
                return Some(Problem::Replaces);
            }
            let Folded { by_key, others } = folded.as_ref()?;
            let key = fold(new);
            if let Some(&other) = by_key[&key].iter().find(|&&j| j != i) {
                return Some(Problem::MayCollide { other });
            }
            others.contains(&key).then_some(Problem::MayReplace)
        })
        .collect()
}

const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn invalid(name: &str, style: PathStyle) -> Option<Problem> {
    if name.is_empty() {
        return Some(Problem::Empty);
    }
    if name == "." || name == ".." {
        return Some(Problem::DotName);
    }
    if name.contains('/') || name.contains('\0') {
        return Some(Problem::Separator);
    }
    if style != PathStyle::Windows {
        return None;
    }
    if name.contains('\\') {
        return Some(Problem::Separator);
    }
    if name
        .chars()
        .any(|c| c.is_ascii_control() || "<>:\"|?*".contains(c))
    {
        return Some(Problem::InvalidOnWindows(
            "has a character Windows doesn't allow",
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(Problem::InvalidOnWindows("ends with a dot or a space"));
    }
    let base = name.split('.').next().unwrap_or(name).trim_end();
    if WINDOWS_RESERVED
        .iter()
        .any(|r| r.eq_ignore_ascii_case(base))
    {
        return Some(Problem::InvalidOnWindows("is a name Windows reserves"));
    }
    None
}

/// One step of a folder's renames, by entry index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Rename(usize),
    /// Each member takes the next one's name, the last the first's.
    Cycle(Vec<usize>),
    /// To a temporary name, out of the way of the steps up to its
    /// `Unpark`, then on to its new name.
    Park(usize),
    Unpark(usize),
}

/// The order to rename a folder's changed entries in: each one's new name
/// is vacated before it's taken, as far as the names tell. Over-careful
/// where names might fold: an ordering a case-sensitive filesystem didn't
/// need costs nothing, a cycle it didn't have costs a swap.
pub fn order(folder: &Folder) -> Vec<Step> {
    let nodes: Vec<usize> = (0..folder.entries.len())
        .filter(|&i| folder.changed(i))
        .collect();
    let mut by_old: HashMap<String, Vec<usize>> = HashMap::new();
    for (n, &i) in nodes.iter().enumerate() {
        by_old
            .entry(folder.key(&folder.entries[i].0))
            .or_default()
            .push(n);
    }
    // `u → v`: `v` must be out of the way before `u` can take its name.
    let edges: Vec<Vec<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(u, &i)| {
            by_old
                .get(&folder.key(&folder.entries[i].1))
                .map(|vs| vs.iter().copied().filter(|&v| v != u).collect())
                .unwrap_or_default()
        })
        .collect();
    let mut steps = Vec::new();
    let all: Vec<usize> = (0..nodes.len()).collect();
    order_within(&all, &edges, &mut steps);
    for step in &mut steps {
        match step {
            Step::Rename(n) | Step::Park(n) | Step::Unpark(n) => *n = nodes[*n],
            Step::Cycle(members) => {
                for n in members {
                    *n = nodes[*n];
                }
            }
        }
    }
    steps
}

/// Steps for the subgraph on `members`, dependencies first.
fn order_within(members: &[usize], edges: &[Vec<usize>], steps: &mut Vec<Step>) {
    let inside: HashSet<usize> = members.iter().copied().collect();
    for component in components(members, edges, &inside) {
        if component.len() == 1 {
            steps.push(Step::Rename(component[0]));
            continue;
        }
        let set: HashSet<usize> = component.iter().copied().collect();
        let succ = |u: usize| edges[u].iter().copied().filter(|v| set.contains(v));
        let simple = component.iter().all(|&u| succ(u).count() == 1) && {
            let mut indegree: HashMap<usize, usize> = HashMap::new();
            for &u in &component {
                for v in succ(u) {
                    *indegree.entry(v).or_default() += 1;
                }
            }
            indegree.values().all(|&d| d == 1)
        };
        if simple {
            let mut cycle = vec![component[0]];
            let mut next = succ(component[0]).next().expect("one successor");
            while next != component[0] {
                cycle.push(next);
                next = succ(next).next().expect("one successor");
            }
            steps.push(Step::Cycle(cycle));
        } else {
            // Several names folding together: take one member out of the
            // way and order the rest again.
            let parked = component[0];
            steps.push(Step::Park(parked));
            order_within(&component[1..], edges, steps);
            steps.push(Step::Unpark(parked));
        }
    }
}

/// Tarjan's strongly connected components of the subgraph on `members`,
/// each emitted after every component it has edges to — the order to run
/// them in. Iterative: a chain of 100k renames is 100k deep.
fn components(members: &[usize], edges: &[Vec<usize>], inside: &HashSet<usize>) -> Vec<Vec<usize>> {
    let mut index: HashMap<usize, usize> = HashMap::new();
    let mut low: HashMap<usize, usize> = HashMap::new();
    let mut on_stack: HashSet<usize> = HashSet::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    let mut next_index = 0;

    for &root in members {
        if index.contains_key(&root) {
            continue;
        }
        // (node, position in its edge list)
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index.insert(root, next_index);
        low.insert(root, next_index);
        next_index += 1;
        stack.push(root);
        on_stack.insert(root);

        while let Some(&mut (u, ref mut pos)) = work.last_mut() {
            let targets = &edges[u];
            if *pos < targets.len() {
                let v = targets[*pos];
                *pos += 1;
                if !inside.contains(&v) {
                    continue;
                }
                if let std::collections::hash_map::Entry::Vacant(slot) = index.entry(v) {
                    slot.insert(next_index);
                    low.insert(v, next_index);
                    next_index += 1;
                    stack.push(v);
                    on_stack.insert(v);
                    work.push((v, 0));
                } else if on_stack.contains(&v) {
                    let lv = index[&v];
                    let lu = low.get_mut(&u).expect("visited");
                    *lu = (*lu).min(lv);
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                let lu = low[&u];
                let lp = low.get_mut(&parent).expect("visited");
                *lp = (*lp).min(lu);
            }
            if low[&u] == index[&u] {
                let mut component = Vec::new();
                loop {
                    let w = stack.pop().expect("on the stack");
                    on_stack.remove(&w);
                    component.push(w);
                    if w == u {
                        break;
                    }
                }
                component.reverse();
                out.push(component);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn folder<'a>(
        entries: &'a [(String, String)],
        others: Option<&'a HashSet<String>>,
    ) -> Folder<'a> {
        Folder {
            entries,
            others,
            style: PathStyle::Unix,
            exact_names: false,
        }
    }

    /// The renames in the order the steps perform them, by old name; a
    /// cycle as its members in brackets, parking as `^name` / `vname`.
    fn steps(pairs: &[(&str, &str)], exact_names: bool) -> Vec<String> {
        let entries = entries(pairs);
        let folder = Folder {
            exact_names,
            ..folder(&entries, None)
        };
        let name = |i: usize| entries[i].0.clone();
        order(&folder)
            .into_iter()
            .map(|step| match step {
                Step::Rename(i) => name(i),
                Step::Cycle(m) => format!(
                    "[{}]",
                    m.into_iter().map(name).collect::<Vec<_>>().join(" ")
                ),
                Step::Park(i) => format!("^{}", name(i)),
                Step::Unpark(i) => format!("v{}", name(i)),
            })
            .collect()
    }

    #[test]
    fn a_chain_vacates_each_name_before_it_is_taken() {
        assert_eq!(
            steps(&[("1", "2"), ("2", "3"), ("3", "4")], false),
            ["3", "2", "1"]
        );
    }

    #[test]
    fn swaps_and_rotations_are_cycles() {
        assert_eq!(steps(&[("a", "b"), ("b", "a")], false), ["[a b]"]);
        assert_eq!(
            steps(&[("a", "b"), ("b", "c"), ("c", "a")], false),
            ["[a b c]"]
        );
    }

    #[test]
    fn names_that_might_fold_are_ordered_carefully_unless_names_are_exact() {
        // On APFS, `Notes` is `notes`, and `A` is `a`: a swap.
        assert_eq!(
            steps(&[("a", "Notes"), ("notes", "A")], false),
            ["[a notes]"]
        );
        // On S3 they are different names: no cycle, no order needed.
        let exact = steps(&[("a", "Notes"), ("notes", "A")], true);
        assert_eq!(exact.len(), 2);
        assert!(exact.iter().all(|s| !s.starts_with('[')));
    }

    #[test]
    fn a_case_only_rename_depends_on_nothing() {
        assert_eq!(steps(&[("readme", "README")], false), ["readme"]);
    }

    #[test]
    fn several_names_folding_together_park_one() {
        // All three present on Linux, rotating; folded, each wants both
        // other names, which no single cycle describes.
        let order = steps(
            &[
                ("README", "readme"),
                ("readme", "Readme"),
                ("Readme", "README"),
            ],
            false,
        );
        assert!(order.first().unwrap().starts_with('^'), "{order:?}");
        assert!(order.last().unwrap().starts_with('v'), "{order:?}");
        // One that only moves out of the way goes first.
        assert_eq!(
            steps(
                &[("README", "readme"), ("readme", "README"), ("Readme", "x")],
                false
            ),
            ["Readme", "[README readme]"]
        );
    }

    #[test]
    fn a_long_chain_does_not_overflow() {
        let pairs: Vec<(String, String)> = (0..200_000)
            .map(|i| (i.to_string(), (i + 1).to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let order = steps(&refs, true);
        assert_eq!(order.first().map(String::as_str), Some("199999"));
        assert_eq!(order.len(), 200_000);
    }

    #[test]
    fn unchanged_entries_take_no_step() {
        assert_eq!(steps(&[("a", "a"), ("b", "c")], false), ["b"]);
    }

    #[test]
    fn checks() {
        let others: HashSet<String> = ["taken".to_string(), "Other".to_string()].into();
        let entries = entries(&[
            ("a", ""),
            ("b", ".."),
            ("c", "x/y"),
            ("d", "same"),
            ("e", "same"),
            ("f", "taken"),
            ("g", "other"),
            ("h", "Twin"),
            ("i", "twin"),
            ("j", "j"),
            ("k", "fine"),
            ("l", "j"),
        ]);
        let problems = check(&folder(&entries, Some(&others)));
        assert_eq!(
            problems,
            [
                Some(Problem::Empty),
                Some(Problem::DotName),
                Some(Problem::Separator),
                Some(Problem::Duplicate { other: 4 }),
                Some(Problem::Duplicate { other: 3 }),
                Some(Problem::Replaces),
                Some(Problem::MayReplace),
                Some(Problem::MayCollide { other: 8 }),
                Some(Problem::MayCollide { other: 7 }),
                None,
                None,
                // Onto a selected entry the rule leaves alone.
                Some(Problem::Duplicate { other: 9 }),
            ]
        );
    }

    #[test]
    fn exact_names_have_no_maybes() {
        let others: HashSet<String> = ["Other".to_string()].into();
        let entries = entries(&[("g", "other"), ("h", "Twin"), ("i", "twin")]);
        let folder = Folder {
            exact_names: true,
            ..folder(&entries, Some(&others))
        };
        assert_eq!(check(&folder), [None, None, None]);
    }

    #[test]
    fn windows_names() {
        let check_one = |name: &str| invalid(name, PathStyle::Windows);
        assert!(check_one("a\\b").is_some());
        assert!(check_one("what?").is_some());
        assert!(check_one("trailing.").is_some());
        assert!(check_one("trailing ").is_some());
        assert!(check_one("nul.txt").is_some());
        assert!(check_one("Com1").is_some());
        assert!(check_one("console.log").is_none());
        assert!(invalid("what?", PathStyle::Unix).is_none());
        assert!(invalid("a\\b", PathStyle::Unix).is_none());
    }
}
