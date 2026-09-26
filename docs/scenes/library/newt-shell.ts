import { mainWindow, pane } from "../builders";
import {
  LOCAL_STATS,
  PROJECT,
  projectEntries,
  sourceEntries,
  docsColumns,
} from "../demo";
import type { Scene } from "../types";

const E = "\x1b[";
const prompt = `${E}1;32mdemo@studio${E}0m ${E}1;34m~/src/walker/src${E}0m ${E}33m(main)${E}0m $ `;

const selected = ["cli.rs", "filter.rs", "walk.rs"];

const scene: Scene = {
  description: "Driving the panes from the shell with the newt command",
  size: { width: 1100, height: 600 },
  ...docsColumns(1100),
  window: "main",
  state: mainWindow({
    terminals: [1],
    terminalFocused: true,
    panes: [
      pane({
        path: `${PROJECT}/src`,
        focused: "walk.rs",
        selected,
        gitBranch: "main",
        fsStats: LOCAL_STATS,
        git: {
          "cli.rs": "modified",
          "filter.rs": "modified",
          "walk.rs": "modified",
        },
        entries: sourceEntries(),
      }),
      pane({ path: PROJECT, entries: projectEntries() }),
    ],
  }),
  terminals: {
    1: [
      `${prompt}newt pwd`,
      `${PROJECT}/src`,
      `${prompt}git diff --name-only --relative | newt select`,
      `${prompt}newt focus walk.rs`,
      prompt,
    ].join("\n"),
  },
};

export default scene;
