import { at } from "../builders";
import { behindDialog, docsColumns, HOME, PROJECT } from "../demo";
import type { Scene } from "../types";

const local = (path: string, when: string, vfs = "Local", display = path) => ({
  path: { vfs_id: 0, path },
  vfs_display_name: vfs,
  display_path: display,
  is_alive: true,
  arrived_at: at(when),
});

const scene: Scene = {
  description: "A pane's navigation history as a persistent dialog",
  size: { width: 1000, height: 560 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "history_navigator",
    context: { pane_handle: 0 },
    data: {
      persistent: true,
      initial_direction: 0,
      current_index: 5,
      entries: [
        local(HOME, "2026-07-28T16:58:10"),
        local(`${HOME}/Downloads`, "2026-07-28T17:02:44"),
        {
          path: { vfs_id: 2, path: "/walker-0.4.2" },
          vfs_display_name: "Archive",
          display_path: `${HOME}/Downloads/walker-0.4.2.tar.zst/walker-0.4.2`,
          is_alive: false,
          arrived_at: at("2026-07-28T17:03:20"),
        },
        {
          path: { vfs_id: 1, path: "/walker" },
          vfs_display_name: "S3",
          display_path: "s3://demo.backup/walker",
          is_alive: true,
          arrived_at: at("2026-07-28T17:11:05"),
        },
        local(`${HOME}/src`, "2026-07-28T17:20:31"),
        local(PROJECT, "2026-07-28T17:24:02"),
      ],
    },
  }),
};

export default scene;
