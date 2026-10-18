//! File associations: what Enter does with an entry, what Browse Into
//! browses a file as, and which mode and language the viewer and editor
//! give it. Of the entries whose patterns match a name, the most specific
//! pattern goes first — `*.tar.gz` before `*.gz` — and the user's before
//! the shipped defaults when they are as specific. Each property then
//! resolves on its own, from the first of those entries that sets it, so
//! an entry setting only `viewer` leaves Enter alone; the command an Enter
//! runs comes from the entry that chose it. Viewer modes and languages no
//! entry sets come from the file's MIME type.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use log::warn;

use crate::preferences::schema::{AssociationEntry, AssociationKind, BrowseFormat, EnterAction};
use crate::viewer::{ViewerMode, webview_formats};

pub struct Associations {
    /// The settings file's entries, then the shipped ones.
    entries: Vec<AssociationEntry>,
    user_len: usize,
    globs: GlobSet,
    /// The entry each of `globs`' patterns belongs to, and how specific
    /// the pattern is.
    owners: Vec<(usize, usize)>,
    /// Each of `globs`' patterns as written.
    glob_patterns: Vec<String>,
    /// Enter opens packages rather than browsing into them.
    open_packages: bool,
}

/// What Enter does, as an entry sets it: an action, and for `command` the
/// command it runs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum EnterChoice {
    Open,
    Browse,
    View,
    Edit,
    Command { command: String },
}

impl EnterChoice {
    fn of(entry: &AssociationEntry) -> Option<Self> {
        Some(match entry.enter? {
            EnterAction::Open => EnterChoice::Open,
            EnterAction::Browse => EnterChoice::Browse,
            EnterAction::View => EnterChoice::View,
            EnterAction::Edit => EnterChoice::Edit,
            EnterAction::Command => match &entry.command {
                Some(command) => EnterChoice::Command {
                    command: command.clone(),
                },
                None => {
                    warn!("association: `enter = \"command\"` without a `command`");
                    return None;
                }
            },
        })
    }
}

/// Each property that resolved for a name, with the entry it came from.
#[derive(Debug, Default, Clone)]
pub struct Traced {
    pub enter: Option<(EnterChoice, usize)>,
    pub format: Option<(BrowseFormat, usize)>,
    pub viewer: Option<(ViewerMode, usize)>,
    pub language: Option<(String, usize)>,
    /// Some entry matched — for a directory, that the user declares it a
    /// package.
    pub matched: bool,
}

/// What Enter does with an entry, after the defaults and what the entry's
/// filesystem allows.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Into the directory.
    Navigate,
    /// The system's default application.
    Open,
    /// Into the file as a filesystem; `None` when no association names a
    /// format, and the file's first bytes decide.
    Browse(Option<BrowseFormat>),
    View,
    Edit,
    /// The `[[command]]` with this title.
    Command(String),
}

/// What a pane row's context menu offers besides what Enter does.
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize, specta::Type)]
pub struct RowActions {
    /// Open in Default App does something Enter doesn't.
    pub open_default: bool,
    /// Browse Into does something Enter doesn't.
    pub browse_into: bool,
}

impl Associations {
    pub fn new(user: Vec<AssociationEntry>, open_packages: bool) -> Self {
        let user_len = user.len();
        let entries: Vec<_> = user.into_iter().chain(defaults()).collect();
        let mut builder = GlobSetBuilder::new();
        let mut owners = Vec::new();
        let mut glob_patterns = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            for pattern in &entry.patterns {
                match GlobBuilder::new(pattern)
                    .case_insensitive(true)
                    .literal_separator(true)
                    .build()
                {
                    Ok(glob) => {
                        builder.add(glob);
                        owners.push((index, sample_name(pattern).1));
                        glob_patterns.push(pattern.clone());
                    }
                    Err(e) => warn!("association: skipping pattern {pattern:?}: {e}"),
                }
            }
        }
        let globs = builder.build().unwrap_or_else(|e| {
            warn!("association: no patterns usable: {e}");
            GlobSet::empty()
        });
        Self {
            entries,
            user_len,
            globs,
            owners,
            glob_patterns,
            open_packages,
        }
    }

    /// The entries matching `name`, in resolution order, leaving `skip` out.
    fn matching(&self, name: &str, kind: AssociationKind, skip: Option<usize>) -> Vec<usize> {
        let mut matching: Vec<(usize, usize)> = self
            .globs
            .matches(name)
            .into_iter()
            .map(|glob| self.owners[glob])
            .filter(|&(entry, _)| self.entries[entry].kind == kind && Some(entry) != skip)
            .collect();
        // Most specific first; an entry matched by several patterns by its
        // most specific one.
        matching
            .sort_unstable_by_key(|&(entry, specificity)| (std::cmp::Reverse(specificity), entry));
        let mut seen = std::collections::HashSet::new();
        matching
            .into_iter()
            .map(|(entry, _)| entry)
            .filter(|entry| seen.insert(*entry))
            .collect()
    }

    pub fn trace(&self, name: &str, kind: AssociationKind, skip: Option<usize>) -> Traced {
        let matching = self.matching(name, kind, skip);
        let mut traced = Traced {
            matched: !matching.is_empty(),
            ..Default::default()
        };
        for index in matching {
            let entry = &self.entries[index];
            if traced.enter.is_none() {
                traced.enter = EnterChoice::of(entry).map(|e| (e, index));
            }
            if traced.format.is_none() {
                traced.format = entry.format.map(|f| (f, index));
            }
            if traced.viewer.is_none() {
                traced.viewer = entry.viewer.map(|v| (v, index));
            }
            if traced.language.is_none() {
                traced.language = entry.language.clone().map(|l| (l, index));
            }
        }
        traced
    }

    /// What Enter does with `name`. Opening a directory hands its path to
    /// the system, so only one on a host-local filesystem opens. `package`
    /// is what the directory's filesystem says it is; an entry's `enter`
    /// overrides what that implies.
    pub fn action(&self, name: &str, is_dir: bool, host_local: bool, package: bool) -> Action {
        if is_dir {
            let traced = self.trace(name, AssociationKind::Directory, None);
            return match traced.enter.map(|(e, _)| e) {
                Some(EnterChoice::Open) if host_local => Action::Open,
                Some(EnterChoice::Command { command }) => Action::Command(command),
                None if package && host_local && self.open_packages => Action::Open,
                _ => Action::Navigate,
            };
        }
        let traced = self.trace(name, AssociationKind::File, None);
        match traced.enter.map(|(e, _)| e) {
            None | Some(EnterChoice::Open) => Action::Open,
            Some(EnterChoice::Browse) => Action::Browse(traced.format.map(|(f, _)| f)),
            Some(EnterChoice::View) => Action::View,
            Some(EnterChoice::Edit) => Action::Edit,
            Some(EnterChoice::Command { command }) => Action::Command(command),
        }
    }

    /// The format Browse Into browses a file as, if any entry names one.
    pub fn format(&self, name: &str) -> Option<BrowseFormat> {
        self.trace(name, AssociationKind::File, None)
            .format
            .map(|(f, _)| f)
    }

    pub fn row_actions(
        &self,
        name: &str,
        is_dir: bool,
        host_local: bool,
        package: bool,
    ) -> RowActions {
        if name == ".." {
            return RowActions::default();
        }
        let action = self.action(name, is_dir, host_local, package);
        if is_dir {
            RowActions {
                // A package; any other directory opens in the file manager.
                open_default: host_local
                    && action != Action::Open
                    && (package || self.trace(name, AssociationKind::Directory, None).matched),
                browse_into: action != Action::Navigate,
            }
        } else {
            RowActions {
                open_default: action != Action::Open,
                browse_into: !matches!(action, Action::Browse(_)) && self.format(name).is_some(),
            }
        }
    }

    /// How the viewer shows a file named `name` of `size` bytes whose
    /// content is `content`: its mode, and the MIME type it is shown as.
    /// An entry's viewer comes first, then [`by_content`].
    pub fn view(
        &self,
        name: &str,
        size: u64,
        content: Content<'_>,
    ) -> (ViewerMode, Option<String>) {
        let (mode, mime) = by_content(name, size, content);
        match self.trace(name, AssociationKind::File, None).viewer {
            Some((viewer, _)) => (viewer, mime),
            None => (mode, mime),
        }
    }

    /// The editor language for a file, falling back to its MIME type.
    pub fn language(&self, name: &str, mime: Option<&str>) -> String {
        self.trace(name, AssociationKind::File, None)
            .language
            .map_or_else(|| language_for_mime(mime).into(), |(l, _)| l)
    }
}

impl PartialEq for Associations {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl std::fmt::Debug for Associations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Associations")
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// A name `pattern` matches, and how many of its characters are literal —
/// its specificity. `*.tar.gz` gives `x.tar.gz` and 7.
fn sample_name(pattern: &str) -> (String, usize) {
    let mut name = String::new();
    let mut literal = 0;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '*' | '?' => name.push('x'),
            '[' => {
                let class: String = chars.by_ref().take_while(|&c| c != ']').collect();
                let sample = match class.strip_prefix(['!', '^']) {
                    Some(excluded) => "x_0".chars().find(|c| !excluded.contains(*c)),
                    None => class.chars().next(),
                };
                name.push(sample.unwrap_or('x'));
            }
            '{' => {
                let mut alternatives = chars.by_ref().take_while(|&c| c != '}');
                for c in alternatives.by_ref().take_while(|&c| c != ',') {
                    name.push(c);
                    literal += 1;
                }
                alternatives.for_each(drop);
            }
            '\\' => {
                if let Some(c) = chars.next() {
                    name.push(c);
                    literal += 1;
                }
            }
            c => {
                name.push(c);
                literal += 1;
            }
        }
    }
    (name, literal)
}

/// Where a value a row doesn't set itself comes from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, specta::Type)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Origin {
    /// Another of the user's entries.
    Entry {
        patterns: Vec<String>,
    },
    BuiltIn,
    /// The MIME type the name implies.
    FileType,
    /// What files of the name usually hold (see `usual_viewer`).
    Content,
    /// Nothing: Enter opens a file in its default application and goes
    /// into a directory, and Browse Into reads the file's first bytes.
    Default,
}

/// One property of a row: what its own entry sets, and what applies when
/// it sets nothing.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct AssociationCell<T> {
    pub set: Option<T>,
    pub inherited: Option<T>,
    pub origin: Origin,
}

/// A name pattern and what happens to what it matches.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct AssociationRow {
    pub pattern: String,
    pub kind: AssociationKind,
    /// The settings-file entry this row edits holds these patterns too.
    pub shared_with: Vec<String>,
    /// The settings file has an entry for the pattern.
    pub customized: bool,
    pub enter: AssociationCell<EnterChoice>,
    pub format: AssociationCell<BrowseFormat>,
    pub viewer: AssociationCell<ViewerMode>,
    pub language: AssociationCell<String>,
}

/// What happens to a file of a given name, everything considered.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct NamePreview {
    pub name: String,
    pub enter: Option<EnterChoice>,
    pub format: Option<BrowseFormat>,
    /// `None`: the file's contents decide.
    pub viewer: Option<ViewerMode>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct AssociationTable {
    pub rows: Vec<AssociationRow>,
    /// The query, read as a file name.
    pub name: Option<NamePreview>,
    /// When no row matches: the pattern the query would add.
    pub candidate: Option<String>,
}

/// A pattern as the user types it: `.rtf` means `*.rtf`; `None` when it
/// is empty or not a glob.
fn normalize_pattern(pattern: &str) -> Option<String> {
    let pattern = pattern.trim();
    let pattern = if pattern.starts_with('.') {
        format!("*{pattern}")
    } else {
        pattern.to_string()
    };
    (!pattern.is_empty() && glob(&pattern).is_some()).then_some(pattern)
}

fn glob(pattern: &str) -> Option<globset::GlobMatcher> {
    GlobBuilder::new(pattern)
        .case_insensitive(true)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

/// The MIME type `name`'s extension suggests.
pub fn mime_of(name: &str) -> Option<String> {
    mime_guess::from_path(name)
        .first()
        .map(|m| m.essence_str().to_string())
}

/// What a file's content was identified as.
#[derive(Clone, Copy, Debug, Default)]
pub struct Content<'a> {
    /// The identified format's MIME type; `None` when nothing matched.
    pub mime: Option<&'a str>,
    /// Whether the content is text, whatever else it is.
    pub text: bool,
}

/// How the viewer shows a file named `name` of `size` bytes whose content
/// is `content`, without associations: its mode, and the MIME type it is
/// shown as. The content decides, except that text with a name naming a
/// text mode (`.md`, `.csv`, `.txt`) is shown as the name says.
pub fn by_content(name: &str, size: u64, content: Content<'_>) -> (ViewerMode, Option<String>) {
    let by_name = mime_of(name);
    let named_text = matches!(
        viewer_for_mime(by_name.as_deref()),
        ViewerMode::Text | ViewerMode::Markdown | ViewerMode::Table
    );
    let mime = if size == 0 || (content.text && named_text) {
        by_name
    } else {
        content
            .mime
            .map(|m| m.split(';').next().unwrap_or(m).trim().to_string())
    };
    let mode = match viewer_for_mime(mime.as_deref()) {
        ViewerMode::Image if size > IMAGE_MAX_SIZE => ViewerMode::Dissect,
        ViewerMode::Hex if content.mime.is_some() => ViewerMode::Dissect,
        mode => mode,
    };
    (mode, mime)
}

/// The mode the viewer usually picks for a file named `name`, for the
/// associations editor, which has no content to go on: a name naming a text
/// mode keeps it, as text content does, and one with an editor language
/// (`.ts`, `Dockerfile`) is text unless its type picks a mode of its own
/// (`.svg`); otherwise what [`by_content`] makes of the formats claiming its
/// extension, if they agree. `None` when it depends on the content.
fn usual_viewer(name: &str, has_language: bool) -> Option<ViewerMode> {
    let by_name = mime_of(name).map(|m| viewer_for_mime(Some(&m)));
    if matches!(
        by_name,
        Some(ViewerMode::Text | ViewerMode::Markdown | ViewerMode::Table)
    ) {
        return by_name;
    }
    if has_language && matches!(by_name, None | Some(ViewerMode::Hex)) {
        return Some(ViewerMode::Text);
    }
    let claims = name
        .rsplit_once('.')
        .map(|(_, extension)| fillyfoal::formats::by_extension(extension))
        .unwrap_or_default();
    let mut modes = claims.iter().map(|format| {
        let content = Content {
            mime: Some(format.mime),
            text: false,
        };
        by_content(name, 1, content).0
    });
    let first = modes.next()?;
    modes.all(|mode| mode == first).then_some(first)
}

/// The largest image Image mode is chosen for: the webview holds all of
/// an image's bytes in one block while decoding it, so a bigger one takes
/// the viewer down.
const IMAGE_MAX_SIZE: u64 = 512 << 20;

impl Associations {
    fn origin(&self, index: usize) -> Origin {
        if index < self.user_len {
            Origin::Entry {
                patterns: self.entries[index].patterns.clone(),
            }
        } else {
            Origin::BuiltIn
        }
    }

    /// The settings-file entry a row for `pattern` edits.
    fn own_entry(&self, pattern: &str, kind: AssociationKind) -> Option<usize> {
        (0..self.user_len).find(|&index| {
            let entry = &self.entries[index];
            entry.kind == kind
                && entry
                    .patterns
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(pattern))
        })
    }

    fn row(&self, pattern: &str, kind: AssociationKind) -> AssociationRow {
        let own = self.own_entry(pattern, kind);
        let sample = sample_name(pattern).0;
        let traced = self.trace(&sample, kind, own);
        let own_entry = own.map(|i| &self.entries[i]);

        fn cell<T>(
            set: Option<T>,
            inherited: Option<(T, usize)>,
            origin: impl Fn(usize) -> Origin,
            otherwise: (Option<T>, Origin),
        ) -> AssociationCell<T> {
            match inherited {
                Some((value, index)) => AssociationCell {
                    set,
                    inherited: Some(value),
                    origin: origin(index),
                },
                None => AssociationCell {
                    set,
                    inherited: otherwise.0,
                    origin: otherwise.1,
                },
            }
        }
        let mime = mime_of(&sample);
        AssociationRow {
            pattern: pattern.to_string(),
            kind,
            shared_with: own_entry
                .map(|e| {
                    e.patterns
                        .iter()
                        .filter(|p| !p.eq_ignore_ascii_case(pattern))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            customized: own.is_some(),
            enter: cell(
                own_entry.and_then(EnterChoice::of),
                traced.enter,
                |i| self.origin(i),
                (None, Origin::Default),
            ),
            format: cell(
                own_entry.and_then(|e| e.format),
                traced.format,
                |i| self.origin(i),
                (None, Origin::Default),
            ),
            viewer: cell(
                own_entry.and_then(|e| e.viewer),
                traced.viewer,
                |i| self.origin(i),
                match usual_viewer(&sample, traced.language.is_some()) {
                    Some(viewer) => (Some(viewer), Origin::Content),
                    None => (None, Origin::Default),
                },
            ),
            language: cell(
                own_entry.and_then(|e| e.language.clone()),
                traced.language,
                |i| self.origin(i),
                (
                    mime.as_deref()
                        .map(|m| language_for_mime(Some(m)).to_string()),
                    Origin::FileType,
                ),
            ),
        }
    }

    /// Every pattern in effect as a row — the user's first — narrowed to
    /// those containing `query` or matching it as a name.
    pub fn table(&self, query: &str) -> AssociationTable {
        let query = query.trim();
        let lower = query.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut matched_as_name = false;
        let mut rows: Vec<AssociationRow> = self
            .entries
            .iter()
            .flat_map(|entry| entry.patterns.iter().map(move |p| (p, entry.kind)))
            .filter(|(pattern, kind)| seen.insert((pattern.to_lowercase(), *kind)))
            .filter(|(pattern, _)| {
                if query.is_empty() {
                    return true;
                }
                let hit = glob(pattern).is_some_and(|g| g.is_match(query));
                matched_as_name |= hit;
                hit || pattern.to_lowercase().contains(&lower)
            })
            .map(|(pattern, kind)| self.row(pattern, kind))
            .collect();
        let sort_key = |row: &AssociationRow| {
            (
                !row.customized,
                row.pattern
                    .trim_start_matches('*')
                    .trim_start_matches('.')
                    .to_lowercase(),
                row.kind != AssociationKind::File,
            )
        };
        rows.sort_by_cached_key(sort_key);

        let wildcard = query.contains(['*', '?', '[', '{']);
        let looks_like_name =
            !wildcard && !query.starts_with('.') && (query.contains('.') || matched_as_name);
        let name = looks_like_name.then(|| {
            let traced = self.trace(query, AssociationKind::File, None);
            let mime = mime_of(query);
            NamePreview {
                name: query.to_string(),
                enter: traced.enter.map(|(e, _)| e),
                format: traced.format.map(|(f, _)| f),
                viewer: traced
                    .viewer
                    .map(|(v, _)| v)
                    .or_else(|| usual_viewer(query, traced.language.is_some())),
                language: traced
                    .language
                    .map(|(l, _)| l)
                    .or_else(|| mime.as_deref().map(|m| language_for_mime(Some(m)).into())),
            }
        });

        let candidate = rows.is_empty().then(|| normalize_pattern(query)).flatten();

        AssociationTable {
            rows,
            name,
            candidate,
        }
    }

    /// The row for a pattern as typed — `.rtf` read as `*.rtf` — whether
    /// or not anything sets it yet; `None` for one that isn't a pattern.
    pub fn row_for(&self, pattern: &str, kind: AssociationKind) -> Option<AssociationRow> {
        normalize_pattern(pattern).map(|pattern| self.row(&pattern, kind))
    }

    /// The pattern whose row decides `name`: the most specific one in
    /// effect, or the one to add for it — its extension for a file that
    /// has one, else the name itself. `true` with a pattern in effect.
    pub fn pattern_for(&self, name: &str, kind: AssociationKind) -> (String, bool) {
        let best = self
            .globs
            .matches(name)
            .into_iter()
            .filter(|&glob| self.entries[self.owners[glob].0].kind == kind)
            .max_by_key(|&glob| (self.owners[glob].1, std::cmp::Reverse(self.owners[glob].0)));
        if let Some(glob) = best {
            return (self.glob_patterns[glob].clone(), true);
        }
        let extension = (kind == AssociationKind::File)
            .then(|| name.rsplit_once('.'))
            .flatten()
            .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty());
        match extension {
            Some((_, ext)) => (format!("*.{ext}"), false),
            None => (name.to_string(), false),
        }
    }
}

impl BrowseFormat {
    pub fn mount_request(
        self,
        origin: newt_common::vfs::VfsPath,
    ) -> newt_common::vfs::MountRequest {
        use newt_common::vfs::{ArchiveFormat, MountRequest};
        let format = match self {
            BrowseFormat::Disc => return MountRequest::Disc { origin },
            BrowseFormat::Zip => ArchiveFormat::Zip,
            BrowseFormat::SevenZ => ArchiveFormat::SevenZ,
            BrowseFormat::Tar => ArchiveFormat::Tar,
            BrowseFormat::Compressed => ArchiveFormat::Compressed,
        };
        MountRequest::Archive { origin, format }
    }
}

const TEXT_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/xml",
    "application/javascript",
    "application/typescript",
    "application/xhtml+xml",
    "application/x-sh",
    "application/x-csh",
    "application/x-python",
    "application/x-ruby",
    "application/x-perl",
    "application/x-lua",
    "application/sql",
    "application/x-yaml",
    "application/toml",
    "application/graphql",
    "application/ld+json",
    "application/x-httpd-php",
    "image/svg+xml",
];

fn viewer_for_mime(mime: Option<&str>) -> ViewerMode {
    let Some(mime) = mime else {
        return ViewerMode::Hex;
    };
    match mime {
        "text/csv" | "text/tab-separated-values" => ViewerMode::Table,
        "text/markdown" | "text/x-markdown" => ViewerMode::Markdown,
        "application/pdf" => ViewerMode::Pdf,
        _ if webview_formats::plays_video(mime) => ViewerMode::Video,
        _ if webview_formats::plays_audio(mime) => ViewerMode::Audio,
        _ if webview_formats::shows_image(mime) => ViewerMode::Image,
        _ if mime.starts_with("text/")
            || TEXT_MIME_TYPES.contains(&mime)
            || mime.ends_with("+xml")
            || mime.ends_with("+json") =>
        {
            ViewerMode::Text
        }
        _ => ViewerMode::Hex,
    }
}

fn language_for_mime(mime: Option<&str>) -> &'static str {
    match mime {
        Some(m) if m == "application/json" || m.ends_with("+json") => "json",
        Some(m) if m == "application/xml" || m.ends_with("+xml") => "xml",
        Some("application/javascript") => "javascript",
        Some("application/typescript") => "typescript",
        Some("text/x-python") => "python",
        Some("text/x-shellscript") => "shell",
        _ => "plaintext",
    }
}

fn entry(patterns: &[&str]) -> AssociationEntry {
    AssociationEntry {
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        kind: AssociationKind::File,
        enter: None,
        format: None,
        command: None,
        viewer: None,
        language: None,
    }
}

fn browse(patterns: &[&str], format: BrowseFormat) -> AssociationEntry {
    AssociationEntry {
        enter: Some(EnterAction::Browse),
        format: Some(format),
        ..entry(patterns)
    }
}

/// Containers that open in their own application, but can be browsed.
fn browsable(patterns: &[&str], format: BrowseFormat) -> AssociationEntry {
    AssociationEntry {
        format: Some(format),
        ..entry(patterns)
    }
}

fn language(patterns: &[&str], language: &str) -> AssociationEntry {
    AssociationEntry {
        language: Some(language.into()),
        ..entry(patterns)
    }
}

/// The shipped associations, in resolution order. A name more specific
/// than another's goes first: `*.tar.gz` before `*.gz`.
fn defaults() -> Vec<AssociationEntry> {
    vec![
        // Everything iluvatar indexes: tar, cpio and ar (static libraries,
        // Debian packages). Windows `.lib` is ar too, but the extension is
        // shared with plain-text library formats (KiCad, SPICE).
        browse(
            &[
                "*.tar",
                "*.tar.gz",
                "*.tgz",
                "*.tar.bz2",
                "*.tbz2",
                "*.tbz",
                "*.tar.xz",
                "*.txz",
                "*.tar.zst",
                "*.tzst",
                "*.tar.zstd",
                "*.cpio",
                "*.cpio.gz",
                "*.cpio.bz2",
                "*.cpio.xz",
                "*.cpio.zst",
                "*.a",
                "*.ar",
                "*.deb",
            ],
            BrowseFormat::Tar,
        ),
        browse(
            &["*.zip", "*.jar", "*.war", "*.ear", "*.apk", "*.ipa"],
            BrowseFormat::Zip,
        ),
        browse(&["*.7z"], BrowseFormat::SevenZ),
        browse(
            &["*.gz", "*.bz2", "*.xz", "*.zst", "*.zstd"],
            BrowseFormat::Compressed,
        ),
        browse(&["*.iso", "*.udf"], BrowseFormat::Disc),
        browsable(
            &[
                "*.docx", "*.xlsx", "*.pptx", "*.odt", "*.ods", "*.odp", "*.epub", "*.whl",
                "*.nupkg", "*.vsix",
            ],
            BrowseFormat::Zip,
        ),
        browsable(&["*.crate"], BrowseFormat::Tar),
        language(&["*.ts", "*.tsx", "*.mts", "*.cts"], "typescript"),
        language(&["Dockerfile", "*.dockerfile"], "dockerfile"),
        language(&["*.js", "*.mjs", "*.cjs", "*.jsx"], "javascript"),
        language(&["*.html", "*.htm"], "html"),
        language(&["*.css"], "css"),
        language(&["*.scss"], "scss"),
        language(&["*.less"], "less"),
        language(&["*.json", "*.jsonc"], "json"),
        language(&["*.yaml", "*.yml"], "yaml"),
        language(&["*.toml", "*.ini"], "ini"),
        language(&["*.xml", "*.svg"], "xml"),
        language(&["*.py"], "python"),
        language(&["*.rs"], "rust"),
        language(&["*.go"], "go"),
        language(&["*.java"], "java"),
        language(&["*.kt", "*.kts"], "kotlin"),
        language(&["*.c", "*.h"], "c"),
        language(&["*.cpp", "*.cc", "*.cxx", "*.hpp", "*.hxx"], "cpp"),
        language(&["*.cs"], "csharp"),
        language(&["*.rb"], "ruby"),
        language(&["*.php"], "php"),
        language(&["*.swift"], "swift"),
        language(&["*.m"], "objective-c"),
        language(&["*.r"], "r"),
        language(&["*.lua"], "lua"),
        language(&["*.pl", "*.pm"], "perl"),
        language(&["*.sh", "*.bash", "*.zsh", "*.fish"], "shell"),
        language(&["*.ps1"], "powershell"),
        language(&["*.bat", "*.cmd"], "bat"),
        language(&["*.md", "*.mdx"], "markdown"),
        language(&["*.sql"], "sql"),
        language(&["*.graphql", "*.gql"], "graphql"),
        language(&["*.tf"], "hcl"),
    ]
}

/// Every language the editor offers, as `(Monaco id, label)`.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("plaintext", "Plain Text"),
    ("bat", "Batch"),
    ("c", "C"),
    ("cpp", "C++"),
    ("csharp", "C#"),
    ("css", "CSS"),
    ("dockerfile", "Dockerfile"),
    ("go", "Go"),
    ("graphql", "GraphQL"),
    ("hcl", "HCL"),
    ("html", "HTML"),
    ("ini", "INI / TOML"),
    ("java", "Java"),
    ("javascript", "JavaScript"),
    ("json", "JSON"),
    ("kotlin", "Kotlin"),
    ("less", "Less"),
    ("lua", "Lua"),
    ("markdown", "Markdown"),
    ("objective-c", "Objective-C"),
    ("perl", "Perl"),
    ("php", "PHP"),
    ("powershell", "PowerShell"),
    ("python", "Python"),
    ("r", "R"),
    ("ruby", "Ruby"),
    ("rust", "Rust"),
    ("scss", "SCSS"),
    ("shell", "Shell"),
    ("sql", "SQL"),
    ("swift", "Swift"),
    ("typescript", "TypeScript"),
    ("xml", "XML"),
    ("yaml", "YAML"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn user(toml: &str) -> Associations {
        user_opening_packages(toml, false)
    }

    fn user_opening_packages(toml: &str, open_packages: bool) -> Associations {
        #[derive(serde::Deserialize)]
        struct File {
            association: Vec<AssociationEntry>,
        }
        let file: File = toml::from_str(toml).unwrap();
        Associations::new(file.association, open_packages)
    }

    fn defaults_only() -> Associations {
        Associations::new(Vec::new(), false)
    }

    #[test]
    fn archives_browse_by_their_most_specific_name() {
        let a = defaults_only();
        let browse = |name| a.action(name, false, true, false);
        assert_eq!(browse("x.tar.gz"), Action::Browse(Some(BrowseFormat::Tar)));
        assert_eq!(browse("X.TGZ"), Action::Browse(Some(BrowseFormat::Tar)));
        assert_eq!(
            browse("notes.txt.gz"),
            Action::Browse(Some(BrowseFormat::Compressed))
        );
        assert_eq!(browse("app.APK"), Action::Browse(Some(BrowseFormat::Zip)));
        assert_eq!(browse("a.7z"), Action::Browse(Some(BrowseFormat::SevenZ)));
        assert_eq!(
            browse("pkg_1.0_amd64.deb"),
            Action::Browse(Some(BrowseFormat::Tar))
        );
        assert_eq!(browse("disk.iso"), Action::Browse(Some(BrowseFormat::Disc)));
        assert_eq!(browse("foo.lib"), Action::Open);
        assert_eq!(browse("tarfile"), Action::Open);
    }

    #[test]
    fn user_entries_override_one_property_at_a_time() {
        let a = user(
            r#"
            [[association]]
            match = "*.zip"
            viewer = "hex"

            [[association]]
            match = ["*.nupkg", "*.VSIX"]
            enter = "browse"
            "#,
        );
        assert_eq!(
            a.action("x.zip", false, true, false),
            Action::Browse(Some(BrowseFormat::Zip))
        );
        assert_eq!(
            a.view("x.zip", 10, content("application/zip", false)).0,
            ViewerMode::Hex
        );
        // The format still comes from the defaults.
        assert_eq!(
            a.action("pkg.vsix", false, true, false),
            Action::Browse(Some(BrowseFormat::Zip))
        );
    }

    #[test]
    fn entries_match_their_kind_only() {
        let a = user(
            r#"
            [[association]]
            match = "build"
            kind = "directory"
            enter = "command"
            command = "Build"
            "#,
        );
        assert_eq!(
            a.action("build", true, true, false),
            Action::Command("Build".into())
        );
        assert_eq!(a.action("build", false, true, false), Action::Open);
    }

    #[test]
    fn directories_open_only_on_the_host() {
        let a = user(
            r#"
            [[association]]
            match = "*.app"
            kind = "directory"
            enter = "open"
            "#,
        );
        assert_eq!(a.action("Safari.app", true, true, false), Action::Open);
        assert_eq!(a.action("Safari.app", true, false, false), Action::Navigate);
        assert_eq!(a.action("src", true, true, false), Action::Navigate);
    }

    #[test]
    fn packages_open_when_asked_to_and_on_the_host() {
        let browse = defaults_only();
        let open = Associations::new(Vec::new(), true);
        assert_eq!(
            browse.action("Safari.app", true, true, true),
            Action::Navigate
        );
        assert_eq!(open.action("Safari.app", true, true, true), Action::Open);
        assert_eq!(
            open.action("Safari.app", true, false, true),
            Action::Navigate
        );
        // A name proves nothing: only the filesystem says it's a package.
        assert_eq!(
            open.action("Safari.app", true, true, false),
            Action::Navigate
        );
        assert_eq!(
            browse.row_actions("Safari.app", true, true, true),
            RowActions {
                open_default: true,
                browse_into: false
            }
        );
        assert_eq!(
            open.row_actions("Safari.app", true, true, true),
            RowActions {
                open_default: false,
                browse_into: true
            }
        );
        // An entry's own `enter` wins.
        let user_browse = user_opening_packages(
            r#"
            [[association]]
            match = "*.app"
            kind = "directory"
            enter = "browse"
            "#,
            true,
        );
        assert_eq!(
            user_browse.action("Safari.app", true, true, true),
            Action::Navigate
        );
    }

    #[test]
    fn row_actions_offer_what_enter_does_not() {
        let a = defaults_only();
        let none = RowActions::default();
        assert_eq!(a.row_actions("..", true, true, false), none);
        assert_eq!(a.row_actions("src", true, true, false), none);
        assert_eq!(
            a.row_actions("x.zip", false, true, false),
            RowActions {
                open_default: true,
                browse_into: false
            }
        );
        assert_eq!(
            a.row_actions("report.docx", false, true, false),
            RowActions {
                open_default: false,
                browse_into: true
            }
        );
        assert_eq!(a.row_actions("notes.txt", false, true, false), none);
        let packages = user(
            r#"
            [[association]]
            match = "*.app"
            kind = "directory"
            "#,
        );
        assert_eq!(
            packages.row_actions("Safari.app", true, true, false),
            RowActions {
                open_default: true,
                browse_into: false
            }
        );
        assert_eq!(packages.row_actions("Safari.app", true, false, false), none);
    }

    #[test]
    fn language_falls_back_to_mime() {
        let a = defaults_only();
        assert_eq!(a.language("main.ts", None), "typescript");
        assert_eq!(a.language("Dockerfile", None), "dockerfile");
        assert_eq!(a.language("data", Some("application/ld+json")), "json");
        assert_eq!(a.language("README", Some("text/plain")), "plaintext");
    }

    fn content(mime: &'static str, text: bool) -> Content<'static> {
        Content {
            mime: Some(mime),
            text,
        }
    }

    #[test]
    fn the_editor_predicts_by_content() {
        let a = defaults_only();
        let usual = |pattern: &str| {
            let row = a.row_for(pattern, AssociationKind::File).unwrap();
            (row.viewer.inherited, row.viewer.origin)
        };
        let content = |mode| (Some(mode), Origin::Content);
        assert_eq!(usual("*.deb"), content(ViewerMode::Dissect));
        assert_eq!(usual("*.raw"), content(ViewerMode::Dissect));
        assert_eq!(usual("*.png"), content(ViewerMode::Image));
        assert_eq!(usual("*.svg"), content(ViewerMode::Image));
        assert_eq!(usual("*.md"), content(ViewerMode::Markdown));
        assert_eq!(usual("*.rs"), content(ViewerMode::Text));
        assert_eq!(usual("*.csv"), content(ViewerMode::Table));
        // An editor language says text: TypeScript, not a transport stream.
        assert_eq!(usual("*.ts"), content(ViewerMode::Text));
        assert_eq!(usual("Dockerfile"), content(ViewerMode::Text));
        assert_eq!(usual("*.unknown-extension"), (None, Origin::Default));
    }

    #[test]
    fn content_picks_the_viewer() {
        let a = defaults_only();
        let view = |name, size, c| a.view(name, size, c).0;
        assert_eq!(
            view("photo.png", 10, content("image/png", false)),
            ViewerMode::Image
        );
        // A disk image whose name says camera RAW, and nothing identified.
        assert_eq!(
            view("data.img.raw", 1 << 40, Content::default()),
            ViewerMode::Hex
        );
        // An image the webview can't decode is dissected.
        assert_eq!(
            view("shot.raw", 10, content("image/x-panasonic-rw", false)),
            ViewerMode::Dissect
        );
        assert_eq!(
            view("huge.png", IMAGE_MAX_SIZE + 1, content("image/png", false)),
            ViewerMode::Dissect
        );
        assert_eq!(
            view("notes.png", 10, content("text/plain", true)),
            ViewerMode::Text
        );
        assert_eq!(
            view("README", 10, content("text/markdown", true)),
            ViewerMode::Markdown
        );
        assert_eq!(
            view("blob", 10, content("application/zip", false)),
            ViewerMode::Dissect
        );
        assert_eq!(
            view("a.m4a", 10, content("audio/mp4; codecs=mp4a.40.2", false)),
            ViewerMode::Audio
        );
        assert_eq!(
            view("empty.md", 0, Content::default()),
            ViewerMode::Markdown
        );
        // `.ts` is TypeScript or an MPEG transport stream, which the webview
        // doesn't play: the content tells.
        assert_eq!(
            view("main.ts", 10, content("text/plain", true)),
            ViewerMode::Text
        );
        assert_eq!(
            view("clip.ts", 10, content("video/mp2t", false)),
            ViewerMode::Dissect
        );
    }

    #[test]
    fn a_text_name_picks_among_text_modes() {
        let a = defaults_only();
        let (mode, mime) = a.view("notes.txt", 10, content("text/markdown", true));
        assert_eq!(
            (mode, mime.as_deref()),
            (ViewerMode::Text, Some("text/plain"))
        );
        assert_eq!(
            a.view("data.csv", 10, content("text/plain", true)).0,
            ViewerMode::Table
        );
        // A name that doesn't name a text mode leaves it to the content.
        assert_eq!(
            a.view("logo.svg", 10, content("image/svg+xml", true)).0,
            ViewerMode::Image
        );
    }

    #[test]
    fn an_association_beats_the_content() {
        let a = user(
            r#"
            [[association]]
            match = ["*.bin"]
            viewer = "text"
            "#,
        );
        assert_eq!(
            a.view("x.bin", 10, content("image/png", false)).0,
            ViewerMode::Text
        );
    }

    #[test]
    fn default_languages_are_offered_by_the_editor() {
        for entry in defaults() {
            if let Some(language) = entry.language {
                assert!(
                    LANGUAGES.iter().any(|(id, _)| *id == language),
                    "{language}"
                );
            }
        }
    }

    #[test]
    fn the_most_specific_pattern_goes_first() {
        let a = user(
            r#"
            [[association]]
            match = "*.gz"
            enter = "open"
            "#,
        );
        assert_eq!(
            a.action("x.tar.gz", false, true, false),
            Action::Browse(Some(BrowseFormat::Tar))
        );
        assert_eq!(a.action("notes.txt.gz", false, true, false), Action::Open);
        // As specific as a built-in: the user's goes first.
        let a = user(
            r#"
            [[association]]
            match = "*.tar.gz"
            enter = "view"
            "#,
        );
        assert_eq!(a.action("x.tar.gz", false, true, false), Action::View);
    }

    #[test]
    fn a_command_comes_with_the_enter_that_chose_it() {
        let a = user(
            r#"
            [[association]]
            match = "*.py"
            enter = "command"

            [[association]]
            match = "*.p?"
            enter = "command"
            command = "Run"
            "#,
        );
        // The first entry has no command, so its Enter doesn't count.
        assert_eq!(
            a.action("x.py", false, true, false),
            Action::Command("Run".into())
        );
    }

    #[test]
    fn sample_names_and_specificity() {
        assert_eq!(sample_name("*.tar.gz"), ("x.tar.gz".into(), 7));
        assert_eq!(sample_name("Dockerfile"), ("Dockerfile".into(), 10));
        assert_eq!(sample_name("IMG_?.{jpg,png}"), ("IMG_x.jpg".into(), 8));
        assert_eq!(sample_name("[!x]y"), ("_y".into(), 1));
        assert_eq!(sample_name("[ch]pp"), ("cpp".into(), 2));
    }

    #[test]
    fn rows_show_what_they_set_and_what_they_inherit() {
        let a = user(
            r#"
            [[association]]
            match = "*.zip"
            viewer = "hex"

            [[association]]
            match = "*.gz"
            enter = "open"
            "#,
        );
        let table = a.table("");
        assert_eq!(table.rows[0].pattern, "*.gz");
        assert!(table.rows[0].customized && table.rows[1].customized);
        let zip = &table.rows[1];
        assert_eq!(zip.viewer.set, Some(ViewerMode::Hex));
        assert_eq!(zip.enter.set, None);
        assert_eq!(zip.enter.inherited, Some(EnterChoice::Browse));
        assert_eq!(zip.enter.origin, Origin::BuiltIn);
        // A built-in row a user entry overrides in part.
        let tgz = a
            .table("*.tar.gz")
            .rows
            .into_iter()
            .find(|r| r.pattern == "*.tar.gz")
            .unwrap();
        assert!(!tgz.customized);
        assert_eq!(tgz.enter.inherited, Some(EnterChoice::Browse));
        assert_eq!(a.table(".txt").candidate.as_deref(), Some("*.txt"));
        assert!(a.table(".zip").candidate.is_none());
        let txt = a.row_for(".txt", AssociationKind::File).unwrap();
        assert_eq!(txt.pattern, "*.txt");
        assert!(!txt.customized);
        assert_eq!(txt.viewer.inherited, Some(ViewerMode::Text));
        assert_eq!(txt.viewer.origin, Origin::Content);
        assert!(a.row_for("[", AssociationKind::File).is_none());
    }

    #[test]
    fn the_row_deciding_a_file() {
        let a = user(
            r#"
            [[association]]
            match = "*.gz"
            viewer = "hex"
            "#,
        );
        let file = AssociationKind::File;
        assert_eq!(
            a.pattern_for("release.tar.gz", file),
            ("*.tar.gz".into(), true)
        );
        assert_eq!(a.pattern_for("notes.txt.gz", file), ("*.gz".into(), true));
        assert_eq!(a.pattern_for("report.rtf", file), ("*.rtf".into(), false));
        assert_eq!(a.pattern_for("Makefile", file), ("Makefile".into(), false));
        assert_eq!(a.pattern_for(".bashrc", file), (".bashrc".into(), false));
        assert_eq!(
            a.pattern_for("src", AssociationKind::Directory),
            ("src".into(), false)
        );
    }

    #[test]
    fn a_name_query_shows_its_matches_and_what_happens() {
        let a = defaults_only();
        let table = a.table("release.tar.gz");
        let patterns: Vec<_> = table.rows.iter().map(|r| r.pattern.as_str()).collect();
        assert_eq!(patterns, ["*.gz", "*.tar.gz"]);
        let name = table.name.unwrap();
        assert_eq!(name.enter, Some(EnterChoice::Browse));
        assert_eq!(name.format, Some(BrowseFormat::Tar));
        assert!(a.table("zip").name.is_none());
        assert!(a.table("Dockerfile").name.is_some());
    }

    #[test]
    fn unusable_patterns_are_skipped() {
        let a = user(
            r#"
            [[association]]
            match = ["[", "*.foo"]
            viewer = "hex"
            "#,
        );
        assert_eq!(
            a.view("x.foo", 10, content("text/plain", true)).0,
            ViewerMode::Hex
        );
    }
}
