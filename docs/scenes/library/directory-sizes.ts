import { at, dir, file, gb, mainWindow, pane } from "../builders";
import { HOME, LOCAL_STATS, projectEntries, docsColumns } from "../demo";
import type { Scene } from "../types";
import type { PaneViewState } from "../../../src/lib/bindings";

/// Adds `recursive_size` annotations, as the du enricher does.
function withSizes(
  view: PaneViewState,
  sizes: Record<string, number>,
): PaneViewState {
  for (const item of view.file_window.items) {
    const bytes = sizes[item.name];
    if (bytes != null) {
      item.annotations = [
        ...item.annotations,
        { recursive_size: { bytes, complete: true, unreadable: 0 } },
      ];
    }
  }
  return view;
}

const layout = docsColumns(1000);

const scene: Scene = {
  description: "Calculated directory sizes in the Size column",
  size: { width: 1000, height: 520 },
  window: "main",
  preferences: (prefs) => {
    layout.preferences(prefs);
    prefs.settings.appearance!.si_size_prefixes = true;
  },
  runtimeState: layout.runtimeState,
  state: mainWindow({
    panes: [
      withSizes(
        pane({
          path: `${HOME}/src`,
          focused: "walker",
          sorting: { key: "name", asc: true },
          fsStats: LOCAL_STATS,
          entries: [
            dir("dotfiles", { modified: at("2026-07-02T21:03:11") }),
            dir("homelab", { modified: at("2026-07-22T19:47:30") }),
            dir("newt", { modified: at("2026-07-28T17:30:02") }),
            dir("scratch", { modified: at("2026-07-27T23:12:45") }),
            dir("walker", { modified: at("2026-07-28T17:24:40") }),
            dir("website", { modified: at("2026-07-25T10:31:08") }),
            file("notes.md", {
              size: 3_902,
              modified: at("2026-07-28T08:44:00"),
            }),
          ],
        }),
        {
          dotfiles: 1_842_117,
          homelab: 38_401_922,
          newt: gb(14.82),
          scratch: 212_404_881,
          walker: gb(1.27),
          website: 486_210_004,
        },
      ),
      pane({ path: `${HOME}/src/walker`, entries: projectEntries() }),
    ],
  }),
};

export default scene;
