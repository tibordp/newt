import {
  at,
  dir,
  file,
  fsStats,
  gb,
  mainWindow,
  pane,
  viewer,
} from "../builders";
import { docsColumns } from "../demo";
import readme from "../fixtures/README.md?url";
import type { Scene } from "../types";

const project = "/Users/demo/src/brewlog";
const focused = `${project}/README.md`;

const scene: Scene = {
  description: "Quick View previewing the focused file beside the list",
  size: { width: 1100, height: 640 },
  ...docsColumns(1100),
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: project,
        focused: "README.md",
        gitBranch: "main",
        fsStats: fsStats(gb(994.66), gb(297.71)),
        entries: [
          dir("src", { modified: at("2026-07-26T20:21:15") }),
          dir("tests", { modified: at("2026-07-24T10:02:41") }),
          file("Cargo.lock", {
            size: 18_204,
            modified: at("2026-07-26T20:19:02"),
          }),
          file("Cargo.toml", {
            size: 612,
            modified: at("2026-07-18T22:47:55"),
          }),
          file("LICENSE", {
            size: 1_069,
            modified: at("2026-06-02T09:14:10"),
          }),
          file("README.md", {
            size: 703,
            modified: at("2026-07-27T12:41:41"),
          }),
        ],
      }),
      pane({
        path: "/Users/demo/Downloads",
        focused: "..",
        entries: [],
      }),
    ],
    preview: viewer({ vfs_id: 0, path: focused }, "markdown"),
  }),
  files: { [focused]: { url: readme, mime: "text/markdown" } },
};

export default scene;
