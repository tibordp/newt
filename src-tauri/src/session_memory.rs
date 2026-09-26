//! Per-target memory of where a session's panes were and how its window
//! was placed, restored when a session to the same target opens again.
//!
//! Lives in `sessions.json` rather than `state.json`: none of it is for the
//! frontend, and remote hostnames and paths have no business riding along in
//! every window's runtime-state broadcast. Every access re-reads the file, so
//! several Newt processes sharing a config dir don't clobber each other's
//! entries.

use std::path::{Path, PathBuf};

use log::warn;
use newt_common::vfs::path::PathBuf as VfsPathBuf;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::connections::ConnectionKind;
use crate::main_window::PaneHandle;

const MAX_REMEMBERED_SESSIONS: usize = 32;

/// Offset between a new window and a live one it would otherwise cover.
const CASCADE_STEP: f64 = 24.0;

/// Stable key for what a session is connected to. Spawned targets use
/// `ConnectionKind::identity`, so a saved profile, a recent, and `--target`
/// for the same host share one memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdentity(String);

impl SessionIdentity {
    pub fn local() -> Self {
        Self("local".into())
    }

    pub fn elevated() -> Self {
        Self("elevated".into())
    }

    #[cfg(windows)]
    pub fn wsl(distro: &str) -> Self {
        Self(format!("wsl:{distro}"))
    }

    pub fn of(kind: &ConnectionKind) -> Self {
        Self(kind.identity())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLocation {
    /// On the session's own filesystem (`VfsId::ROOT`).
    pub path: VfsPathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionLocations {
    /// `None` for a pane with nothing restorable (e.g. only ever on S3).
    pub panes: [Option<PaneLocation>; 2],
    pub active_pane: PaneHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    /// Outer top-left in logical pixels; `None` lets the OS place it.
    pub position: Option<(f64, f64)>,
    /// Inner size in logical pixels — the un-maximized size when
    /// `maximized`, so un-maximizing lands somewhere sensible.
    pub size: (f64, f64),
    pub maximized: bool,
}

/// Window bounds outside maximized/fullscreen/minimized, tracked as the
/// window moves because a maximized window only reports the screen's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalBounds {
    pub position: (f64, f64),
    pub size: (f64, f64),
}

/// Where a session's panes open. Resolved per pane at connect.
#[derive(Debug, Clone, Default)]
pub struct SessionStart {
    /// Paths named on the command line, expanded on the session's side.
    pub explicit: [Option<String>; 2],
    pub seed: Seed,
}

#[derive(Debug, Clone, Default)]
pub enum Seed {
    /// The target's remembered locations, if `restore_locations` covers it.
    #[default]
    Remembered,
    /// Handed over directly: a New Window inheriting its opener's panes, or
    /// a reconnect resuming where the session was.
    Given(SessionLocations),
    /// The connection's defaults.
    Default,
}

impl SessionStart {
    pub fn remembered() -> Self {
        Self::default()
    }

    pub fn given(locations: SessionLocations) -> Self {
        Self {
            explicit: [None, None],
            seed: Seed::Given(locations),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildWindow {
    Viewer,
    Editor,
}

/// How a recorded close treats the remembered locations.
pub enum LocationUpdate {
    Set(SessionLocations),
    /// The session never had panes (it failed to connect): keep what was
    /// there so one bad attempt doesn't erase a good memory.
    Keep,
    /// Restoring is off for this target: stop keeping its paths.
    Clear,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct MemoryFile {
    /// Most recently closed first.
    sessions: Vec<SessionEntry>,
    viewer_size: Option<(f64, f64)>,
    editor_size: Option<(f64, f64)>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionEntry {
    identity: String,
    #[serde(default)]
    locations: Option<SessionLocations>,
    #[serde(default)]
    geometry: Option<WindowGeometry>,
}

pub struct SessionMemory {
    path: PathBuf,
    /// Serializes this process's read-modify-write cycles.
    lock: Mutex<()>,
}

impl SessionMemory {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("sessions.json"),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> MemoryFile {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Failed to parse {:?}: {}. Starting afresh.", self.path, e);
                MemoryFile::default()
            }),
            Err(_) => MemoryFile::default(),
        }
    }

    fn update(&self, f: impl FnOnce(&mut MemoryFile)) {
        let _guard = self.lock.lock();
        let mut file = self.load();
        f(&mut file);
        let tmp = self.path.with_extension("json.tmp");
        let written = std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&file).expect("MemoryFile serializes"),
        )
        .and_then(|()| std::fs::rename(&tmp, &self.path));
        if let Err(e) = written {
            warn!("Failed to write {:?}: {}", self.path, e);
        }
    }

    pub fn locations(&self, identity: &SessionIdentity) -> Option<SessionLocations> {
        self.load()
            .sessions
            .into_iter()
            .find(|e| e.identity == identity.0)
            .and_then(|e| e.locations)
    }

    /// The target's own geometry, else the most recently closed window's
    /// size (and maximized state) without its position, so a first session
    /// to a new target still opens at the preferred size.
    pub fn geometry(&self, identity: &SessionIdentity) -> Option<WindowGeometry> {
        let sessions = self.load().sessions;
        if let Some(own) = sessions
            .iter()
            .find(|e| e.identity == identity.0)
            .and_then(|e| e.geometry)
        {
            return Some(own);
        }
        sessions
            .iter()
            .find_map(|e| e.geometry)
            .map(|g| WindowGeometry {
                position: None,
                ..g
            })
    }

    /// Record a closing session, moving its entry to the front.
    pub fn record(
        &self,
        identity: &SessionIdentity,
        locations: LocationUpdate,
        geometry: Option<WindowGeometry>,
    ) {
        self.update(|file| {
            let mut entry = match file.sessions.iter().position(|e| e.identity == identity.0) {
                Some(i) => file.sessions.remove(i),
                None => SessionEntry {
                    identity: identity.0.clone(),
                    locations: None,
                    geometry: None,
                },
            };
            match locations {
                LocationUpdate::Set(l) => entry.locations = Some(l),
                LocationUpdate::Keep => {}
                LocationUpdate::Clear => entry.locations = None,
            }
            if geometry.is_some() {
                entry.geometry = geometry;
            }
            file.sessions.insert(0, entry);
            file.sessions.truncate(MAX_REMEMBERED_SESSIONS);
        });
    }

    pub fn forget(&self, identity: &SessionIdentity) {
        self.update(|file| file.sessions.retain(|e| e.identity != identity.0));
    }

    pub fn child_size(&self, kind: ChildWindow) -> Option<(f64, f64)> {
        let file = self.load();
        match kind {
            ChildWindow::Viewer => file.viewer_size,
            ChildWindow::Editor => file.editor_size,
        }
    }

    pub fn record_child_size(&self, kind: ChildWindow, size: (f64, f64)) {
        self.update(|file| match kind {
            ChildWindow::Viewer => file.viewer_size = Some(size),
            ChildWindow::Editor => file.editor_size = Some(size),
        });
    }
}

/// Whether the window is away from its normal bounds.
fn is_displaced(window: &tauri::Window) -> bool {
    window.is_maximized().unwrap_or(false)
        || window.is_fullscreen().unwrap_or(false)
        || window.is_minimized().unwrap_or(false)
}

fn current_bounds(window: &tauri::Window) -> Option<NormalBounds> {
    let scale = window.scale_factor().ok()?;
    let position = window.outer_position().ok()?.to_logical::<f64>(scale);
    let size = window.inner_size().ok()?.to_logical::<f64>(scale);
    Some(NormalBounds {
        position: (position.x, position.y),
        size: (size.width, size.height),
    })
}

/// The window's bounds if it's currently in its normal state.
pub fn normal_bounds(window: &tauri::Window) -> Option<NormalBounds> {
    if is_displaced(window) {
        None
    } else {
        current_bounds(window)
    }
}

/// Geometry to remember for a closing (or cloned) window. Fullscreen is
/// deliberately not carried over: the window comes back as a normal one at
/// its pre-fullscreen bounds.
pub fn capture_geometry(
    window: &tauri::Window,
    tracked: Option<NormalBounds>,
) -> Option<WindowGeometry> {
    let maximized = window.is_maximized().unwrap_or(false);
    let bounds = normal_bounds(window).or(tracked)?;
    Some(WindowGeometry {
        position: Some(bounds.position),
        size: bounds.size,
        maximized,
    })
}

/// Size to remember for a closing viewer/editor; nothing while it's
/// maximized or fullscreen, whose size isn't a preference.
pub fn capture_child_size(window: &tauri::Window) -> Option<(f64, f64)> {
    normal_bounds(window).map(|b| b.size)
}

/// A monitor's work area (minus taskbar / menu bar) in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkArea {
    pub position: (f64, f64),
    pub size: (f64, f64),
}

pub fn work_areas(monitors: &[tauri::Monitor]) -> Vec<WorkArea> {
    monitors
        .iter()
        .map(|m| {
            let scale = m.scale_factor();
            let area = m.work_area();
            WorkArea {
                position: (
                    area.position.x as f64 / scale,
                    area.position.y as f64 / scale,
                ),
                size: (
                    area.size.width as f64 / scale,
                    area.size.height as f64 / scale,
                ),
            }
        })
        .collect()
}

/// Fit remembered geometry to the displays attached now: drop a position
/// whose title bar would be unreachable (a monitor since unplugged), and
/// step off any live window sitting at the same spot so two windows of one
/// target don't stack into what looks like one.
pub fn place(
    areas: &[WorkArea],
    occupied: &[(f64, f64)],
    geometry: WindowGeometry,
) -> WindowGeometry {
    let size = geometry.size;
    let position = geometry
        .position
        .filter(|&p| title_bar_visible(areas, p, size))
        .map(|mut p| {
            for _ in 0..occupied.len() {
                let taken = occupied
                    .iter()
                    .any(|o| (o.0 - p.0).abs() < 2.0 && (o.1 - p.1).abs() < 2.0);
                if !taken {
                    break;
                }
                p = (p.0 + CASCADE_STEP, p.1 + CASCADE_STEP);
            }
            p
        });
    WindowGeometry {
        position,
        ..geometry
    }
}

fn title_bar_visible(areas: &[WorkArea], pos: (f64, f64), size: (f64, f64)) -> bool {
    const MIN_GRAB_WIDTH: f64 = 100.0;
    const TITLE_BAR: f64 = 30.0;
    const EDGE_SLACK: f64 = 16.0;
    areas.iter().any(|a| {
        let (ax, ay) = a.position;
        let (aw, ah) = a.size;
        let overlap = (pos.0 + size.0).min(ax + aw) - pos.0.max(ax);
        overlap >= MIN_GRAB_WIDTH && pos.1 + EDGE_SLACK >= ay && pos.1 + TITLE_BAR <= ay + ah
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> (tempfile::TempDir, SessionMemory) {
        let dir = tempfile::tempdir().unwrap();
        let memory = SessionMemory::new(dir.path());
        (dir, memory)
    }

    fn locations(path: &str) -> SessionLocations {
        SessionLocations {
            panes: [
                Some(PaneLocation {
                    path: VfsPathBuf::from_wire_str(path),
                    focused: Some("x".into()),
                }),
                None,
            ],
            active_pane: PaneHandle::right(),
        }
    }

    fn geometry(x: f64) -> WindowGeometry {
        WindowGeometry {
            position: Some((x, 10.0)),
            size: (900.0, 600.0),
            maximized: false,
        }
    }

    #[test]
    fn last_closed_wins_and_keep_preserves() {
        let (_dir, memory) = memory();
        let local = SessionIdentity::local();
        memory.record(&local, LocationUpdate::Set(locations("/a")), None);
        memory.record(&local, LocationUpdate::Set(locations("/b")), None);
        assert_eq!(memory.locations(&local), Some(locations("/b")));

        memory.record(&local, LocationUpdate::Keep, Some(geometry(5.0)));
        assert_eq!(memory.locations(&local), Some(locations("/b")));
        assert_eq!(memory.geometry(&local), Some(geometry(5.0)));

        memory.record(&local, LocationUpdate::Clear, None);
        assert_eq!(memory.locations(&local), None);
        assert_eq!(memory.geometry(&local), Some(geometry(5.0)));
    }

    #[test]
    fn unseen_target_borrows_latest_size_without_position() {
        let (_dir, memory) = memory();
        memory.record(
            &SessionIdentity::local(),
            LocationUpdate::Keep,
            Some(geometry(1.0)),
        );
        memory.record(
            &SessionIdentity::elevated(),
            LocationUpdate::Keep,
            Some(geometry(2.0)),
        );
        let borrowed = memory
            .geometry(&SessionIdentity(String::from("ssh:host")))
            .unwrap();
        assert_eq!(borrowed.position, None);
        assert_eq!(borrowed.size, geometry(2.0).size);
    }

    #[test]
    fn capped_and_forgettable() {
        let (_dir, memory) = memory();
        for i in 0..MAX_REMEMBERED_SESSIONS + 3 {
            memory.record(
                &SessionIdentity(format!("ssh:h{i}")),
                LocationUpdate::Set(locations("/")),
                None,
            );
        }
        assert_eq!(memory.load().sessions.len(), MAX_REMEMBERED_SESSIONS);
        assert!(
            memory
                .locations(&SessionIdentity("ssh:h0".into()))
                .is_none()
        );

        let newest = SessionIdentity(format!("ssh:h{}", MAX_REMEMBERED_SESSIONS + 2));
        assert!(memory.locations(&newest).is_some());
        memory.forget(&newest);
        assert!(memory.locations(&newest).is_none());
    }

    #[test]
    fn corrupt_file_starts_afresh() {
        let (dir, memory) = memory();
        std::fs::write(dir.path().join("sessions.json"), "{not json").unwrap();
        assert!(memory.locations(&SessionIdentity::local()).is_none());
        memory.record(
            &SessionIdentity::local(),
            LocationUpdate::Set(locations("/a")),
            None,
        );
        assert_eq!(
            memory.locations(&SessionIdentity::local()),
            Some(locations("/a"))
        );
    }

    #[test]
    fn position_dropped_when_off_every_monitor() {
        let areas = [WorkArea {
            position: (0.0, 25.0),
            size: (1440.0, 875.0),
        }];
        assert_eq!(place(&[], &[], geometry(0.0)).position, None);
        assert_eq!(place(&areas, &[], geometry(2000.0)).position, None);
        // Just the left edge peeking out isn't grabbable.
        assert_eq!(place(&areas, &[], geometry(1400.0)).position, None);
        assert_eq!(
            place(
                &areas,
                &[],
                WindowGeometry {
                    position: Some((100.0, 890.0)),
                    ..geometry(0.0)
                }
            )
            .position,
            None
        );
        assert_eq!(
            place(&areas, &[], geometry(100.0)).position,
            Some((100.0, 10.0))
        );
    }

    #[test]
    fn cascades_off_occupied_spots() {
        let areas = [WorkArea {
            position: (0.0, 0.0),
            size: (1920.0, 1080.0),
        }];
        let occupied = [(100.0, 10.0), (124.0, 34.0), (500.0, 500.0)];
        assert_eq!(
            place(&areas, &occupied, geometry(100.0)).position,
            Some((148.0, 58.0))
        );
        assert_eq!(
            place(&areas, &occupied, geometry(300.0)).position,
            Some((300.0, 10.0))
        );
    }
}
