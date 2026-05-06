export type VfsPath = {
  vfs_id: number;
  path: string;
};

export type Breadcrumb = {
  label: string;
  nav_path: string;
};

export type VfsTarget = {
  vfs_id: number | null;
  type_name: string;
  display_name: string;
  label: string | null;
  mount_dialog: string | null;
};

export type HistoryEntryView = {
  path: VfsPath;
  vfs_display_name: string;
  display_path: string;
  is_alive: boolean;
  /// Unix milliseconds — when the user originally arrived at this path.
  arrived_at: number;
};
