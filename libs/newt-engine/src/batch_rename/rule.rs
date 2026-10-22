//! The find & replace rule: a Rust regex, and a replacement in Monaco's
//! (VS Code's) syntax rather than `regex`'s, since that is what people type
//! in the editor next door.

use std::ops::Range;

use regex::{Captures, Regex, RegexBuilder};

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct Rule {
    pub find: String,
    pub replace: String,
    pub case_sensitive: bool,
    /// Give each replacement the case shape of the text it replaces.
    pub keep_case: bool,
}

/// A name with the rule applied, and where the replacements went.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub name: String,
    /// Byte ranges of the matches in the old name.
    pub matched: Vec<Range<usize>>,
    /// Byte ranges of the replacements in the new name.
    pub replaced: Vec<Range<usize>>,
}

pub struct CompiledRule {
    regex: Regex,
    replacement: Vec<Piece>,
    keep_case: bool,
}

impl CompiledRule {
    /// `None` for an empty find, which matches nothing — not the empty
    /// string at every position.
    pub fn new(rule: &Rule) -> Result<Option<Self>, String> {
        if rule.find.is_empty() {
            return Ok(None);
        }
        let regex = RegexBuilder::new(&rule.find)
            .case_insensitive(!rule.case_sensitive)
            .build()
            .map_err(|e| match e {
                regex::Error::Syntax(message) => message,
                e => e.to_string(),
            })?;
        // A name the pattern doesn't have is literal text, as in JavaScript.
        let replacement = parse_replacement(&rule.replace)
            .into_iter()
            .map(|piece| match piece {
                Piece::Named(name, _) if !regex.capture_names().any(|n| n == Some(&name)) => {
                    Piece::Text(format!("$<{name}>"))
                }
                piece => piece,
            })
            .collect();
        Ok(Some(Self {
            regex,
            replacement,
            keep_case: rule.keep_case,
        }))
    }

    /// `name` with every match replaced, or `None` when nothing matches.
    pub fn apply(&self, name: &str) -> Option<Applied> {
        let mut out = String::with_capacity(name.len());
        let mut matched = Vec::new();
        let mut replaced = Vec::new();
        let mut last = 0;
        for caps in self.regex.captures_iter(name) {
            let whole = caps.get(0).expect("group 0 always participates");
            out.push_str(&name[last..whole.start()]);
            let mut replacement = expand(&self.replacement, &caps);
            if self.keep_case {
                replacement = with_case_of(whole.as_str(), &replacement);
            }
            let start = out.len();
            out.push_str(&replacement);
            matched.push(whole.range());
            replaced.push(start..out.len());
            last = whole.end();
        }
        if matched.is_empty() {
            return None;
        }
        out.push_str(&name[last..]);
        Some(Applied {
            name: out,
            matched,
            replaced,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    /// A group by number, with the case operations (`\u \l \U \L`) written
    /// before it.
    Group(usize, Vec<char>),
    Named(String, Vec<char>),
}

/// Monaco's `parseReplaceString`: `$&` and `$0` the whole match, `$1`…`$99`
/// a group, `$$` a dollar; `\n`, `\t`, `\\`; `\u \l \U \L` change the case
/// of the next group reference only. Plus `$<name>`, as JavaScript has it.
/// Anything else is literal.
fn parse_replacement(replace: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut case_ops = Vec::new();
    let mut chars = replace.chars().peekable();
    let flush = |text: &mut String, pieces: &mut Vec<Piece>| {
        if !text.is_empty() {
            pieces.push(Piece::Text(std::mem::take(text)));
        }
    };
    while let Some(c) = chars.next() {
        match (c, chars.peek().copied()) {
            ('\\', Some(next)) => {
                chars.next();
                match next {
                    'n' => text.push('\n'),
                    't' => text.push('\t'),
                    '\\' => text.push('\\'),
                    'u' | 'U' | 'l' | 'L' => case_ops.push(next),
                    _ => {
                        text.push('\\');
                        text.push(next);
                    }
                }
            }
            ('$', Some('$')) => {
                chars.next();
                text.push('$');
            }
            ('$', Some('&')) => {
                chars.next();
                flush(&mut text, &mut pieces);
                pieces.push(Piece::Group(0, std::mem::take(&mut case_ops)));
            }
            ('$', Some(d)) if d.is_ascii_digit() => {
                chars.next();
                let mut index = d.to_digit(10).expect("a digit") as usize;
                if let Some(d2) = chars.peek().and_then(|c| c.to_digit(10)) {
                    chars.next();
                    index = index * 10 + d2 as usize;
                }
                flush(&mut text, &mut pieces);
                pieces.push(Piece::Group(index, std::mem::take(&mut case_ops)));
            }
            ('$', Some('<')) => {
                let rest: String = chars.clone().skip(1).collect();
                match rest.find('>') {
                    Some(end) if end > 0 => {
                        let name = rest[..end].to_string();
                        for _ in 0..name.chars().count() + 2 {
                            chars.next();
                        }
                        flush(&mut text, &mut pieces);
                        pieces.push(Piece::Named(name, std::mem::take(&mut case_ops)));
                    }
                    _ => text.push('$'),
                }
            }
            _ => text.push(c),
        }
    }
    flush(&mut text, &mut pieces);
    pieces
}

fn expand(pieces: &[Piece], caps: &Captures) -> String {
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(text) => out.push_str(text),
            Piece::Group(index, ops) => {
                out.push_str(&with_case_ops(&substitute(*index, caps), ops))
            }
            Piece::Named(name, ops) => {
                let group = caps.name(name).map_or("", |m| m.as_str());
                out.push_str(&with_case_ops(group, ops));
            }
        }
    }
    out
}

/// Monaco's `_substitute`: `$12` with fewer than 12 groups is `$1`
/// followed by `2`, and a reference past every group is literal.
fn substitute(index: usize, caps: &Captures) -> String {
    if index == 0 {
        return caps[0].to_string();
    }
    let mut index = index;
    let mut remainder = String::new();
    while index > 0 {
        if index < caps.len() {
            let group = caps.get(index).map_or("", |m| m.as_str());
            return format!("{group}{remainder}");
        }
        remainder.insert_str(0, &(index % 10).to_string());
        index /= 10;
    }
    format!("${remainder}")
}

/// Monaco's case operations on a group's text: `u`/`l` change one
/// character and move on to the next operation, `U`/`L` change every
/// character from there on.
fn with_case_ops(text: &str, ops: &[char]) -> String {
    if ops.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut op = 0;
    for (i, c) in text.char_indices() {
        let Some(&current) = ops.get(op) else {
            out.push_str(&text[i..]);
            break;
        };
        match current {
            'U' => out.extend(c.to_uppercase()),
            'L' => out.extend(c.to_lowercase()),
            'u' => {
                out.extend(c.to_uppercase());
                op += 1;
            }
            'l' => {
                out.extend(c.to_lowercase());
                op += 1;
            }
            _ => out.push(c),
        }
    }
    out
}

/// VS Code's `buildReplaceStringWithCasePreserved`: `replacement` in the
/// case shape of `matched` — all upper, all lower, capitalized, or part by
/// part when both are split alike by `-` or `_`.
fn with_case_of(matched: &str, replacement: &str) -> String {
    if matched.is_empty() {
        return replacement.to_string();
    }
    let hyphens = split_alike(matched, replacement, '-');
    let underscores = split_alike(matched, replacement, '_');
    if hyphens && !underscores {
        return by_parts(matched, replacement, '-');
    }
    if underscores && !hyphens {
        return by_parts(matched, replacement, '_');
    }
    let first = matched.chars().next().expect("not empty");
    if matched.to_uppercase() == matched {
        replacement.to_uppercase()
    } else if matched.to_lowercase() == matched {
        replacement.to_lowercase()
    } else if first.is_uppercase() {
        with_first(replacement, |c| c.to_uppercase().collect())
    } else if !first.is_uppercase() && first.to_uppercase().next() != Some(first) {
        with_first(replacement, |c| c.to_lowercase().collect())
    } else {
        replacement.to_string()
    }
}

fn split_alike(matched: &str, replacement: &str, separator: char) -> bool {
    matched.contains(separator)
        && replacement.contains(separator)
        && matched.split(separator).count() == replacement.split(separator).count()
}

fn by_parts(matched: &str, replacement: &str, separator: char) -> String {
    matched
        .split(separator)
        .zip(replacement.split(separator))
        .map(|(m, r)| with_case_of(m, r))
        .collect::<Vec<_>>()
        .join(&separator.to_string())
}

fn with_first(text: &str, change: impl Fn(char) -> String) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => change(first) + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(find: &str, replace: &str, name: &str) -> Option<String> {
        rule(find, replace, false, false, name)
    }

    fn rule(
        find: &str,
        replace: &str,
        case_sensitive: bool,
        keep_case: bool,
        name: &str,
    ) -> Option<String> {
        CompiledRule::new(&Rule {
            find: find.into(),
            replace: replace.into(),
            case_sensitive,
            keep_case,
        })
        .unwrap()
        .expect("a rule")
        .apply(name)
        .map(|a| a.name)
    }

    #[test]
    fn every_match_is_replaced_and_no_match_is_none() {
        assert_eq!(apply("o", "0", "foo.log").as_deref(), Some("f00.l0g"));
        assert_eq!(apply("x", "y", "foo.log"), None);
        assert_eq!(
            apply("^", "2026-", "notes.md").as_deref(),
            Some("2026-notes.md")
        );
    }

    #[test]
    fn an_empty_find_is_no_rule() {
        let compiled = CompiledRule::new(&Rule::default()).unwrap();
        assert!(compiled.is_none());
    }

    #[test]
    fn case_sensitivity() {
        assert_eq!(
            apply("img", "photo", "IMG_1.jpg").as_deref(),
            Some("photo_1.jpg")
        );
        assert_eq!(rule("img", "photo", true, false, "IMG_1.jpg"), None);
    }

    #[test]
    fn groups_whole_match_and_dollars() {
        assert_eq!(
            apply(r"IMG_(\d+)", "trip-$1", "IMG_0042.JPG").as_deref(),
            Some("trip-0042.JPG")
        );
        assert_eq!(apply(r"\d+", "[$&]", "a1b22").as_deref(), Some("a[1]b[22]"));
        assert_eq!(apply(r"\d+", "<$0>", "a1").as_deref(), Some("a<1>"));
        assert_eq!(apply("a", "$$", "a").as_deref(), Some("$"));
        assert_eq!(apply("a", "$x", "a").as_deref(), Some("$x"));
    }

    #[test]
    fn group_numbers_past_the_groups_follow_monaco() {
        // One group: `$12` is group 1 then a literal 2; `$3` is literal.
        assert_eq!(apply("(a)", "$12", "a").as_deref(), Some("a2"));
        assert_eq!(apply("(a)", "$3", "a").as_deref(), Some("$3"));
    }

    #[test]
    fn named_groups() {
        assert_eq!(
            apply(r"(?<year>\d{4})-(?<n>\d+)", "$<n>-$<year>", "2026-7.txt").as_deref(),
            Some("7-2026.txt")
        );
        assert_eq!(apply("a", "$<nope>", "a").as_deref(), Some("$<nope>"));
    }

    #[test]
    fn escapes_and_case_operations() {
        assert_eq!(apply("a", r"\\", "a").as_deref(), Some("\\"));
        assert_eq!(apply("a", r"\q", "a").as_deref(), Some("\\q"));
        assert_eq!(apply("(foo)", r"\U$1", "foo").as_deref(), Some("FOO"));
        assert_eq!(apply("(foo)", r"\u$1", "foo").as_deref(), Some("Foo"));
        assert_eq!(apply("(FOO)", r"\l$1", "FOO").as_deref(), Some("fOO"));
        assert_eq!(apply("(FOO)", r"\u\L$1", "FOO").as_deref(), Some("Foo"));
        // The operations apply to the group, not to text written after them.
        assert_eq!(apply("(foo)", r"\Ubar$1", "foo").as_deref(), Some("barFOO"));
    }

    #[test]
    fn keep_case_follows_vs_code() {
        let keep = |matched: &str, replacement: &str| with_case_of(matched, replacement);
        assert_eq!(keep("abc", "Def"), "def");
        assert_eq!(keep("Abc", "Def"), "Def");
        assert_eq!(keep("ABC", "Def"), "DEF");
        assert_eq!(keep("abc", "DEF"), "def");
        assert_eq!(keep("aBc", "Def"), "def");
        assert_eq!(keep("Foo-Bar", "newfoo-newbar"), "Newfoo-Newbar");
        assert_eq!(
            keep("Foo-Bar-Abc", "newfoo-newbar-newabc"),
            "Newfoo-Newbar-Newabc"
        );
        assert_eq!(keep("Foo-Bar-abc", "newfoo-newbar"), "Newfoo-newbar");
        assert_eq!(keep("Foo_Bar", "newfoo_newbar"), "Newfoo_Newbar");
        assert_eq!(
            keep("Foo_Bar-abc", "newfoo_newbar-abc"),
            "Newfoo_newbar-abc"
        );

        assert_eq!(
            rule("report", "summary", false, true, "REPORT.txt").as_deref(),
            Some("SUMMARY.txt")
        );
        assert_eq!(
            rule("report", "summary", false, true, "Report.txt").as_deref(),
            Some("Summary.txt")
        );
    }

    #[test]
    fn spans_point_at_the_matches_and_their_replacements() {
        let compiled = CompiledRule::new(&Rule {
            find: r"\d+".into(),
            replace: "#".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
        let applied = compiled.apply("a12b3").unwrap();
        assert_eq!(applied.name, "a#b#");
        assert_eq!(applied.matched, [1..3, 4..5]);
        assert_eq!(applied.replaced, [1..2, 3..4]);
    }
}
