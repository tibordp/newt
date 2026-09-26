import { at, dir, file, mainWindow, pane } from "../builders";
import { docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "The command palette filtered to copy commands",
  size: { width: 1000, height: 640 },
  ...docsColumns(1000),
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: "/Users/demo/Documents",
        focused: "Invoices",
        entries: [
          dir("Invoices", { modified: at("2026-07-02T09:12:00") }),
          dir("Photos", { modified: at("2026-06-21T18:40:00") }),
          dir("Taxes", { modified: at("2026-04-11T14:05:00") }),
          file("budget-2026.xlsx", {
            size: 48_211,
            modified: at("2026-07-27T21:19:00"),
          }),
          file("lease.pdf", {
            size: 1_204_338,
            modified: at("2025-11-03T10:00:00"),
          }),
          file("notes.md", {
            size: 3_902,
            modified: at("2026-07-28T08:44:00"),
          }),
        ],
      }),
      pane({
        path: "/Volumes/Backup/Documents",
        entries: [
          dir("Invoices", { modified: at("2026-06-30T23:00:00") }),
          dir("Taxes", { modified: at("2026-04-11T14:05:00") }),
        ],
      }),
    ],
    modal: {
      type: "command_palette",
      data: { category_filter: null },
      context: { pane_handle: 0 },
    },
  }),
  steps: [{ type: "copy" }],
};

export default scene;
