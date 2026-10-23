import {
  at,
  dir,
  file,
  fsStats,
  gb,
  kb,
  mainWindow,
  pane,
  type VfsInfo,
} from "../builders";
import { docsColumns, LOCAL_STATS, PROJECT, projectEntries } from "../demo";
import type { Scene } from "../types";

/// A build host mounted over SSH: its paths and owners are Linux's on every
/// platform, so the dialog shows the same Unix permissions everywhere.
const BUILD_01: VfsInfo = {
  id: 2,
  name: "SSH",
  typeName: "agent",
  hostLocal: false,
  display: (p) => p,
  rootLabel: "/",
};

const path = "/srv/app/current";
const deploy = { user: { name: "deploy" }, group: { name: "deploy" } };

const scene: Scene = {
  description:
    "The Properties dialog for a file on a Linux host over SSH, with Unix permissions",
  size: { width: 1000, height: 640 },
  ...docsColumns(1000),
  window: "main",
  state: mainWindow({
    modal: {
      type: "properties",
      context: { pane_handle: 0 },
      data: {
        paths: [{ vfs_id: BUILD_01.id, path: `${path}/app.env` }],
        name: "app.env",
        size: 1_284,
        allocated_size: 4096,
        hard_links: 1,
        inode: 1_572_918,
        device_id: 2_049,
        is_dir: false,
        symlink: null,
        can_set_metadata: true,
        mode_set: 0o640,
        mode_clear: 0o7137,
        has_mode: true,
        owner: { name: "deploy" },
        group: { name: "www-data" },
        owner_id: 1001,
        group_id: 33,
        modified: at("2026-07-28T15:31:47"),
        accessed: at("2026-07-28T17:02:13"),
        created: at("2026-07-28T15:31:47"),
        sheet: { status: "hidden" },
        fs_stats: null,
      },
    },
    panes: [
      pane({
        path,
        vfs: BUILD_01,
        focused: "app.env",
        fsStats: fsStats(gb(492.1), gb(187.4)),
        override: {
          metadata_traits: { unix_owner: true, windows_attributes: false },
        },
        entries: [
          dir("bin", { ...deploy, modified: at("2026-07-28T15:31:47") }),
          dir("config", { ...deploy, modified: at("2026-07-28T15:31:47") }),
          dir("logs", { ...deploy, modified: at("2026-07-28T17:02:13") }),
          dir("static", { ...deploy, modified: at("2026-07-28T15:31:47") }),
          file("app.env", {
            user: { name: "deploy" },
            group: { name: "www-data" },
            mode: 0o100640,
            size: 1_284,
            modified: at("2026-07-28T15:31:47"),
          }),
          file("release.json", {
            ...deploy,
            size: 412,
            modified: at("2026-07-28T15:31:47"),
          }),
          file("run.sh", {
            ...deploy,
            mode: 0o100755,
            size: kb(2.3),
            modified: at("2026-07-28T15:31:47"),
          }),
        ],
      }),
      pane({
        path: PROJECT,
        gitBranch: "main",
        fsStats: LOCAL_STATS,
        entries: projectEntries(),
      }),
    ],
  }),
};

export default scene;
