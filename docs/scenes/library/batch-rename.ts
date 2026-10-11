import { at, file, mainWindow, mb, pane } from "../builders";
import { HOME, LOCAL_STATS, docsColumns, photoEntries } from "../demo";
import type { BatchRenameRow } from "../../../src/lib/bindings";
import type { Scene } from "../types";

const folder = `${HOME}/Pictures/Lisbon`;

const entries = [
  ...photoEntries(),
  file("img_4132.HEIC", { size: mb(5.4), modified: at("2026-07-19T18:06:40") }),
  file("lisbon-4140.HEIC", {
    size: mb(4.7),
    modified: at("2026-07-20T08:00:00"),
  }),
  file("notes.txt", { size: 1840, modified: at("2026-07-20T21:14:02") }),
];
const selected = entries
  .map((f) => f.name)
  .filter((name) => name !== "lisbon-4140.HEIC");

/// What `IMG_(\d+)` → `lisbon-$1` makes of a name, case-insensitively.
function row(name: string): BatchRenameRow {
  const match = /IMG_(\d+)/i.exec(name);
  if (!match) {
    return {
      name,
      is_dir: false,
      new_name: null,
      matched: [],
      replaced: [],
      problem: null,
      folder: null,
    };
  }
  const replacement = `lisbon-${match[1]}`;
  const newName = replacement + name.slice(match[0].length);
  const problem =
    newName === "lisbon-4132.HEIC"
      ? {
          error: true,
          message: `${name === "IMG_4132.HEIC" ? "img_4132.HEIC" : "IMG_4132.HEIC"} gets the same name`,
        }
      : newName === "lisbon-4140.HEIC"
        ? { error: false, message: "Replaces an existing entry" }
        : null;
  return {
    name,
    is_dir: false,
    new_name: newName,
    matched: [[0, match[0].length]],
    replaced: [[0, replacement.length]],
    problem,
    folder: null,
  };
}

const rows = [...selected].sort().map(row);

const scene: Scene = {
  description:
    "Batch Rename on a folder of photos: a regex find & replace with a live preview, flagging a duplicate and a replaced file",
  size: { width: 1000, height: 640 },
  ...docsColumns(1000),
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: folder,
        entries,
        focused: "IMG_4102.HEIC",
        selected,
        fsStats: LOCAL_STATS,
      }),
      pane({ path: HOME, entries: [] }),
    ],
    modal: {
      type: "batch_rename",
      context: { pane_handle: 0 },
      data: {
        title: `${selected.length} items in ${folder}`,
        defaults: { case_sensitive: false, keep_case: false },
        preview: {
          total: rows.length,
          renamed: rows.filter((r) => r.new_name !== null).length,
          errors: rows.filter((r) => r.problem?.error).length,
          warnings: rows.filter((r) => r.problem && !r.problem.error).length,
          rule_error: null,
          copies: null,
          filter: "all",
          shown: rows.length,
          offset: 0,
          window: rows,
        },
      },
    },
  }),
  commands: {
    batch_rename_set_rule: () => null,
    batch_rename_set_viewport: () => null,
  },
  steps: [{ type: "IMG_(\\d+)" }, { press: "Tab" }, { type: "lisbon-$1" }],
};

export default scene;
