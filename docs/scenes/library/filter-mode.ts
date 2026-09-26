import { mainWindow, pane } from "../builders";
import {
  LOCAL_STATS,
  PROJECT,
  projectEntries,
  sourceEntries,
  docsColumns,
} from "../demo";
import type { Scene } from "../types";

const pattern = "^(walk|filter)";

const scene: Scene = {
  description: "Filter mode narrowing a listing with a regular expression",
  size: { width: 1000, height: 480 },
  ...docsColumns(1000),
  // The filter box reports its text on mount; nothing to do with it here.
  commands: { set_filter: () => null },
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: `${PROJECT}/src`,
        focused: "walk.rs",
        fsStats: LOCAL_STATS,
        entries: sourceEntries().filter((f) =>
          new RegExp(pattern).test(f.name),
        ),
        override: { filter: pattern, filter_mode: "filter" },
      }),
      pane({ path: PROJECT, entries: projectEntries() }),
    ],
  }),
};

export default scene;
