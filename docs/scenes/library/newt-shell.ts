import { mainWindow, pane } from "../builders";
import {
  LOCAL_STATS,
  PROJECT,
  projectEntries,
  sourceEntries,
  docsColumns,
  shellPrompt,
} from "../demo";
import type { Scene } from "../types";

const prompt = shellPrompt(`${PROJECT}/src`, "main");

const selected = ["cli.rs", "filter.rs", "walk.rs"];
const columns = docsColumns(1100);

const scene: Scene = {
  description: "Driving the panes from the shell with the newt command",
  size: { width: 1100, height: 600 },
  ...columns,
  // The panes are the point; the terminal only needs its few lines.
  runtimeState: (state) => {
    columns.runtimeState(state);
    state.layout = { ...state.layout, terminal_height: 160 };
  },
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
