import { at } from "../builders";
import { behindDialog, docsColumns, PROJECT } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "The Properties dialog for a file, with Unix permissions",
  size: { width: 1000, height: 640 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog(
    {
      type: "properties",
      context: { pane_handle: 0 },
      data: {
        paths: [{ vfs_id: 0, path: `${PROJECT}/Cargo.toml` }],
        name: "Cargo.toml",
        size: 936,
        allocated_size: 4096,
        hard_links: 1,
        inode: 48_211_907,
        device_id: 16_777_231,
        is_dir: false,
        is_symlink: false,
        symlink_target: null,
        can_set_metadata: true,
        mode_set: 0o644,
        mode_clear: 0o7133,
        has_mode: true,
        owner: { name: "demo" },
        group: { name: "staff" },
        owner_id: 501,
        group_id: 20,
        modified: at("2026-07-18T22:47:55"),
        accessed: at("2026-07-28T17:31:02"),
        created: at("2026-06-02T10:11:12"),
        sheet: { status: "hidden" },
        fs_stats: null,
      },
    },
    { focused: "Cargo.toml" },
  ),
};

export default scene;
