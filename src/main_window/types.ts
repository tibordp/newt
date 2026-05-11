import { ReactElement } from "react";

import type {
  AskpassPrompt,
  Breadcrumb,
  ConnectionStatus,
  DndData,
  DndFile,
  File,
  FilterMode,
  FsStats,
  ModalData,
  OperationState,
  PaneHandle,
  Sorting,
  TerminalHandle,
  VfsPath,
  VfsProgress,
} from "../lib/bindings";

export type { File, FilterMode, FsStats, Sorting } from "../lib/bindings";
export type { OperationState } from "./OperationsPanel";

/// Per-row context passed to column renderers.
export type FileRowContext = {
  isFocused: boolean;
  filter: string | null;
  filterMode: FilterMode;
};

export type ColumnDef = {
  align: "left" | "right" | "center";
  initialWidth: number;
  subcolumns?: SubcolumnDef[];
  key: string;
  render: (info: FileView, ctx: FileRowContext) => ReactElement;
};

export type SubcolumnDef = {
  name: string;
  sortKey?: string;
  style?: React.CSSProperties;
};

export type PaneStats = {
  file_count: number;
  dir_count: number;
  bytes: number;
  selected_file_count: number;
  selected_dir_count: number;
  selected_bytes: number;
  total_count?: number;
};

/// Display projection of `File` produced by the host: same fields as
/// `File`, with `source_display` pre-rendered through the source VFS's
/// descriptor for synthetic-VFS entries (search results, …).
export type FileView = File & {
  source_display?: string;
};

export type FileWindow = {
  items: FileView[];
  offset: number;
  total_count: number;
};

export type PaneState = {
  path: VfsPath;
  pending_path?: VfsPath;
  loading?: boolean;
  partial?: boolean;
  sorting: Sorting;
  file_window: FileWindow;
  focused?: string;
  selected: string[];
  active: boolean;
  filter: string | null;
  filter_mode: FilterMode;
  fs_stats?: FsStats;
  stats: PaneStats;
  focused_index?: number;
  display_path: string;
  vfs_display_name: string;
  is_host_local: boolean;
  breadcrumbs: Breadcrumb[];
};

export type DisplayOptions = {
  show_hidden: boolean;
  active_pane: PaneHandle;
  panes_focused: boolean;
  active_terminal?: TerminalHandle;
  terminal_panel_visible: boolean;
};

export type Terminal = {
  handle: TerminalHandle;
  defunct: boolean;
};

/// Local DnD info kept by the source pane while a drag is in flight.
/// Mirrors the codegen `DndFile` shape but kept separately so the local code
/// doesn't drift when DndFile gains optional fields.
export type DndFileInfo = DndFile;

export type MainWindowState = {
  connection_status: ConnectionStatus;
  askpass?: AskpassPrompt;
  panes: PaneState[];
  terminals: Terminal[];
  display_options: DisplayOptions;
  modal?: ModalData;
  dnd?: DndData;
  operations: Record<string, OperationState>;
  window_title: string;
  foreground_operation_id?: number;
  /// VFS-keyed background progress (e.g. SearchVfs walker status). Keys
  /// are stringified VfsIds.
  vfs_progress: Record<string, VfsProgress>;
};
