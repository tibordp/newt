import { ReactElement } from "react";

import type {
  Annotation,
  ContextBadge,
  FileView,
  FilterMode,
  SizeUnits,
  Symlink,
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

/// Whether a row is a package (an app or another folder macOS presents
/// as a file), as its filesystem says.
export function isPackage(row: FileView): boolean {
  return row.annotations?.some((a) => "package" in a) ?? false;
}

/// A link's target, when it could be read.
export function symlinkTarget(link: Symlink | null): string | null {
  return link !== null && link !== "unknown" ? link.target : null;
}

/// Whether a row is a file with an execute bit. A symlink's own mode says
/// nothing about its target, so a link never counts.
export function isExecutable(row: FileView): boolean {
  return (
    !row.is_dir &&
    row.symlink === null &&
    row.mode != null &&
    (row.mode & 0o111) !== 0
  );
}

/// The du annotation's payload for a row, if any.
export function recursiveSize(row: FileView): RecursiveSize | undefined {
  const a = row.annotations?.find((a) => "recursive_size" in a);
  return a && "recursive_size" in a ? a.recursive_size : undefined;
}

/// Window event carrying a keyboard-requested context menu to the active
/// pane, dispatched by the document-level handler in MainWindow.
export const KEYBOARD_MENU_EVENT = "newt:keyboard-context-menu";

/// Window event asking whatever holds focus in Rust state — the active pane
/// or the active terminal — to take DOM focus back, sent when a menu closes
/// on a pane that doesn't hold it.
export const REFOCUS_EVENT = "newt:refocus";
