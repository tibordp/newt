import { dir, at, fsStats, gb, mainWindow, pane } from "../builders";
import { HOME, LOCAL_STATS, photoEntries, docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "A multi-file selection, with its total in the status bar",
  size: { width: 1000, height: 560 },
  ...docsColumns(1000),
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: `${HOME}/Pictures/2026-07 Lisbon`,
        focused: "IMG_4126.HEIC",
        selected: [
          "IMG_4107.HEIC",
          "IMG_4111.HEIC",
          "IMG_4118.HEIC",
          "IMG_4126.HEIC",
        ],
        fsStats: LOCAL_STATS,
        entries: photoEntries(),
      }),
      pane({
        path: "/Volumes/Backup/Photos/2026",
        fsStats: fsStats(gb(4000.79), gb(1204.33)),
        entries: [
          dir("2026-03 Ljubljana", { modified: at("2026-03-29T20:14:02") }),
          dir("2026-05 Dolomites", { modified: at("2026-05-18T21:40:37") }),
        ],
      }),
    ],
  }),
};

export default scene;
