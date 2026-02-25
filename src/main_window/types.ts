import { ReactElement } from "react";
import { VfsPath } from "../lib/types";
import { ModalState } from "./modals/ModalContent";
import { OperationState } from "./OperationsPanel";

export type { OperationState };

export type File = {
  name: string;
  size?: number;
  is_dir: boolean;
  is_symlink: boolean;
  is_hidden: boolean;
  user: {
    name?: string,
    id?: number
  },
  group: {
    name?: string,
    id?: number
  },
  mode: number;
  modified: number;
  accessed: number;
  created: number;
};

export type FileRowContext = {
  isFocused: boolean;
  filter?: string;
};

export type ColumnDef = {
  align: "left" | "right" | "center";
  initialWidth: number;
  subcolumns?: SubcolumnDef[];
  key: string;
  render: (info: File, ctx: FileRowContext) => ReactElement;
};

export type SubcolumnDef = {
  name: string;
  sortKey?: string;
  style?: React.CSSProperties;
};

export type Sorting = {
  key: string;
  asc: boolean;
};

export type FsStats = {
  available_bytes: number;
  free_bytes: number;
  total_bytes: number;
};

export type PaneStats = {
  file_count: number;
  dir_count: number;
  bytes: number;
  selected_file_count: number;
  selected_dir_count: number;
  selected_bytes: number;
};

export type PaneState = {
  path: VfsPath;
  pending_path?: VfsPath;
  loading?: boolean;
  partial?: boolean;
  sorting: Sorting;
  files: File[];
  focused?: string;
  selected: string[];
  active: boolean;
  filter?: string;
  fs_stats?: FsStats;
  stats: PaneStats;
  focused_index?: number;
};

export type DisplayOptions = {
  show_hidden: boolean;
  active_pane: number;
  panes_focused: boolean;
  active_terminal?: number;
};

export type Terminal = {
  handle: number;
};

export type DndFileInfo = {
  name: string;
  is_dir: boolean;
};

export type DndState = {
  source_pane: number;
  files: DndFileInfo[];
};

export type MainWindowState = {
  panes: PaneState[];
  terminals: Terminal[];
  display_options: DisplayOptions;
  modal?: ModalState;
  dnd?: DndState;
  operations: Record<string, OperationState>;
  window_title: string;
  foreground_operation_id?: number;
};
