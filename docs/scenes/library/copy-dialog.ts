import { mainWindow, pane } from "../builders";
import { HOME, LOCAL_STATS, docsColumns, photoEntries } from "../demo";
import type { CopyOptions } from "../../../src/lib/bindings";
import type { Scene } from "../types";

const source = `${HOME}/Pictures/2026-07 Lisbon`;
const destination = "/Volumes/Backup/Photos/2026";
const selected = [
  "IMG_4107.HEIC",
  "IMG_4111.HEIC",
  "IMG_4118.HEIC",
  "IMG_4126.HEIC",
];

/// What a Mac sets up until a toggle is changed.
const options: CopyOptions = {
  preserve_timestamps: true,
  preserve_permissions: true,
  ownership_by_name: false,
  preserve_owner: false,
  preserve_group: false,
  preserve_xattrs: true,
  preserve_acl: false,
  preserve_streams: true,
  preserve_hard_links: false,
  preserve_sparse: false,
  preserve_object_metadata: false,
  preserve_object_tags: false,
  preserve_object_access: false,
  object_storage_class: null,
  object_canned_acl: null,
  follow_symlinks: false,
  preserve_merged_directories: false,
  create_symlink: false,
  one_file_system: false,
  conflict_resolution: null,
  folder_conflicts: "merge",
};

const scene: Scene = {
  description: "The Copy dialog for four selected photos",
  size: { width: 1000, height: 600 },
  ...docsColumns(1000),
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: source,
        focused: "IMG_4126.HEIC",
        selected,
        fsStats: LOCAL_STATS,
        entries: photoEntries(),
      }),
      pane({ path: destination, entries: [] }),
    ],
    modal: {
      type: "copy_move",
      context: { pane_handle: 0 },
      data: {
        kind: "copy",
        object_destination: false,
        object_source: false,
        sources: selected.map((name) => ({
          vfs_id: 0,
          path: `${source}/${name}`,
        })),
        destination: { vfs_id: 0, path: destination },
        display_destination: destination,
        summary: `${selected.length} items`,
        default_name: null,
        default_name_is_dir: false,
        name_separators: "/",
        defaults: options,
        stock: options,
      },
    },
  }),
};

export default scene;
