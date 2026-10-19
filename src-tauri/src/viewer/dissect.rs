//! The Structure mode: a file dissected by `fillyfoal` into a lazily
//! expanded tree of typed fields, each with the bytes it came from.
//!
//! A dissection is a `fillyfoal::Session` plus a positioned reader on the
//! file; the session asks for bytes and this side reads them, so a file on
//! S3, in an archive or across a remote session is read only where the tree
//! is expanded. Sessions belong to the window that opened them and end with
//! it, or when the viewer lets go of them.
//!
//! Nodes cross to the frontend under small integer keys: fillyfoal's own
//! handles aren't serializable. A key always names the same node; one whose
//! node is gone (dropped when an ancestor is dissected as another format)
//! reads as an empty leaf.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fillyfoal::{
    Activity, ByteRequest, ChildState, Count, DecodeProgress, DiagKind, Diagnostic, Limits, NodeId,
    Progress, ReadProgress, Secret, SecretRequest, Session, SourceId, Span, Wait, formats,
};
use newt_common::filesystem::Filesystem;
use newt_common::vfs::{VfsPath, VfsRandomReader};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, State};
use tokio_util::sync::CancellationToken;

use crate::GlobalContext;
use crate::common::{Error, RawBytes};
use crate::main_window::MainWindowContext;

use super::CopyFormat;

mod value;

use value::DissectValue;

/// Work units per step, between which the dissection lets other requests
/// in and yields its runtime worker.
const BUDGET: u64 = 10_000;
/// Children produced per request for more.
const PAGE: u64 = 200;
/// How many children `locate` looks through under one node before giving up
/// on it.
const LOCATE_SCAN: usize = 20_000;
/// How many nodes `locate` looks through below one whose children don't
/// hold the byte, before it gives up there.
const LOCATE_SEARCH: usize = 4_096;
/// How many of a node's children `content_of` looks through for the file it
/// holds.
const CONTENT_SCAN: usize = 16;
const LOCATE_DEPTH: usize = 64;
/// Largest byte range the hex pane or a copy may ask for at once.
const MAX_READ: u64 = 16 * 1024 * 1024;
/// Bytes read at a time when saving a span to a file.
const SAVE_CHUNK: u64 = 1024 * 1024;
/// How often a running expansion reports how far it has got, and how long
/// it runs before it starts to: a quick one never does.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// The dissections open in every window, by id, with the window's label.
#[derive(Default)]
pub struct Dissections {
    next: AtomicU64,
    open: Mutex<HashMap<u64, (String, Arc<Dissection>)>>,
}

impl Dissections {
    /// End the dissections a window had open.
    pub fn forget_window(&self, label: &str) {
        self.open.lock().retain(|_, (owner, dissection)| {
            let keep = owner != label;
            if !keep {
                dissection.stop(None);
            }
            keep
        });
    }

    fn get(&self, id: u64) -> Result<Arc<Dissection>, Error> {
        self.open
            .lock()
            .get(&id)
            .map(|(_, d)| d.clone())
            .ok_or_else(|| Error::Custom("the dissection has ended".into()))
    }
}

/// A dissection. Requests share it: each holds the state only for a step
/// of work, so a long expansion never keeps the others waiting, and stops
/// when its token is cancelled. An expansion that stops stays parked with
/// what it produced, and continues when its node is asked for again.
pub struct Dissection {
    id: u64,
    /// Where progress goes; none in tests.
    window: Option<tauri::WebviewWindow>,
    root: NodeId,
    file: SourceId,
    state: tokio::sync::Mutex<DissectionState>,
    reader: tokio::sync::Mutex<Box<dyn VfsRandomReader>>,
    stops: Mutex<Stops>,
}

/// What stops the work in flight: everything, or what one node's requests
/// are doing.
#[derive(Default)]
struct Stops {
    all: CancellationToken,
    nodes: HashMap<u32, CancellationToken>,
}

struct DissectionState {
    session: Session,
    ids: Vec<NodeId>,
    keys: HashMap<NodeId, u32>,
    /// Every source a span sent to the frontend names, by index: the
    /// session has no lookup from an index back to a source.
    sources: HashMap<u32, SourceId>,
    /// Passwords the expansions are waiting for; the frontend answers them
    /// by index and asks again.
    secrets: Vec<SecretRequest>,
}

impl Dissection {
    /// A dissection of the `len` bytes `reader` reads, called `name`, with
    /// the formats chosen for it or for files within it, and whatever else
    /// identified. Nothing is read until something is expanded.
    fn open(
        id: u64,
        window: Option<tauri::WebviewWindow>,
        reader: Box<dyn VfsRandomReader>,
        name: String,
        len: u64,
        choices: &[(Vec<u64>, &'static formats::Format)],
    ) -> Self {
        let mut session = Session::new(Limits::default());
        let root = session.open(name, len);
        for (path, format) in choices {
            session.reinterpret_at(root, path, Some(format));
        }
        let file = session
            .node(root)
            .and_then(|n| n.span)
            .map_or(SourceId::default_host(), |s| s.source);
        Dissection {
            id,
            window,
            root,
            file,
            state: tokio::sync::Mutex::new(DissectionState {
                session,
                ids: Vec::new(),
                keys: HashMap::new(),
                sources: HashMap::from([(file.index(), file)]),
                secrets: Vec::new(),
            }),
            reader: tokio::sync::Mutex::new(reader),
            stops: Mutex::new(Stops::default()),
        }
    }

    /// What stops a request: for one about `node`, also stopping that
    /// node's work.
    fn token(&self, node: Option<u32>) -> CancellationToken {
        let mut stops = self.stops.lock();
        let Stops { all, nodes } = &mut *stops;
        match node {
            Some(key) => nodes
                .entry(key)
                .or_insert_with(|| all.child_token())
                .clone(),
            None => all.child_token(),
        }
    }

    /// Stop the requests about `node`, or with `None`, every request so
    /// far.
    fn stop(&self, node: Option<u32>) {
        let mut stops = self.stops.lock();
        match node {
            Some(key) => {
                if let Some(token) = stops.nodes.remove(&key) {
                    token.cancel();
                }
            }
            None => {
                stops.all.cancel();
                *stops = Stops::default();
            }
        }
    }

    /// Run the expansion of `id` until it has what was asked of it, reading
    /// what it needs, until it waits for a password, or until `stop`. With
    /// `show`, the children it produces go to the window as they come.
    async fn drive(&self, id: NodeId, stop: &CancellationToken, show: bool) -> Result<(), Error> {
        let started = Instant::now();
        let mut reported = started;
        let mut sent = if show {
            let state = self.state.lock().await;
            Some(state.session.children(id).map_or(0, |c| c.ids.len()))
        } else {
            None
        };
        loop {
            if stop.is_cancelled() {
                return Ok(());
            }
            let (step, activity, key) = {
                let mut state = self.state.lock().await;
                let step = state.session.poll_node(id, BUDGET);
                let activity = state.session.activity(id);
                (step, activity, state.key(id))
            };
            match step {
                Progress::Idle => return Ok(()),
                Progress::NeedSecret(requests) => {
                    self.state.lock().await.secrets = requests;
                    return Ok(());
                }
                Progress::Yielded => tokio::task::yield_now().await,
                Progress::NeedBytes(requests) => {
                    tokio::select! {
                        fetched = self.fetch(requests) => fetched?,
                        _ = stop.cancelled() => return Ok(()),
                    }
                }
            }
            if started.elapsed() >= PROGRESS_EVERY && reported.elapsed() >= PROGRESS_EVERY {
                reported = Instant::now();
                if let Some((done, total)) = activity.as_ref().and_then(Activity::estimate) {
                    let codec = activity.and_then(|a| a.decoding.last().map(|d| d.codec));
                    self.report(key, done, total, codec);
                }
                if let Some(sent) = &mut sent {
                    let added = self.state.lock().await.added(id, sent);
                    if let Some((first, nodes)) = added {
                        self.show(key, first, nodes);
                    }
                }
            }
        }
    }

    fn report(&self, node: u32, done: u64, total: u64, codec: Option<&'static str>) {
        if let Some(window) = &self.window {
            let progress = DissectProgress {
                id: self.id,
                node,
                done,
                total,
                codec,
            };
            let _ = window.emit_to(window.label(), "dissect-progress", progress);
        }
    }

    fn report_task(&self, task: DissectTask, done: u64, total: u64, codec: Option<&'static str>) {
        if let Some(window) = &self.window {
            let progress = DissectTaskProgress {
                id: self.id,
                task,
                done,
                total,
                codec,
            };
            let _ = window.emit_to(window.label(), "dissect-task-progress", progress);
        }
    }

    fn show(&self, node: u32, first: u32, nodes: Vec<DissectNode>) {
        if let Some(window) = &self.window {
            let added = DissectChildrenAdded {
                id: self.id,
                node,
                first,
                nodes,
            };
            let _ = window.emit_to(window.label(), "dissect-children", added);
        }
    }

    /// Read the file bytes the session asked for into it.
    async fn fetch(&self, requests: Vec<ByteRequest>) -> Result<(), Error> {
        for request in requests {
            let data = self
                .reader
                .lock()
                .await
                .read_at(request.offset, request.len)
                .await?;
            let read = data.len() as u64;
            let mut state = self.state.lock().await;
            state.session.supply(request.source, request.offset, &data);
            if read < request.len {
                state
                    .session
                    .set_source_len(request.source, request.offset + read);
            }
        }
        Ok(())
    }

    /// Have at least `at_least` children of `id`, or all of them.
    async fn expand(
        &self,
        id: NodeId,
        at_least: u64,
        stop: &CancellationToken,
    ) -> Result<(), Error> {
        self.state.lock().await.session.expand(id, at_least);
        self.drive(id, stop, false).await
    }

    /// `expand`, for the node the viewer is expanding: its children are
    /// shown as they're produced. Expansions on the way to another node
    /// aren't, since the viewer has no state for those nodes to put them in.
    async fn expand_shown(
        &self,
        id: NodeId,
        at_least: u64,
        stop: &CancellationToken,
    ) -> Result<(), Error> {
        self.state.lock().await.session.expand(id, at_least);
        self.drive(id, stop, true).await
    }

    /// The bytes of `span`, from the file, a decoded stream or the pieces
    /// a fragmented one is made of, decoded a step at a time.
    async fn read(&self, span: Span, stop: &CancellationToken) -> Result<Vec<u8>, Error> {
        if span.len > MAX_READ {
            return Err(Error::Custom(format!(
                "{} bytes is more than can be read at once",
                span.len
            )));
        }
        if span.source == self.file {
            return Ok(self
                .reader
                .lock()
                .await
                .read_at(span.offset, span.len)
                .await?);
        }
        let started = Instant::now();
        let mut reported = started;
        loop {
            if stop.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let (step, chain) = {
                let mut state = self.state.lock().await;
                let step = state.session.read_step(span, BUDGET);
                (step, state.session.read_activity())
            };
            match step {
                ReadProgress::Done(bytes) => return Ok(bytes),
                ReadProgress::Yielded => tokio::task::yield_now().await,
                ReadProgress::NeedBytes(requests) => {
                    tokio::select! {
                        fetched = self.fetch(requests) => fetched?,
                        _ = stop.cancelled() => return Err(Error::Cancelled),
                    }
                }
            }
            if started.elapsed() >= PROGRESS_EVERY && reported.elapsed() >= PROGRESS_EVERY {
                reported = Instant::now();
                if let Some((done, total)) = chain.iter().rev().find_map(DecodeProgress::estimate) {
                    let codec = chain.last().map(|d| d.codec);
                    self.report_task(DissectTask::Decode, done, total, codec);
                }
            }
        }
    }

    /// Whether `id` holds a file within the file. Only its expansion tells,
    /// so an unexpanded node is expanded.
    async fn holds_content(&self, id: NodeId, stop: &CancellationToken) -> Result<bool, Error> {
        let unknown = {
            let state = self.state.lock().await;
            state.session.interpretation(id).is_none()
                && matches!(
                    state.session.children(id).map(|c| c.state),
                    Some(ChildState::NotRequested)
                )
        };
        if unknown {
            self.expand(id, 1, stop).await?;
        }
        Ok(self.state.lock().await.session.interpretation(id).is_some())
    }

    /// The node holding the file within the file that `id` stands for: `id`
    /// itself, or else the one child that holds one, as an archive member
    /// holds its data beside its headers.
    async fn content_of(
        &self,
        id: NodeId,
        stop: &CancellationToken,
    ) -> Result<Option<NodeId>, Error> {
        if self.holds_content(id, stop).await? {
            return Ok(Some(id));
        }
        if self.state.lock().await.session.children(id).is_some() {
            self.expand(id, CONTENT_SCAN as u64, stop).await?;
        }
        let children: Vec<NodeId> = self
            .state
            .lock()
            .await
            .session
            .children(id)
            .map(|c| c.ids.iter().take(CONTENT_SCAN).copied().collect())
            .unwrap_or_default();
        let mut holding = Vec::new();
        for child in children {
            if self.holds_content(child, stop).await? {
                holding.push(child);
            }
        }
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(match holding[..] {
            [child] => Some(child),
            _ => None,
        })
    }

    /// The path from the root to the innermost node holding byte `offset`
    /// of `source`, expanding along the way; as far as it got when stopped.
    ///
    /// A node can hold the byte itself or, for a decoded stream, the bytes
    /// the stream was decoded from: a "Content" node spans the compressed
    /// data, its fields the stream. Where no child holds either, the nodes
    /// below are searched, since some formats list a part away from its
    /// bytes (a ZIP's members under its Central Directory).
    async fn locate(
        &self,
        source: SourceId,
        offset: u64,
        stop: &CancellationToken,
    ) -> Result<Vec<NodeId>, Error> {
        let lineage = self.lineage(Span::new(source, offset, 1)).await;
        let mut path = vec![self.root];
        let mut current = self.root;
        while path.len() < LOCATE_DEPTH && !stop.is_cancelled() {
            let found = match self.holding_child(current, &lineage, stop).await? {
                Some(child) => vec![child],
                None => match self.search(current, &lineage, stop).await? {
                    Some(way) => way,
                    None => break,
                },
            };
            current = found[found.len() - 1];
            path.extend(found);
        }
        Ok(path)
    }

    /// `at`, then the bytes each stream it lies in was decoded from, down to
    /// the file.
    async fn lineage(&self, at: Span) -> Vec<Span> {
        let state = self.state.lock().await;
        let mut lineage = vec![at];
        while let Some(origin) = state.session.origin(lineage[lineage.len() - 1].source) {
            lineage.push(origin.parent);
        }
        lineage
    }

    /// How close `id` comes to the byte: 0 if it holds the byte, n if it
    /// holds the bytes the n-th stream down was decoded from; `None` if
    /// neither. Also whether it has children, and whether they can lead to
    /// the byte at all: only through the streams on its lineage.
    fn reach(state: &DissectionState, id: NodeId, lineage: &[Span]) -> (Option<usize>, bool, bool) {
        let Some(node) = state.session.node(id) else {
            return (None, false, false);
        };
        let span = node.span.filter(|s| !s.is_empty());
        let rank = span.and_then(|s| lineage.iter().position(|l| s.contains(l)));
        let leads = span.is_none_or(|s| lineage.iter().any(|l| l.source == s.source));
        (rank, node.has_children(), leads)
    }

    /// The child of `id` that comes closest to the byte, a container before
    /// a plain field at the same distance; paging through its children.
    async fn holding_child(
        &self,
        id: NodeId,
        lineage: &[Span],
        stop: &CancellationToken,
    ) -> Result<Option<NodeId>, Error> {
        let mut scanned = 0;
        loop {
            let unexpanded = matches!(
                self.state
                    .lock()
                    .await
                    .session
                    .children(id)
                    .map(|c| c.state),
                Some(ChildState::NotRequested)
            );
            if unexpanded {
                self.expand(id, PAGE, stop).await?;
            }
            let (best, more) = {
                let state = self.state.lock().await;
                let Some(children) = state.session.children(id) else {
                    return Ok(None);
                };
                let best = children.ids[scanned.min(children.ids.len())..]
                    .iter()
                    .filter_map(|&child| {
                        let (rank, parent, _) = Self::reach(&state, child, lineage);
                        rank.map(|rank| ((rank, !parent), child))
                    })
                    .min_by_key(|(order, _)| *order)
                    .map(|(_, child)| child);
                scanned = children.ids.len();
                (best, children.state == ChildState::More)
            };
            if best.is_some() || !more || scanned > LOCATE_SCAN || stop.is_cancelled() {
                return Ok(best);
            }
            self.state.lock().await.session.expand_more(id, PAGE);
            self.drive(id, stop, false).await?;
        }
    }

    /// The way down from `id` to the nearest node below it that comes close
    /// to the byte, searched breadth first through the nodes that can lead
    /// to it, up to `LOCATE_SEARCH` of them.
    async fn search(
        &self,
        id: NodeId,
        lineage: &[Span],
        stop: &CancellationToken,
    ) -> Result<Option<Vec<NodeId>>, Error> {
        let mut queue = std::collections::VecDeque::from([(id, Vec::new())]);
        let mut budget = LOCATE_SEARCH;
        while let Some((parent, way)) = queue.pop_front() {
            let mut scanned = 0;
            loop {
                if stop.is_cancelled() || budget == 0 {
                    return Ok(None);
                }
                let unexpanded = matches!(
                    self.state
                        .lock()
                        .await
                        .session
                        .children(parent)
                        .map(|c| c.state),
                    Some(ChildState::NotRequested)
                );
                if unexpanded {
                    self.expand(parent, PAGE, stop).await?;
                }
                let more = {
                    let state = self.state.lock().await;
                    let Some(children) = state.session.children(parent) else {
                        break;
                    };
                    for &child in &children.ids[scanned.min(children.ids.len())..] {
                        budget = budget.saturating_sub(1);
                        let (rank, has_children, leads) = Self::reach(&state, child, lineage);
                        let mut down = way.clone();
                        down.push(child);
                        if rank.is_some() {
                            return Ok(Some(down));
                        }
                        if has_children && leads {
                            queue.push_back((child, down));
                        }
                    }
                    scanned = children.ids.len();
                    children.state == ChildState::More && scanned <= LOCATE_SCAN
                };
                if !more {
                    break;
                }
                self.state.lock().await.session.expand_more(parent, PAGE);
                self.drive(parent, stop, false).await?;
            }
        }
        Ok(None)
    }
}

impl DissectionState {
    fn span(&mut self, span: Span) -> DissectSpan {
        self.sources.insert(span.source.index(), span.source);
        span.into()
    }

    fn source(&self, index: u32) -> Result<SourceId, Error> {
        self.sources
            .get(&index)
            .copied()
            .ok_or_else(|| Error::Custom(format!("no source {index}")))
    }

    fn key(&mut self, id: NodeId) -> u32 {
        if let Some(key) = self.keys.get(&id) {
            return *key;
        }
        let key = u32::try_from(self.ids.len()).unwrap_or(u32::MAX);
        self.ids.push(id);
        self.keys.insert(id, key);
        key
    }

    fn id(&self, key: u32) -> Result<NodeId, Error> {
        self.ids
            .get(key as usize)
            .copied()
            .ok_or_else(|| Error::Custom(format!("no node {key}")))
    }

    fn node(&mut self, id: NodeId) -> DissectNode {
        let key = self.key(id);
        let Some(node) = self.session.node(id) else {
            return DissectNode::gone(key);
        };
        let spans: Vec<Span> = [node.span, node.target]
            .into_iter()
            .flatten()
            .chain(node.diagnostics.iter().filter_map(|d| d.span))
            .collect();
        let node = DissectNode {
            key,
            name: node.name.to_string(),
            value: node.value.as_ref().map(DissectValue::from),
            summary: node.summary.clone(),
            description: node.description.as_ref().map(|d| d.to_string()),
            span: node.span.map(DissectSpan::from),
            target: node.target.map(DissectSpan::from),
            diagnostics: node
                .diagnostics
                .iter()
                .map(DissectDiagnostic::from)
                .collect(),
            interpretation: self
                .session
                .interpretation(id)
                .map(|i| DissectInterpretation {
                    format: i.format.map(|f| DissectFormatName {
                        name: f.name,
                        title: f.title,
                    }),
                    forced: i.forced,
                }),
            children: match self.session.children(id) {
                Some(c) => DissectChildren {
                    state: c.state.into(),
                    loaded: u32::try_from(c.ids.len()).unwrap_or(u32::MAX),
                    count: match c.count {
                        Count::Exact(n) => Some(DissectCount { n, at_least: false }),
                        Count::AtLeast(n) => Some(DissectCount { n, at_least: true }),
                        Count::Unknown => None,
                    },
                    error: c.error.map(DissectDiagnostic::from),
                },
                None => DissectChildren::leaf(),
            },
        };
        for span in spans {
            self.span(span);
        }
        if let Some(error) = self
            .session
            .children(id)
            .and_then(|c| c.error.and_then(|e| e.span))
        {
            self.span(error);
        }
        node
    }

    /// The children of `id` past the first `sent`, with the index of the
    /// first of them; `sent` then counts them too.
    fn added(&mut self, id: NodeId, sent: &mut usize) -> Option<(u32, Vec<DissectNode>)> {
        let ids: Vec<NodeId> = self.session.children(id)?.ids.get(*sent..)?.to_vec();
        if ids.is_empty() {
            return None;
        }
        let first = u32::try_from(*sent).unwrap_or(u32::MAX);
        *sent += ids.len();
        Some((
            first,
            ids.into_iter().map(|child| self.node(child)).collect(),
        ))
    }

    fn page(&mut self, id: NodeId) -> DissectPage {
        let ids: Vec<NodeId> = self
            .session
            .children(id)
            .map(|c| c.ids.to_vec())
            .unwrap_or_default();
        DissectPage {
            parent: self.node(id),
            nodes: ids.into_iter().map(|child| self.node(child)).collect(),
            secret: self.secret(),
        }
    }

    /// The first password an expansion is waiting for.
    fn secret(&self) -> Option<DissectSecret> {
        self.secrets.first().map(|request| DissectSecret {
            index: 0,
            prompt: request.prompt.clone(),
            attempt: request.attempt,
        })
    }

    /// The name of the file `id` holds, whose extension suggests formats:
    /// the nearest name with one, from `id` up (a member's data is called
    /// "Content", the member by its file name).
    fn content_name(&self, id: NodeId) -> String {
        let mut cursor = Some(id);
        while let Some(id) = cursor {
            let name = self.session.node(id).map(|n| n.name.as_ref());
            if let Some(name) = name.filter(|n| n.contains('.')) {
                return name.to_string();
            }
            cursor = self.session.parent(id);
        }
        self.session
            .node(id)
            .map(|n| n.name.to_string())
            .unwrap_or_default()
    }
}

// --- Wire types ---

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectNode {
    key: u32,
    name: String,
    value: Option<DissectValue>,
    summary: Option<String>,
    description: Option<String>,
    span: Option<DissectSpan>,
    /// What the field points to (an offset field's target).
    target: Option<DissectSpan>,
    diagnostics: Vec<DissectDiagnostic>,
    /// What dissects the node's content, for one holding a file within the
    /// file; known once it has been expanded.
    interpretation: Option<DissectInterpretation>,
    children: DissectChildren,
}

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectInterpretation {
    /// `None` when the content wasn't recognised.
    format: Option<DissectFormatName>,
    /// The format was chosen with Dissect As.
    forced: bool,
}

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectFormatName {
    name: &'static str,
    title: &'static str,
}

impl DissectNode {
    fn gone(key: u32) -> Self {
        DissectNode {
            key,
            name: String::new(),
            value: None,
            summary: None,
            description: None,
            span: None,
            target: None,
            diagnostics: Vec::new(),
            interpretation: None,
            children: DissectChildren::leaf(),
        }
    }
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DissectChildState {
    Leaf,
    /// Not expanded yet.
    Unloaded,
    /// The expansion waits for a password.
    Locked,
    /// More children follow those loaded.
    More,
    /// Its expansion was stopped part-way; asking for its children again
    /// continues it.
    Stopped,
    Complete,
    /// The expansion failed; children produced before it remain.
    Failed,
}

impl From<ChildState> for DissectChildState {
    fn from(state: ChildState) -> Self {
        match state {
            ChildState::Leaf => Self::Leaf,
            ChildState::NotRequested => Self::Unloaded,
            ChildState::Running(Wait::Secret) => Self::Locked,
            ChildState::More => Self::More,
            // A finished request leaves no expansion running, so one still
            // running was stopped (or is being run by another request).
            ChildState::Running(_) => Self::Stopped,
            ChildState::Complete => Self::Complete,
            ChildState::Failed => Self::Failed,
        }
    }
}

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectChildren {
    state: DissectChildState,
    loaded: u32,
    count: Option<DissectCount>,
    error: Option<DissectDiagnostic>,
}

impl DissectChildren {
    fn leaf() -> Self {
        DissectChildren {
            state: DissectChildState::Leaf,
            loaded: 0,
            count: None,
            error: None,
        }
    }
}

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectCount {
    n: u64,
    at_least: bool,
}

#[derive(Serialize, specta::Type, Clone, Copy)]
pub struct DissectSpan {
    /// The byte space: the file, or a stream decoded or reassembled from it.
    source: u32,
    offset: u64,
    len: u64,
}

impl From<Span> for DissectSpan {
    fn from(span: Span) -> Self {
        DissectSpan {
            source: span.source.index(),
            offset: span.offset,
            len: span.len,
        }
    }
}

#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DissectDiagnosticKind {
    Truncated,
    Malformed,
    Unsupported,
    Limit,
    Warning,
    Note,
}

#[derive(Serialize, specta::Type, Clone)]
pub struct DissectDiagnostic {
    kind: DissectDiagnosticKind,
    message: String,
    span: Option<DissectSpan>,
}

impl From<&Diagnostic> for DissectDiagnostic {
    fn from(d: &Diagnostic) -> Self {
        use DissectDiagnosticKind as K;
        DissectDiagnostic {
            kind: match d.kind {
                DiagKind::Truncated => K::Truncated,
                DiagKind::Malformed => K::Malformed,
                DiagKind::Unsupported => K::Unsupported,
                DiagKind::Limit => K::Limit,
                DiagKind::Warning => K::Warning,
                // `Internal` and anything added later read as a note.
                _ => K::Note,
            },
            message: d.message.clone(),
            span: d.span.map(DissectSpan::from),
        }
    }
}

#[derive(Serialize, specta::Type)]
pub struct DissectPage {
    parent: DissectNode,
    /// Every child loaded so far.
    nodes: Vec<DissectNode>,
    /// A password the dissection waits for, to answer with
    /// `dissect_answer_secret` and then ask again.
    secret: Option<DissectSecret>,
}

#[derive(Serialize, specta::Type)]
pub struct DissectSecret {
    index: u32,
    /// What it unlocks, e.g. "Password for the encrypted ZIP entries".
    prompt: String,
    /// 0 at first; more after a wrong password.
    attempt: u32,
}

#[derive(Serialize, specta::Type)]
pub struct DissectOpened {
    id: u64,
    page: DissectPage,
}

/// How far a node's expansion has got, sent to the window as
/// `dissect-progress` while it runs.
#[derive(Serialize, specta::Type, Clone)]
pub struct DissectProgress {
    /// The dissection.
    id: u64,
    node: u32,
    done: u64,
    total: u64,
    /// The codec of the innermost stream the expansion waits on decoding,
    /// when it does ("zstd", "deflate", ...).
    codec: Option<&'static str>,
}

/// Work on the dissection's bytes rather than a node's children.
#[derive(Serialize, specta::Type, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DissectTask {
    /// Decoding a stream up to a read (the hex pane, a copy): the measure
    /// is the innermost stream's that has one.
    Decode,
    /// Writing a span to a file: bytes written of the span.
    Save,
}

/// How far a task has got, sent to the window as `dissect-task-progress`
/// while it runs.
#[derive(Serialize, specta::Type, Clone)]
pub struct DissectTaskProgress {
    /// The dissection.
    id: u64,
    task: DissectTask,
    done: u64,
    total: u64,
    /// As [`DissectProgress::codec`].
    codec: Option<&'static str>,
}

/// Children a node's expansion produced since it last sent some, sent to the
/// window as `dissect-children` while it runs.
#[derive(Serialize, specta::Type, Clone)]
pub struct DissectChildrenAdded {
    /// The dissection.
    id: u64,
    node: u32,
    /// The index of the first among the node's children.
    first: u32,
    nodes: Vec<DissectNode>,
}

/// A format chosen for the file (an empty path) or for a file within it,
/// by the child indices down to it.
#[derive(Serialize, serde::Deserialize, specta::Type, Clone, Debug, PartialEq)]
pub struct DissectChoice {
    path: Vec<u64>,
    format: String,
}

/// After a reinterpretation: the way down to the node and its children, and
/// every choice now in effect, for the viewer to keep.
#[derive(Serialize, specta::Type)]
pub struct DissectReinterpreted {
    located: DissectLocated,
    choices: Vec<DissectChoice>,
}

#[derive(Serialize, specta::Type)]
pub struct DissectContent {
    node: DissectNode,
    /// The file's name, for suggesting formats by its extension.
    name: String,
}

/// A format the file can be dissected as, chosen instead of identified.
#[derive(Serialize, specta::Type)]
pub struct DissectFormat {
    name: &'static str,
    title: &'static str,
    extensions: Vec<&'static str>,
    /// It lists the file's extension.
    suggested: bool,
}

#[derive(Serialize, specta::Type)]
pub struct DissectSource {
    /// An upper bound until `len_known`.
    len: u64,
    /// Whether `len` is the real length: not for a stream decoded on demand
    /// whose size nothing records, until it has been decoded to its end.
    len_known: bool,
    /// Where a decoded or reassembled stream came from (`None` for the
    /// file itself).
    origin: Option<DissectOrigin>,
}

#[derive(Serialize, specta::Type)]
pub struct DissectOrigin {
    parent: DissectSpan,
    transform: String,
}

#[derive(Serialize, specta::Type)]
pub struct DissectLocated {
    /// Keys from the root down to the node found.
    path: Vec<u32>,
    /// The children of each node on the path: all but the last when
    /// locating a byte, all of them after a reinterpretation.
    pages: Vec<DissectPage>,
    /// A password that stopped the way down; see `DissectPage::secret`.
    secret: Option<DissectSecret>,
}

// --- Commands ---

/// What the dissector identifies the file at `path`, `len` bytes named
/// `name`, as: the same answer its tree's root gets, reading only what
/// identification asks for.
pub async fn identify(
    fs: &dyn Filesystem,
    path: &VfsPath,
    len: u64,
    name: &str,
) -> Result<formats::Identification, Error> {
    let mut identify = formats::Identify::new(len, Some(name));
    loop {
        match identify.poll() {
            formats::Identified::NeedBytes(ranges) => {
                let chunks = futures::future::try_join_all(
                    ranges
                        .iter()
                        .map(|r| fs.read_range(path.clone(), r.offset, r.len)),
                )
                .await?;
                for (range, chunk) in ranges.iter().zip(chunks) {
                    identify.provide(range.offset, &chunk.data);
                }
            }
            formats::Identified::Done(found) => return Ok(found),
        }
    }
}

/// Every format, those that list the extension of the file `name` first.
#[tauri::command]
#[specta::specta]
pub fn dissect_formats(name: String) -> Vec<DissectFormat> {
    let suggested = name
        .rsplit_once('.')
        .map(|(_, extension)| formats::by_extension(extension))
        .unwrap_or_default();
    let mut rest: Vec<_> = formats::FORMATS
        .iter()
        .filter(|f| !suggested.iter().any(|s| s.name == f.name))
        .collect();
    rest.sort_by_cached_key(|f| f.title.to_lowercase());
    let entry = |f: &formats::Format, suggested| DissectFormat {
        name: f.name,
        title: f.title,
        extensions: f.extensions.to_vec(),
        suggested,
    };
    suggested
        .iter()
        .map(|f| entry(f, true))
        .chain(rest.into_iter().map(|f| entry(f, false)))
        .collect()
}

/// Start dissecting `path`, with the formats chosen for it or for files
/// within it. Nothing is read yet: the root's children come from
/// `dissect_children`.
#[tauri::command]
#[specta::specta]
pub async fn dissect_open(
    ctx: MainWindowContext,
    window: tauri::WebviewWindow,
    global_ctx: State<'_, GlobalContext>,
    path: VfsPath,
    choices: Vec<DissectChoice>,
) -> Result<DissectOpened, Error> {
    let choices = choices
        .into_iter()
        .map(|c| Ok((c.path, format_named(&c.format)?)))
        .collect::<Result<Vec<_>, Error>>()?;
    let fs = ctx.fs()?;
    let len = fs.file_info(path.clone(), true).await?.size.unwrap_or(0);
    let reader = fs.open_read_at(path.clone()).await?;
    let name = path.file_name().unwrap_or_default().to_string();
    let dissections = &global_ctx.dissections;
    let id = dissections.next.fetch_add(1, Ordering::Relaxed);
    let label = window.label().to_string();
    let dissection = Dissection::open(id, Some(window), reader, name, len, &choices);
    let page = dissection.state.lock().await.page(dissection.root);
    dissections
        .open
        .lock()
        .insert(id, (label, Arc::new(dissection)));
    Ok(DissectOpened { id, page })
}

/// End a dissection, stopping what it's doing.
#[tauri::command]
#[specta::specta]
pub fn dissect_close(global_ctx: State<'_, GlobalContext>, id: u64) -> Result<(), Error> {
    if let Some((_, dissection)) = global_ctx.dissections.open.lock().remove(&id) {
        dissection.stop(None);
    }
    Ok(())
}

/// Stop what the requests about `node` are doing, or with `None`, every
/// request so far. A stopped expansion keeps what it produced; its request
/// returns that.
#[tauri::command]
#[specta::specta]
pub fn dissect_stop(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    node: Option<u32>,
) -> Result<(), Error> {
    global_ctx.dissections.get(id)?.stop(node);
    Ok(())
}

/// The children of `node`, at least `at_least` of them when there are that
/// many (fewer if stopped).
#[tauri::command]
#[specta::specta]
pub async fn dissect_children(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    node: u32,
    at_least: u64,
) -> Result<DissectPage, Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let stop = dissection.token(Some(node));
    let node = dissection.state.lock().await.id(node)?;
    dissection.expand_shown(node, at_least, &stop).await?;
    Ok(dissection.state.lock().await.page(node))
}

/// The node whose format Dissect As chooses when `node` is selected (see
/// `Dissection::content_of`); `None` when it doesn't hold a file.
#[tauri::command]
#[specta::specta]
pub async fn dissect_content_of(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    node: u32,
) -> Result<Option<DissectContent>, Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let stop = dissection.token(Some(node));
    let node = dissection.state.lock().await.id(node)?;
    let Some(content) = dissection.content_of(node, &stop).await? else {
        return Ok(None);
    };
    let mut state = dissection.state.lock().await;
    Ok(Some(DissectContent {
        name: state.content_name(content),
        node: state.node(content),
    }))
}

/// Dissect `node`'s content as `format` (`None` identifies it again), with
/// the children on the way down to it and its own first ones. What was
/// below it is gone, choices included.
#[tauri::command]
#[specta::specta]
pub async fn dissect_reinterpret(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    node: u32,
    format: Option<String>,
) -> Result<DissectReinterpreted, Error> {
    let format = format.map(|name| format_named(&name)).transpose()?;
    let dissection = global_ctx.dissections.get(id)?;
    let stop = dissection.token(Some(node));
    let node = {
        let mut state = dissection.state.lock().await;
        let node = state.id(node)?;
        state.session.reinterpret(node, format);
        node
    };
    dissection.expand(node, PAGE, &stop).await?;
    let mut state = dissection.state.lock().await;
    let mut path = vec![node];
    while let Some(parent) = state.session.parent(path[path.len() - 1]) {
        path.push(parent);
    }
    path.reverse();
    let pages = path.iter().map(|&id| state.page(id)).collect();
    let choices = state
        .session
        .reinterpretations(dissection.root)
        .into_iter()
        .map(|(path, format)| DissectChoice {
            path,
            format: format.name.to_string(),
        })
        .collect();
    Ok(DissectReinterpreted {
        located: DissectLocated {
            path: path.into_iter().map(|id| state.key(id)).collect(),
            pages,
            secret: state.secret(),
        },
        choices,
    })
}

fn format_named(name: &str) -> Result<&'static formats::Format, Error> {
    formats::by_name(name).ok_or_else(|| Error::Custom(format!("Unknown format {name}")))
}

/// The innermost node holding byte `offset` of `source`, with the children
/// on the way down to it; as far as it got if stopped.
#[tauri::command]
#[specta::specta]
pub async fn dissect_locate(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    source: u32,
    offset: u64,
) -> Result<DissectLocated, Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let stop = dissection.token(None);
    let source = dissection.state.lock().await.source(source)?;
    let path = dissection.locate(source, offset, &stop).await?;
    let mut state = dissection.state.lock().await;
    let pages = path[..path.len().saturating_sub(1)]
        .iter()
        .map(|&id| state.page(id))
        .collect();
    Ok(DissectLocated {
        path: path.into_iter().map(|id| state.key(id)).collect(),
        pages,
        secret: state.secret(),
    })
}

/// Answer a password request (`None` declines it). The expansion waiting for
/// it resumes when its node is asked for again.
#[tauri::command]
#[specta::specta]
pub async fn dissect_answer_secret(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    index: u32,
    password: Option<String>,
) -> Result<(), Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let mut state = dissection.state.lock().await;
    let Some(request) = state.secrets.get(index as usize).cloned() else {
        return Ok(());
    };
    state
        .session
        .answer_secret(&request, password.as_deref().map(Secret::password));
    state.secrets.remove(index as usize);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn dissect_source(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    source: u32,
) -> Result<DissectSource, Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let mut state = dissection.state.lock().await;
    let source = state.source(source)?;
    let origin = state.session.origin(source);
    Ok(DissectSource {
        len: state.session.source_len(source),
        len_known: state.session.source_len_known(source),
        origin: origin.map(|o| DissectOrigin {
            parent: state.span(o.parent),
            transform: o.transform.to_string(),
        }),
    })
}

/// Bytes of any source of the dissection, for the hex pane.
#[tauri::command]
#[specta::specta]
pub async fn dissect_read(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    source: u32,
    offset: u64,
    len: u64,
) -> Result<RawBytes, Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let stop = dissection.token(None);
    let span = {
        let state = dissection.state.lock().await;
        let source = state.source(source)?;
        let len = len.min(state.session.source_len(source).saturating_sub(offset));
        Span::new(source, offset, len)
    };
    Ok(RawBytes(dissection.read(span, &stop).await?))
}

/// Copy the bytes of a span to the clipboard.
#[tauri::command]
#[specta::specta]
pub async fn dissect_copy(
    ctx: MainWindowContext,
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    source: u32,
    offset: u64,
    len: u64,
    format: CopyFormat,
) -> Result<(), Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let stop = dissection.token(None);
    let source = dissection.state.lock().await.source(source)?;
    let bytes = dissection
        .read(Span::new(source, offset, len), &stop)
        .await?;
    let text = super::format_bytes(&bytes, format);
    ctx.clipboard().set_text(text).await?;
    Ok(())
}

/// Write the bytes of a span to `path`, a file on this machine (chosen in
/// the save dialog, so a remote session's span lands here too). A save that
/// is stopped or fails leaves no partial file.
#[tauri::command]
#[specta::specta]
pub async fn dissect_save(
    global_ctx: State<'_, GlobalContext>,
    id: u64,
    source: u32,
    offset: u64,
    len: u64,
    path: String,
) -> Result<(), Error> {
    let dissection = global_ctx.dissections.get(id)?;
    let stop = dissection.token(None);
    let source = dissection.state.lock().await.source(source)?;
    let saved = save_span(&dissection, Span::new(source, offset, len), &path, &stop).await;
    if saved.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    saved
}

async fn save_span(
    dissection: &Dissection,
    span: Span,
    path: &str,
    stop: &CancellationToken,
) -> Result<(), Error> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path).await?;
    let mut at = span.offset;
    let started = Instant::now();
    let mut reported = started;
    while at < span.end() {
        if started.elapsed() >= PROGRESS_EVERY && reported.elapsed() >= PROGRESS_EVERY {
            reported = Instant::now();
            dissection.report_task(DissectTask::Save, at - span.offset, span.len, None);
        }
        let len = (span.end() - at).min(SAVE_CHUNK);
        let bytes = dissection
            .read(Span::new(span.source, at, len), stop)
            .await?;
        // Short at the end of a stream whose length was only a bound.
        if bytes.is_empty() {
            break;
        }
        file.write_all(&bytes).await?;
        at += bytes.len() as u64;
    }
    file.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(Vec<u8>);

    #[async_trait::async_trait]
    impl VfsRandomReader for Bytes {
        async fn read_at(
            &mut self,
            offset: u64,
            len: u64,
        ) -> Result<Vec<u8>, newt_common::error::Error> {
            let start = (offset as usize).min(self.0.len());
            let end = (offset.saturating_add(len) as usize).min(self.0.len());
            Ok(self.0[start..end].to_vec())
        }
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            }
        }
        !crc
    }

    /// A ZIP holding `data` as `name`: stored, or deflated (as stored
    /// deflate blocks).
    fn zip(name: &str, data: &[u8], deflate: bool) -> Vec<u8> {
        let (crc, size, name_len) = (crc32(data), data.len() as u32, name.len() as u16);
        let (method, packed) = if deflate {
            let mut blocks = Vec::new();
            let mut chunks = data.chunks(0xFFFF).peekable();
            while let Some(chunk) = chunks.next() {
                let len = chunk.len() as u16;
                blocks.push(u8::from(chunks.peek().is_none()));
                blocks.extend(len.to_le_bytes());
                blocks.extend((!len).to_le_bytes());
                blocks.extend(chunk);
            }
            (8u16, blocks)
        } else {
            (0u16, data.to_vec())
        };
        let packed_size = packed.len() as u32;
        let mut out = Vec::new();
        out.extend(b"PK\x03\x04");
        out.extend([20, 0, 0, 0]);
        out.extend(method.to_le_bytes());
        out.extend([0, 0, 0, 0]);
        out.extend(crc.to_le_bytes());
        out.extend(packed_size.to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(name.as_bytes());
        out.extend(&packed);
        let directory = out.len() as u32;
        out.extend(b"PK\x01\x02");
        out.extend([20, 0, 20, 0, 0, 0]);
        out.extend(method.to_le_bytes());
        out.extend([0, 0, 0, 0]);
        out.extend(crc.to_le_bytes());
        out.extend(packed_size.to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend([0; 12]);
        out.extend(0u32.to_le_bytes());
        out.extend(name.as_bytes());
        let directory_len = out.len() as u32 - directory;
        out.extend(b"PK\x05\x06");
        out.extend([0, 0, 0, 0, 1, 0, 1, 0]);
        out.extend(directory_len.to_le_bytes());
        out.extend(directory.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    /// A dissection of `data` with the root's first children.
    /// One zstd frame of raw blocks that, like one written to a pipe,
    /// doesn't record its content size.
    fn zstd(data: &[u8]) -> Vec<u8> {
        // Magic; no content size, checksum or dictionary; a 128 KiB window.
        let mut out = vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 0x38];
        let mut chunks = data.chunks(0x10000).peekable();
        while let Some(chunk) = chunks.next() {
            let header = u32::from(chunks.peek().is_none()) | ((chunk.len() as u32) << 3);
            out.extend(&header.to_le_bytes()[..3]);
            out.extend(chunk);
        }
        out
    }

    async fn open(data: Vec<u8>, format: Option<&str>) -> Dissection {
        let len = data.len() as u64;
        let choices: Vec<_> = format
            .map(|name| (Vec::new(), format_named(name).unwrap()))
            .into_iter()
            .collect();
        let dissection = Dissection::open(
            0,
            None,
            Box::new(Bytes(data)),
            "test.zip".into(),
            len,
            &choices,
        );
        let root = dissection.root;
        dissection.expand(root, PAGE, &go()).await.unwrap();
        dissection
    }

    fn go() -> CancellationToken {
        CancellationToken::new()
    }

    /// The node called `name` among the loaded nodes below `id`, expanding
    /// along the way.
    async fn find(dissection: &Dissection, id: NodeId, name: &str) -> Option<NodeId> {
        let mut queue = vec![id];
        while let Some(id) = queue.pop() {
            dissection.expand(id, PAGE, &go()).await.unwrap();
            let state = dissection.state.lock().await;
            for &child in state.session.children(id).map_or(&[][..], |c| c.ids) {
                if state.session.node(child).is_some_and(|n| n.name == name) {
                    return Some(child);
                }
                queue.push(child);
            }
        }
        None
    }

    async fn forced(dissection: &Dissection, id: NodeId) -> Option<&'static str> {
        dissection
            .state
            .lock()
            .await
            .session
            .interpretation(id)
            .filter(|i| i.forced)
            .and_then(|i| i.format)
            .map(|f| f.name)
    }

    async fn child_state(dissection: &Dissection, id: NodeId) -> (DissectChildState, usize) {
        let state = dissection.state.lock().await;
        let children = state.session.children(id).unwrap();
        (children.state.into(), children.ids.len())
    }

    /// Records the dissection the Structure scene shows
    /// (`docs/scenes/library/structure-viewer.ts`): a pcap inside a ZIP,
    /// expanded down to the first TCP header. Run with
    /// `cargo test -p newt -- --ignored record_structure_scene`.
    #[tokio::test]
    #[ignore]
    async fn record_structure_scene() {
        let fixtures =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/scenes/fixtures");
        let data = std::fs::read(fixtures.join("captures.zip")).unwrap();
        let dissection = Dissection::open(
            0,
            None,
            Box::new(Bytes(data.clone())),
            "edge-2026-10-08.zip".into(),
            data.len() as u64,
            &[],
        );
        let mut path = vec![dissection.root];
        let way = [
            "Central Directory",
            "edge-01/handshake.pcap",
            "Content",
            "Packet 0",
            "TCP",
        ];
        for name in way {
            let parent = path[path.len() - 1];
            dissection.expand(parent, PAGE, &go()).await.unwrap();
            let state = dissection.state.lock().await;
            let child = state
                .session
                .children(parent)
                .unwrap()
                .ids
                .iter()
                .copied()
                .find(|&id| state.session.node(id).is_some_and(|n| n.name == name))
                .unwrap_or_else(|| panic!("no {name}"));
            path.push(child);
        }
        let tcp = path[path.len() - 1];
        dissection.expand(tcp, PAGE, &go()).await.unwrap();
        let mut state = dissection.state.lock().await;
        let pages: Vec<_> = path.iter().map(|&id| state.page(id)).collect();
        let keys: Vec<u32> = path.iter().map(|&id| state.key(id)).collect();
        let offset = state.session.node(tcp).unwrap().span.unwrap().offset;
        let recording = serde_json::json!({ "offset": offset, "path": keys, "pages": pages });
        std::fs::write(
            fixtures.join("captures.dissection.json"),
            serde_json::to_string_pretty(&recording).unwrap() + "\n",
        )
        .unwrap();
    }

    async fn names(dissection: &Dissection, path: &[NodeId]) -> Vec<String> {
        let state = dissection.state.lock().await;
        path.iter()
            .map(|&id| state.session.node(id).unwrap().name.to_string())
            .collect()
    }

    #[tokio::test]
    async fn locating_a_byte_of_a_member_reaches_past_the_central_directory() {
        let data = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../docs/scenes/fixtures/captures.zip"),
        )
        .unwrap();
        // The first segment's source port, 51820.
        let at = data
            .windows(4)
            .position(|w| w == [0xca, 0x6c, 0x01, 0xbb])
            .unwrap() as u64;
        let dissection = open(data, None).await;
        let path = dissection.locate(dissection.file, at, &go()).await.unwrap();
        assert_eq!(
            names(&dissection, &path).await[1..],
            [
                "Central Directory",
                "edge-01/handshake.pcap",
                "Content",
                "Packet 0",
                "TCP",
                "Source port"
            ]
        );
    }

    #[tokio::test]
    async fn locating_a_byte_of_a_decoded_stream_stays_in_it() {
        let text = "café au lait\n".repeat(500).into_bytes();
        let dissection = open(zip("notes.txt", &text, true), None).await;
        let member = find(&dissection, dissection.root, "notes.txt")
            .await
            .unwrap();
        let content = dissection.content_of(member, &go()).await.unwrap().unwrap();
        dissection.expand(content, PAGE, &go()).await.unwrap();
        let (stream, packed) = {
            let state = dissection.state.lock().await;
            let stream = state
                .session
                .children(content)
                .unwrap()
                .ids
                .iter()
                .find_map(|&id| state.session.node(id).unwrap().span)
                .unwrap()
                .source;
            let packed = state.session.node(content).unwrap().span.unwrap();
            (stream, packed)
        };
        assert_ne!(stream, dissection.file);

        // A byte of the stream: a field of the stream holds it.
        let path = dissection.locate(stream, 1000, &go()).await.unwrap();
        assert!(path.contains(&content));
        let state = dissection.state.lock().await;
        let last = state
            .session
            .node(path[path.len() - 1])
            .unwrap()
            .span
            .unwrap();
        assert!(last.source == stream && last.contains(&Span::new(stream, 1000, 1)));
        drop(state);

        // A byte of the compressed data: nothing below Content holds it.
        let path = dissection
            .locate(dissection.file, packed.offset + 10, &go())
            .await
            .unwrap();
        assert_eq!(path[path.len() - 1], content);
    }

    #[tokio::test]
    async fn lazily_decompressed_bytes_can_be_read() {
        // Past fillyfoal's threshold for decoding on demand.
        let text = b"<?xml version=\"1.0\"?><model unit=\"millimeter\"/>".repeat(30_000);
        let dissection = open(zip("3D/object.model", &text, true), None).await;
        let member = find(&dissection, dissection.root, "3D/object.model")
            .await
            .unwrap();
        let content = dissection.content_of(member, &go()).await.unwrap().unwrap();
        dissection.expand(content, PAGE, &go()).await.unwrap();
        // The content's fields lie in the decompressed stream.
        let source = {
            let state = dissection.state.lock().await;
            let field = state.session.children(content).unwrap().ids[0];
            state.session.node(field).unwrap().span.unwrap().source
        };
        assert_ne!(source, dissection.file);
        let len = {
            let state = dissection.state.lock().await;
            assert!(state.session.derived_data(source).is_none());
            state.session.source_len(source)
        };
        let whole = Span::new(source, 0, len);
        assert_eq!(dissection.read(whole, &go()).await.unwrap(), text);
        let middle = Span::new(source, 100, 50);
        assert_eq!(
            dissection.read(middle, &go()).await.unwrap(),
            &text[100..150]
        );

        let stopped = go();
        stopped.cancel();
        assert!(matches!(
            dissection.read(whole, &stopped).await,
            Err(Error::Cancelled)
        ));

        // Saved in steps, larger than one.
        let path = std::env::temp_dir().join(format!("newt-save-{}.xml", std::process::id()));
        let path = path.to_str().unwrap();
        save_span(&dissection, whole, path, &go()).await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), text);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn children_are_sent_once_each_from_where_the_last_send_ended() {
        let dissection = open(zip("song.gpx", b"hello", false), None).await;
        let root = dissection.root;
        let mut state = dissection.state.lock().await;
        let ids = state.session.children(root).unwrap().ids.to_vec();
        assert!(ids.len() > 1);
        let keys = |state: &mut DissectionState, ids: &[NodeId]| -> Vec<u32> {
            ids.iter().map(|&id| state.key(id)).collect()
        };

        let mut sent = 1;
        let (first, nodes) = state.added(root, &mut sent).unwrap();
        assert_eq!(first, 1);
        assert_eq!(
            nodes.iter().map(|n| n.key).collect::<Vec<_>>(),
            keys(&mut state, &ids[1..])
        );
        assert_eq!(sent, ids.len());
        assert!(state.added(root, &mut sent).is_none());

        // A list that shrank under it (a reinterpretation) sends nothing.
        let mut past = ids.len() + 1;
        assert!(state.added(root, &mut past).is_none());
    }

    #[tokio::test]
    async fn a_stopped_expansion_keeps_its_place_and_continues() {
        let dissection = open(zip("song.gpx", b"hello", false), None).await;
        let member = find(&dissection, dissection.root, "song.gpx")
            .await
            .unwrap();
        let stopped = go();
        stopped.cancel();
        dissection.expand(member, PAGE, &stopped).await.unwrap();
        assert!(matches!(
            child_state(&dissection, member).await,
            (DissectChildState::Stopped, 0)
        ));

        dissection.expand(member, PAGE, &go()).await.unwrap();
        let (state, loaded) = child_state(&dissection, member).await;
        assert!(matches!(state, DissectChildState::Complete));
        assert!(loaded > 0);
    }

    #[tokio::test]
    async fn an_unsized_stream_learns_its_length_at_its_end() {
        // Plain text whose first bytes would pass for a length prefix if
        // the provisional length were taken for the real one.
        let text = b"hello world\n".repeat(20_000);
        let dissection = open(zstd(&text), None).await;
        let root = dissection.root;
        let children: Vec<NodeId> = dissection
            .state
            .lock()
            .await
            .session
            .children(root)
            .unwrap()
            .ids
            .to_vec();
        let mut content = None;
        for child in children {
            if dissection.holds_content(child, &go()).await.unwrap() {
                content = Some(child);
            }
        }
        let content = content.expect("the decompressed content");
        dissection.expand(content, PAGE, &go()).await.unwrap();
        let source = {
            let state = dissection.state.lock().await;
            let children = state.session.children(content).unwrap().ids;
            children
                .iter()
                .find_map(|&id| state.session.node(id).unwrap().span)
                .unwrap()
                .source
        };
        let bound = {
            let state = dissection.state.lock().await;
            assert!(!state.session.source_len_known(source));
            state.session.source_len(source)
        };
        assert!(bound > text.len() as u64);

        // Past the real end, a read comes back short, and the length is known.
        let end = text.len() as u64;
        let tail = dissection
            .read(Span::new(source, end - 10, 100), &go())
            .await
            .unwrap();
        assert_eq!(tail, &text[text.len() - 10..]);
        let state = dissection.state.lock().await;
        assert!(state.session.source_len_known(source));
        assert_eq!(state.session.source_len(source), end);
    }

    #[tokio::test]
    async fn choices_inside_the_file_carry_over_to_a_new_dissection() {
        let data = zip("song.gpx", b"hello", false);
        let first = open(data.clone(), None).await;
        let member = find(&first, first.root, "song.gpx").await.unwrap();
        let content = first.content_of(member, &go()).await.unwrap().unwrap();
        let format = format_named("guitar-pro-6").unwrap();
        let choices = {
            let mut state = first.state.lock().await;
            state.session.reinterpret(content, Some(format));
            state.session.reinterpretations(first.root)
        };
        assert_eq!(choices.len(), 1);

        let again = Dissection::open(
            1,
            None,
            Box::new(Bytes(data.clone())),
            "test.zip".into(),
            data.len() as u64,
            &choices,
        );
        let member = find(&again, again.root, "song.gpx").await.unwrap();
        let content = again.content_of(member, &go()).await.unwrap().unwrap();
        assert_eq!(forced(&again, content).await, Some("guitar-pro-6"));
        assert_eq!(forced(&again, again.root).await, None);
    }

    #[test]
    fn stopping_a_node_leaves_the_rest_running() {
        let dissection = Dissection::open(0, None, Box::new(Bytes(Vec::new())), "x".into(), 0, &[]);
        let (one, two, other) = (
            dissection.token(Some(1)),
            dissection.token(Some(2)),
            dissection.token(None),
        );
        dissection.stop(Some(1));
        assert!(one.is_cancelled());
        assert!(!two.is_cancelled() && !other.is_cancelled());
        assert!(!dissection.token(Some(1)).is_cancelled());

        dissection.stop(None);
        assert!(two.is_cancelled() && other.is_cancelled());
        assert!(!dissection.token(Some(2)).is_cancelled());
        assert!(!dissection.token(None).is_cancelled());
    }

    #[tokio::test]
    async fn the_root_takes_a_chosen_format() {
        let dissection = open(zip("song.gpx", b"hello", false), Some("guitar-pro-6")).await;
        let root = dissection.root;
        assert_eq!(forced(&dissection, root).await, Some("guitar-pro-6"));
        assert_eq!(
            dissection.content_of(root, &go()).await.unwrap(),
            Some(root)
        );
    }

    #[tokio::test]
    async fn a_member_stands_for_its_data_and_its_headers_for_nothing() {
        let dissection = open(zip("song.gpx", b"hello", false), None).await;
        let root = dissection.root;
        let member = find(&dissection, root, "song.gpx").await.unwrap();
        let content = dissection.content_of(member, &go()).await.unwrap().unwrap();
        assert_ne!(content, member);
        {
            let state = dissection.state.lock().await;
            assert_eq!(state.session.parent(content), Some(member));
            assert_eq!(state.content_name(content), "song.gpx");
        }
        assert_eq!(
            dissection.content_of(content, &go()).await.unwrap(),
            Some(content)
        );

        let format = format_named("guitar-pro-6").unwrap();
        assert!(
            dissection
                .state
                .lock()
                .await
                .session
                .reinterpret(content, Some(format))
        );
        dissection.expand(content, PAGE, &go()).await.unwrap();
        assert_eq!(forced(&dissection, content).await, Some("guitar-pro-6"));
        assert_eq!(forced(&dissection, root).await, None);

        // The member's headers hold no file.
        let header = find(&dissection, member, "Local File Header")
            .await
            .unwrap();
        assert_eq!(dissection.content_of(header, &go()).await.unwrap(), None);
    }
}
