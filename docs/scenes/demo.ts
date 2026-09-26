/// Shared demo content for scenes: the `demo` user's files, reused so the
/// screenshots across the docs describe one consistent machine.

import type {
  File,
  ResolvedPreferences,
  RuntimeState,
} from "../../src/lib/bindings";
import type { MainWindowState, ModalData } from "../../src/lib/bindings";
import {
  at,
  dir,
  file,
  gb,
  fsStats,
  kb,
  mb,
  mainWindow,
  pane,
} from "./builders";

export const HOME = "/Users/demo";
export const PROJECT = `${HOME}/src/walker`;
export const LOCAL_STATS = fsStats(gb(994.66), gb(297.71));

/// A small Rust project, the working directory of most main-window scenes.
export const projectEntries = (): File[] => [
  dir(".git", { modified: at("2026-07-28T17:30:02") }),
  dir("benches", { modified: at("2026-07-09T11:20:14") }),
  dir("docs", { modified: at("2026-07-17T00:58:44") }),
  dir("examples", { modified: at("2026-06-30T16:02:51") }),
  dir("src", { modified: at("2026-07-28T17:24:38") }),
  dir("target", { modified: at("2026-07-28T17:24:40") }),
  dir("tests", { modified: at("2026-07-26T20:21:15") }),
  file(".gitignore", { size: 402, modified: at("2026-06-02T10:11:12") }),
  file("Cargo.lock", { size: 41_227, modified: at("2026-07-28T16:52:10") }),
  file("Cargo.toml", { size: 936, modified: at("2026-07-18T22:47:55") }),
  file("CHANGELOG.md", { size: 7_318, modified: at("2026-07-27T13:36:02") }),
  file("LICENSE", { size: 35_149, modified: at("2026-06-02T10:09:40") }),
  file("README.md", { size: 4_696, modified: at("2026-07-27T12:41:41") }),
  file("rustfmt.toml", { size: 88, modified: at("2026-06-02T10:11:40") }),
];

/// `src/` of the project.
export const sourceEntries = (): File[] => [
  dir("fs", { modified: at("2026-07-26T20:21:15") }),
  dir("visitor", { modified: at("2026-07-28T17:24:38") }),
  file("cli.rs", { size: kb(6.1), modified: at("2026-07-21T09:55:50") }),
  file("error.rs", { size: kb(2.4), modified: at("2026-07-12T14:08:08") }),
  file("filter.rs", { size: kb(9.8), modified: at("2026-07-24T12:08:08") }),
  file("lib.rs", { size: kb(3.2), modified: at("2026-07-28T17:02:11") }),
  file("main.rs", { size: kb(1.7), modified: at("2026-07-14T18:16:35") }),
  file("walk.rs", { size: kb(14.3), modified: at("2026-07-28T17:24:38") }),
];

/// Holiday photos, for selection and copy scenes.
export const photoEntries = (): File[] => [
  ...[
    ["4102", 4.81, "09:12:40"],
    ["4103", 5.02, "09:13:05"],
    ["4107", 4.66, "10:41:18"],
    ["4111", 6.13, "11:02:57"],
    ["4118", 5.37, "12:30:22"],
    ["4120", 4.94, "12:31:48"],
    ["4126", 5.58, "16:47:03"],
    ["4131", 6.02, "18:05:39"],
    ["4132", 5.91, "18:06:11"],
    ["4140", 4.73, "19:22:50"],
  ].map(([n, size, time]) =>
    file(`IMG_${n}.HEIC`, {
      size: mb(size as number),
      modified: at(`2026-07-19T${time}`),
    }),
  ),
  file("IMG_4126.MOV", { size: mb(48.2), modified: at("2026-07-19T16:47:21") }),
  file("IMG_4131.MOV", { size: mb(61.7), modified: at("2026-07-19T18:05:58") }),
];

/// Docs windows are narrower than a real one, so trim the file list to
/// Name, Size, Date and Time and size them to fill each pane exactly —
/// no column clipped at the pane edge, no truncated 12-hour time.
export function docsColumns(windowWidth: number) {
  const time = 100;
  const date = 90;
  const size = 96;
  // Each pane gets half the window, less the divider and the scrollbar gutter.
  const name = Math.floor(windowWidth / 2) - time - date - size - 16;
  const widths = { name, size, modified_date: date, modified_time: time };
  return {
    preferences: (prefs: ResolvedPreferences) => {
      prefs.settings.appearance!.columns = [
        "name",
        "size",
        "modified_date",
        "modified_time",
      ];
    },
    runtimeState: (state: RuntimeState) => {
      state.column_widths = { "0": { ...widths }, "1": { ...widths } };
    },
  };
}

/// The window behind a dialog: the project on the left (active, focused on
/// `focused`), its `src/` on the right.
export function behindDialog(
  modal: ModalData,
  options: { focused?: string; selected?: string[] } = {},
): MainWindowState {
  return mainWindow({
    modal,
    panes: [
      pane({
        path: PROJECT,
        focused: options.focused ?? "src",
        selected: options.selected,
        gitBranch: "main",
        fsStats: LOCAL_STATS,
        entries: projectEntries(),
      }),
      pane({ path: `${PROJECT}/src`, entries: sourceEntries() }),
    ],
  });
}
