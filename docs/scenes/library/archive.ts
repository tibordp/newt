import { at, dir, file, kb, mainWindow, pane, type VfsInfo } from "../builders";
import { HOME, LOCAL_STATS, docsColumns } from "../demo";
import type { Scene } from "../types";

const archivePath = `${HOME}/Downloads/walker-0.4.2.tar.zst`;
const ARCHIVE: VfsInfo = {
  id: 2,
  name: "Archive",
  hostLocal: false,
  display: (p) => `${archivePath}${p === "/" ? "" : p}`,
  rootLabel: "/",
};

const tarEntry = { user: { name: "demo" }, group: { name: "staff" } };

const inside = pane({
  path: "/walker-0.4.2",
  vfs: ARCHIVE,
  focused: "Cargo.toml",
  entries: [
    dir("benches", { ...tarEntry, modified: at("2026-07-09T11:20:14") }),
    dir("examples", { ...tarEntry, modified: at("2026-06-30T16:02:51") }),
    dir("src", { ...tarEntry, modified: at("2026-07-21T09:55:50") }),
    dir("tests", { ...tarEntry, modified: at("2026-07-20T18:44:05") }),
    file("Cargo.lock", {
      ...tarEntry,
      size: 40_918,
      modified: at("2026-07-21T09:40:12"),
    }),
    file("Cargo.toml", {
      ...tarEntry,
      size: 902,
      modified: at("2026-07-21T09:40:12"),
    }),
    file("CHANGELOG.md", {
      ...tarEntry,
      size: 6_844,
      modified: at("2026-07-21T09:39:57"),
    }),
    file("LICENSE", {
      ...tarEntry,
      size: 35_149,
      modified: at("2026-06-02T10:09:40"),
    }),
    file("README.md", {
      ...tarEntry,
      size: 4_512,
      modified: at("2026-07-20T22:03:31"),
    }),
  ],
});
// Origin breadcrumbs: the host path up to the archive, then the inside.
inside.breadcrumbs = [
  { label: "/", nav_path: "/" },
  { label: "Users/", nav_path: "/" },
  { label: "demo/", nav_path: "/" },
  { label: "Downloads/", nav_path: "/" },
  { label: "walker-0.4.2.tar.zst/", nav_path: "/" },
  { label: "walker-0.4.2", nav_path: "/walker-0.4.2" },
];

const scene: Scene = {
  description: "Browsing inside a compressed TAR archive",
  size: { width: 1000, height: 480 },
  ...docsColumns(1000),
  window: "main",
  state: mainWindow({
    panes: [
      inside,
      pane({
        path: `${HOME}/Downloads`,
        focused: "walker-0.4.2.tar.zst",
        fsStats: LOCAL_STATS,
        entries: [
          file("walker-0.4.2.tar.zst", {
            size: kb(612.4),
            modified: at("2026-07-21T09:41:30"),
          }),
          file("ubuntu-24.04.2-live-server-amd64.iso", {
            size: 3_213_064_192,
            modified: at("2026-06-14T12:03:44"),
          }),
          file("release-notes.md", {
            size: 2_114,
            modified: at("2026-07-21T09:43:02"),
          }),
        ],
      }),
    ],
  }),
};

export default scene;
