import { at, file, kb, mainWindow, pane, type VfsInfo } from "../builders";
import { PROJECT, projectEntries, docsColumns } from "../demo";
import type { Scene } from "../types";

const label = `${PROJECT} [*.rs · "TODO"]`;
const SEARCH: VfsInfo = {
  id: 3,
  name: "Search",
  hostLocal: false,
  display: () => label,
  rootLabel: label,
};

const hits: [string, number, string][] = [
  ["src/walk.rs", kb(14.3), "2026-07-28T17:24:38"],
  ["src/filter.rs", kb(9.8), "2026-07-24T12:08:08"],
  ["src/fs/metadata.rs", kb(5.6), "2026-07-26T20:21:15"],
  ["src/fs/windows.rs", kb(7.9), "2026-07-11T15:30:02"],
  ["src/visitor/mod.rs", kb(4.1), "2026-07-28T17:24:38"],
  ["tests/symlinks.rs", kb(3.3), "2026-07-26T19:58:10"],
  ["benches/deep_tree.rs", kb(2.2), "2026-07-09T11:20:14"],
];

const view = pane({
  path: "/",
  vfs: SEARCH,
  presorted: true,
  focused: "filter.rs",
  entries: hits.map(([rel, size, modified]) =>
    file(rel.split("/").pop()!, {
      size,
      modified: at(modified),
      source: { vfs_id: 0, path: `${PROJECT}/${rel}` },
    }),
  ),
});
// The "where from" column: the source's parent, as the backend renders it.
for (const item of view.file_window.items) {
  if (item.source) {
    item.source_display = item.source.path.replace(/\/[^/]*$/, "");
  }
}
view.breadcrumbs = [{ label, path: { ...view.path, path: "/" } }];

const scene: Scene = {
  description: "Find in Folder results as a flat result filesystem",
  size: { width: 1100, height: 520 },
  ...docsColumns(1100),
  window: "main",
  state: mainWindow({
    panes: [view, pane({ path: PROJECT, entries: projectEntries() })],
  }),
};

export default scene;
