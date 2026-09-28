import { at, dir, file, fsStats, gb, mainWindow, pane } from "../builders";
import { docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "One pane maximized in place of the split layout",
  size: { width: 1100, height: 640 },
  // Column widths are per pane and don't stretch; these are sized for the
  // whole window, as they would be after widening the Name column.
  ...docsColumns(2 * 1100),
  window: "main",
  state: mainWindow({
    maximized: true,
    terminals: [1],
    panes: [
      pane({
        path: "/Users/demo/src/newt",
        focused: "src-tauri",
        gitBranch: "master",
        fsStats: fsStats(gb(994.66), gb(297.71)),
        entries: [
          dir("docs", { modified: at("2026-07-17T00:58:44") }),
          dir("libs", { modified: at("2026-07-18T23:10:24") }),
          dir("scripts", { modified: at("2026-07-14T18:16:35") }),
          dir("src", { modified: at("2026-07-26T20:21:15") }),
          dir("src-tauri", { modified: at("2026-07-28T17:24:38") }),
          dir("xtask", { modified: at("2026-07-27T11:02:51") }),
          file("Cargo.lock", {
            size: 214_307,
            modified: at("2026-07-28T17:24:02"),
          }),
          file("Cargo.toml", {
            size: 536,
            modified: at("2026-07-18T22:47:55"),
          }),
          file("package.json", {
            size: 1_941,
            modified: at("2026-07-16T13:10:58"),
          }),
          file("README.md", {
            size: 4_696,
            modified: at("2026-07-27T12:41:41"),
          }),
        ],
      }),
      pane({
        path: "/Users/demo/src/newt/libs",
        focused: "newt-common",
        entries: [
          dir("newt-agent", { modified: at("2026-07-28T16:02:19") }),
          dir("newt-common", { modified: at("2026-07-28T17:11:47") }),
        ],
      }),
    ],
  }),
};

export default scene;
