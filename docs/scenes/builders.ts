import type {
  Annotation,
  ContextBadge,
  EditorState,
  File,
  FsStats,
  GitEntryStatus,
  MainWindowState,
  ModalData,
  OperationState,
  PaneViewState,
  Sorting,
  VfsPath,
  ViewerMode,
  ViewerState,
} from "../../src/lib/bindings";

/// Unix milliseconds, the unit `File` timestamps use.
export const at = (iso: string): number => Date.parse(iso);

/// Decimal units, as the app displays sizes by default.
export const kb = (n: number) => Math.round(n * 1e3);
export const mb = (n: number) => Math.round(n * 1e6);
export const gb = (n: number) => Math.round(n * 1e9);

export function file(name: string, fields: Partial<File> = {}): File {
  return {
    name,
    size: 0,
    allocated_size: null,
    device_id: null,
    inode: null,
    hard_links: null,
    is_dir: false,
    is_hidden: name.startsWith(".") && name !== "..",
    is_symlink: false,
    symlink_target: null,
    user: { name: "demo" },
    group: { name: "staff" },
    mode: 0o100644,
    attributes: null,
    modified: null,
    accessed: null,
    created: null,
    key: null,
    source: null,
    ...fields,
  };
}

export function dir(name: string, fields: Partial<File> = {}): File {
  return file(name, { is_dir: true, size: null, mode: 0o40755, ...fields });
}

export type VfsInfo = {
  id: number;
  /// Pane header label ("Local", "S3", …).
  name: string;
  hostLocal: boolean;
  /// Display form of a path on this VFS.
  display: (path: string) => string;
  /// Label of the root breadcrumb.
  rootLabel: string;
};

export const LOCAL: VfsInfo = {
  id: 0,
  name: "Local",
  hostLocal: true,
  display: (p) => p,
  rootLabel: "/",
};

export const s3 = (id: number, bucket: string): VfsInfo => ({
  id,
  name: "S3",
  hostLocal: false,
  display: (p) => `s3://${bucket}${p === "/" ? "" : p}`,
  rootLabel: `s3://${bucket}/`,
});

export type PaneSpec = {
  path: string;
  vfs?: VfsInfo;
  /// Listing without `..`; sorted dirs-first by name unless `sorting` or
  /// `presorted` says otherwise.
  entries: File[];
  presorted?: boolean;
  focused?: string;
  selected?: string[];
  sorting?: Sorting;
  showHidden?: boolean;
  fsStats?: FsStats;
  gitBranch?: string;
  /// Entry name → git status annotation.
  git?: Record<string, GitEntryStatus>;
  /// Escape hatch for anything the builder doesn't model.
  override?: Partial<PaneViewState>;
};

export function pane(spec: PaneSpec): PaneViewState {
  const vfs = spec.vfs ?? LOCAL;
  const sorting = spec.sorting ?? { key: "name", asc: true };
  const visible = spec.entries.filter((f) => spec.showHidden || !f.is_hidden);
  const sorted = spec.presorted ? visible : sortEntries(visible, sorting);
  const items =
    spec.path === "/" ? sorted : [dir("..", { mode: null }), ...sorted];
  const selected = new Set(spec.selected ?? []);
  const focused = spec.focused ?? items[0]?.name ?? null;

  const annotations = (name: string): Annotation[] => {
    const git = spec.git?.[name];
    return git ? [{ git }] : [];
  };

  const stats = {
    file_count: 0,
    dir_count: 0,
    bytes: 0,
    selected_file_count: 0,
    selected_dir_count: 0,
    selected_bytes: 0,
    total_count: null,
    hidden_count: spec.entries.length - visible.length,
  };
  for (const f of sorted) {
    const size = f.size ?? 0;
    stats[f.is_dir ? "dir_count" : "file_count"] += 1;
    stats.bytes += size;
    if (selected.has(f.name)) {
      stats[f.is_dir ? "selected_dir_count" : "selected_file_count"] += 1;
      stats.selected_bytes += size;
    }
  }

  const badges: ContextBadge[] = spec.gitBranch
    ? [
        {
          git_branch: {
            name: spec.gitBranch,
            detached: false,
            ahead: 0,
            behind: 0,
            dirty: false,
          },
        },
      ]
    : [];

  return {
    path: { vfs_id: vfs.id, path: spec.path },
    pending_path: null,
    loading: false,
    partial: null,
    sorting,
    file_window: {
      items: items.map((f) => ({
        ...f,
        source_display: null,
        annotations: annotations(f.name),
      })),
      offset: 0,
      total_count: items.length,
    },
    focused,
    selected: [...selected],
    filter: null,
    filter_mode: "quick_search",
    fs_stats: spec.fsStats ?? null,
    stats,
    focused_index:
      focused === null ? null : items.findIndex((f) => f.name === focused),
    display_path: vfs.display(spec.path),
    vfs_display_name: vfs.name,
    is_host_local: vfs.hostLocal,
    metadata_traits: { unix_owner: vfs.hostLocal, windows_attributes: false },
    breadcrumbs: breadcrumbs(spec.path, vfs.rootLabel),
    context_badges: badges,
    enrichment_activity: {},
    ...spec.override,
  };
}

function sortEntries(entries: File[], sorting: Sorting): File[] {
  const key = (f: File): string | number =>
    sorting.key === "size"
      ? (f.size ?? 0)
      : sorting.key === "modified"
        ? (f.modified ?? 0)
        : f.name.toLowerCase();
  return [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    const ka = key(a);
    const kb = key(b);
    const c = ka < kb ? -1 : ka > kb ? 1 : 0;
    return sorting.asc ? c : -c;
  });
}

/// Mirrors `unix_breadcrumbs` in newt-common.
function breadcrumbs(path: string, rootLabel: string) {
  const segs = path.split("/").filter(Boolean);
  let acc = "";
  return [
    { label: rootLabel, nav_path: "/" },
    ...segs.map((seg, i) => {
      acc += `/${seg}`;
      return { label: i === segs.length - 1 ? seg : `${seg}/`, nav_path: acc };
    }),
  ];
}

export function fsStats(total: number, available: number): FsStats {
  return {
    total_bytes: total,
    free_bytes: available,
    available_bytes: available,
    volume: null,
  };
}

export type MainWindowSpec = {
  panes: [PaneViewState, PaneViewState];
  activePane?: 0 | 1;
  title?: string;
  /// Terminal handles; the first is active.
  terminals?: number[];
  terminalVisible?: boolean;
  /// Focus is in the terminal rather than the panes.
  terminalFocused?: boolean;
  showHidden?: boolean;
  /// Only the focused pane (or the terminal) is shown.
  maximized?: boolean;
  modal?: ModalData;
  operations?: OperationState[];
};

export function mainWindow(spec: MainWindowSpec): MainWindowState {
  const terminals = spec.terminals ?? [];
  const operations = spec.operations ?? [];
  const foreground = operations.find((op) => !op.backgrounded && !op.silent);
  return {
    connection_status: { status: "connected", log: [] },
    askpass: null,
    panes: spec.panes,
    terminals: Object.fromEntries(
      terminals.map((handle) => [String(handle), { handle, defunct: false }]),
    ),
    modal: spec.modal ?? null,
    dnd: null,
    display_options: {
      show_hidden: spec.showHidden ?? false,
      active_pane: spec.activePane ?? 0,
      active_terminal: terminals[0] ?? null,
      panes_focused: !spec.terminalFocused,
      terminal_panel_visible: spec.terminalVisible ?? terminals.length > 0,
      maximized: spec.maximized ?? false,
    },
    operations: Object.fromEntries(operations.map((op) => [String(op.id), op])),
    window_title: spec.title ?? "Newt",
    foreground_operation_id: foreground?.id ?? null,
    vfs_progress: {},
    mount_log: [],
    mount_summary: { has_split_root_vfs: false },
  };
}

export function operation(
  fields: Pick<OperationState, "id" | "kind" | "description"> &
    Partial<OperationState>,
): OperationState {
  return {
    total_bytes: null,
    total_items: null,
    bytes_done: 0,
    items_done: 0,
    current_item: "",
    status: "running",
    error: null,
    issue: null,
    backgrounded: false,
    silent: false,
    scanning_items: null,
    scanning_bytes: null,
    ...fields,
  };
}

export function viewer(
  path: VfsPath,
  mode: ViewerMode,
  displayPath = path.path,
): ViewerState {
  return {
    mode,
    file_path: path,
    display_path: displayPath,
    // Pointed at the fixture by the harness.
    file_server_base: null,
    encoding: { detected: null, selected: null },
    table: {
      delimiter: null,
      detected_delimiter: null,
      quoted: true,
      header: null,
      detected_header: null,
    },
  };
}

export function editor(
  path: VfsPath,
  language: string,
  displayPath = path.path,
): EditorState {
  return {
    language,
    word_wrap: false,
    file_path: path,
    display_path: displayPath,
  };
}
