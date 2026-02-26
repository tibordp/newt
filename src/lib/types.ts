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
};
