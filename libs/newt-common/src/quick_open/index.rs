//! The walked entries and their ranking against a query.

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str, Utf32String};

use super::QuickOpenHit;

struct Entry {
    rel: String,
    haystack: Utf32String,
    /// Character index in `rel` where the name starts.
    name_start: u32,
    depth: u32,
    is_dir: bool,
}

/// How well an entry matches: a match within the name beats any match
/// that needs the path, then the alignment's score decides.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Score {
    in_path: bool,
    score: u32,
}

pub(super) struct Index {
    entries: Vec<Entry>,
    query: String,
    atoms: Vec<Atom>,
    /// The entries among the first `considered` that match `query`.
    matched: Vec<(u32, Score)>,
    considered: usize,
    matcher: Matcher,
}

impl Index {
    pub fn new() -> Self {
        Index {
            entries: Vec::new(),
            query: String::new(),
            atoms: Vec::new(),
            matched: Vec::new(),
            considered: 0,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn matched(&self) -> usize {
        self.matched.len()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Add an entry; `rel` is `/`-separated, relative to the root.
    pub fn push(&mut self, rel: String, is_dir: bool) {
        let chars = rel.chars().count() as u32;
        let name_chars = rel.rsplit('/').next().unwrap_or("").chars().count() as u32;
        let depth = rel.matches('/').count() as u32;
        self.entries.push(Entry {
            haystack: Utf32String::from(rel.as_str()),
            rel,
            name_start: chars - name_chars,
            depth,
            is_dir,
        });
    }

    /// Rank against `query` from here on. A query that only adds to the
    /// previous one can match nothing the previous one didn't, so only
    /// its matches are looked at again.
    pub fn set_query(&mut self, query: &str) {
        if query == self.query {
            return;
        }
        let narrows = query.starts_with(&self.query);
        self.query = query.to_string();
        self.atoms = query
            .split_whitespace()
            .map(|piece| {
                Atom::new(
                    piece,
                    CaseMatching::Ignore,
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                    false,
                )
            })
            .collect();
        if narrows {
            let mut matched = std::mem::take(&mut self.matched);
            matched.retain_mut(|(i, score)| match self.score(*i as usize) {
                Some(s) => {
                    *score = s;
                    true
                }
                None => false,
            });
            self.matched = matched;
        } else {
            self.matched.clear();
            self.considered = 0;
        }
    }

    /// Match the entries added since the last call.
    pub fn catch_up(&mut self) {
        for i in self.considered..self.entries.len() {
            if let Some(score) = self.score(i) {
                self.matched.push((i as u32, score));
            }
        }
        self.considered = self.entries.len();
    }

    fn score(&mut self, i: usize) -> Option<Score> {
        let entry = &self.entries[i];
        if self.atoms.is_empty() {
            return Some(Score {
                in_path: false,
                score: 0,
            });
        }
        let haystack = entry.haystack.slice(..);
        let name = haystack.slice(entry.name_start as usize..);
        if let Some(score) = sum(&self.atoms, name, &mut self.matcher) {
            return Some(Score {
                in_path: false,
                score,
            });
        }
        sum(&self.atoms, haystack, &mut self.matcher).map(|score| Score {
            in_path: true,
            score,
        })
    }

    /// The best `k` matches, best first.
    pub fn top(&mut self, k: usize) -> Vec<QuickOpenHit> {
        let entries = &self.entries;
        let order = |a: &(u32, Score), b: &(u32, Score)| {
            let (ea, eb) = (&entries[a.0 as usize], &entries[b.0 as usize]);
            a.1.in_path
                .cmp(&b.1.in_path)
                .then(b.1.score.cmp(&a.1.score))
                .then(ea.depth.cmp(&eb.depth))
                .then(ea.rel.len().cmp(&eb.rel.len()))
                .then_with(|| ea.rel.cmp(&eb.rel))
        };
        if self.matched.len() > k {
            self.matched.select_nth_unstable_by(k, order);
            self.matched[..k].sort_unstable_by(order);
        } else {
            self.matched.sort_unstable_by(order);
        }
        let Index {
            entries,
            atoms,
            matched,
            matcher,
            ..
        } = self;
        matched
            .iter()
            .take(k)
            .map(|&(i, score)| {
                let entry = &entries[i as usize];
                QuickOpenHit {
                    rel: entry.rel.clone(),
                    is_dir: entry.is_dir,
                    highlights: highlights(atoms, matcher, entry, score),
                }
            })
            .collect()
    }
}

/// The sum of every atom's score, if they all match.
fn sum(atoms: &[Atom], haystack: Utf32Str<'_>, matcher: &mut Matcher) -> Option<u32> {
    atoms.iter().try_fold(0, |total, atom| {
        atom.score(haystack, matcher)
            .map(|score| total + u32::from(score))
    })
}

/// Character indices into the entry's `rel` of what `score` matched.
fn highlights(atoms: &[Atom], matcher: &mut Matcher, entry: &Entry, score: Score) -> Vec<u32> {
    let haystack = entry.haystack.slice(..);
    let (haystack, offset) = if score.in_path {
        (haystack, 0)
    } else {
        (
            haystack.slice(entry.name_start as usize..),
            entry.name_start,
        )
    };
    let mut indices = Vec::new();
    for atom in atoms {
        atom.indices(haystack, matcher, &mut indices);
    }
    indices.sort_unstable();
    indices.dedup();
    indices.iter_mut().for_each(|i| *i += offset);
    indices
}
