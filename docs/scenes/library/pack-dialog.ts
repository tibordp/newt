import { at, dir, mainWindow, pane, s3 } from "../builders";
import { HOME, LOCAL_STATS, PROJECT, docsColumns } from "../demo";
import type { Scene } from "../types";

const bucket = s3(1, "demo.backup");

const scene: Scene = {
  description: "Packing a project into an archive on S3",
  size: { width: 1000, height: 600 },
  ...docsColumns(1000),
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: `${HOME}/src`,
        focused: "walker",
        fsStats: LOCAL_STATS,
        entries: [
          dir("dotfiles", { modified: at("2026-07-02T21:03:11") }),
          dir("homelab", { modified: at("2026-07-22T19:47:30") }),
          dir("newt", { modified: at("2026-07-28T17:30:02") }),
          dir("walker", { modified: at("2026-07-28T17:24:40") }),
          dir("website", { modified: at("2026-07-25T10:31:08") }),
        ],
      }),
      pane({ path: "/walker", vfs: bucket, entries: [] }),
    ],
    modal: {
      type: "create_archive",
      context: { pane_handle: 0 },
      data: {
        sources: [{ vfs_id: 0, path: PROJECT }],
        destination: { vfs_id: 1, path: "/walker" },
        display_destination: "s3://demo.backup/walker",
        summary: "walker",
        default_name: "walker",
        name_separators: "/",
        defaults: {
          format: "tar_zst",
          preserve_symlinks: true,
          zip_level: 6,
          sevenz_level: 6,
          gzip_level: 6,
          xz_level: 6,
          zstd_level: 3,
        },
      },
    },
  }),
};

export default scene;
