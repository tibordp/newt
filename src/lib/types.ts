export type VfsPath = {
  vfs_id: number;
  path: string;
};

export function joinVfsPath(base: VfsPath, name: string): VfsPath {
  return { vfs_id: base.vfs_id, path: base.path + "/" + name };
}
