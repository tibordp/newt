import { behindDialog, docsColumns, HOME, PROJECT } from "../demo";
import type { HotPathEntry } from "../../../src/lib/bindings";
import type { Scene } from "../types";

const entry = (
  path: string,
  category: HotPathEntry["category"],
  name: string | null = null,
  vfs_id = 0,
  display_path = path,
): HotPathEntry => ({
  path: { vfs_id, path },
  display_path,
  name,
  category,
  bookmark_key: category === "UserBookmark" ? path : null,
});

const scene: Scene = {
  description: "The Hot Paths palette with bookmarks, folders and mounts",
  size: { width: 1000, height: 600 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({ type: "hot_paths", context: { pane_handle: 0 } }),
  commands: {
    get_hot_paths: () => [
      entry(PROJECT, "UserBookmark", "Walker"),
      entry(`${HOME}/src/homelab/ansible`, "UserBookmark", "Ansible"),
      entry(HOME, "StandardFolder", "Home"),
      entry(`${HOME}/Desktop`, "StandardFolder", "Desktop"),
      entry(`${HOME}/Documents`, "StandardFolder", "Documents"),
      entry(`${HOME}/Downloads`, "StandardFolder", "Downloads"),
      entry("/Volumes/Backup", "Mount", "Backup"),
      entry("/walker", "Mount", null, 1, "s3://demo.backup/walker"),
      entry(`${HOME}/Pictures/2026-07 Lisbon`, "RecentFolder"),
      entry(`${HOME}/src/newt`, "RecentFolder"),
    ],
  },
};

export default scene;
