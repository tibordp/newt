import { ReactElement } from "react";

import type {
  Annotation,
  ContextBadge,
  DndFile,
  FileView,
  FilterMode,
  SizeUnits,
} from "../lib/bindings";

export type {
  Annotation,
  ContextBadge,
  DisplayOptionsInner as DisplayOptions,
  File,
  FileView,
  FileWindow,
  FilterMode,
  FsStats,
  GitEntryStatus,
  MainWindowState,
  PaneStats,
  PaneViewState as PaneState,
  Sorting,
  TerminalView as Terminal,
} from "../lib/bindings";
export type { OperationState } from "./OperationsPanel";

/// Per-row context passed to column renderers.
export type FileRowContext = {
  isFocused: boolean;
  filter: string | null;
  filterMode: FilterMode;
  /// strftime-style formats from preferences; empty/undefined = system locale.
  dateFormat?: string;
  timeFormat?: string;
  /// Resolved BCP-47 locale for numbers and dates; undefined = runtime default.
  locale?: string;
  /// Render the Size column with unit prefixes instead of exact byte counts.
  siSizePrefixes?: boolean;
  sizeUnits?: SizeUnits;
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

export type RecursiveSize = Extract<
  Annotation,
  { recursive_size: unknown }
>["recursive_size"];
export type GitBranch = ContextBadge["git_branch"];

/// The git annotation's payload for a row, if any.
export function gitStatus(
  row: FileView,
): Extract<Annotation, { git: unknown }>["git"] | undefined {
  const a = row.annotations?.find((a) => "git" in a);
  return a && "git" in a ? a.git : undefined;
}

/// The du annotation's payload for a row, if any.
export function recursiveSize(row: FileView): RecursiveSize | undefined {
  const a = row.annotations?.find((a) => "recursive_size" in a);
  return a && "recursive_size" in a ? a.recursive_size : undefined;
}

/// Local DnD info kept by the source pane while a drag is in flight.
/// Mirrors the codegen `DndFile` shape but kept separately so the local code
/// doesn't drift when DndFile gains optional fields.
export type DndFileInfo = DndFile;

/// Window event carrying a keyboard-requested context menu to the active
/// pane, dispatched by the document-level handler in MainWindow.
export const KEYBOARD_MENU_EVENT = "newt:keyboard-context-menu";

/// Window event asking whatever holds focus in Rust state — the active pane
/// or the active terminal — to take DOM focus back, sent when a menu closes
/// on a pane that doesn't hold it.
export const REFOCUS_EVENT = "newt:refocus";

/// Whether a `contextmenu` event came from the keyboard rather than a
/// pointer. The Menu key produces no keydown the panes can bind, only this
/// event, and the webview aims it by hit-testing a point of its own
/// choosing — an unrelated row, in whichever pane the point fell in — so it
/// must be recognised and re-routed rather than acted on where it landed.
/// A pointer carries button 2; macOS ctrl+click reports no button either,
/// but carries `ctrlKey`.
export function isKeyboardContextMenu(e: MouseEvent): boolean {
  return e.button !== 2 && !e.ctrlKey;
}
