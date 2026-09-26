import { editor } from "../builders";
import walk from "../fixtures/walk.rs?url";
import type { Scene } from "../types";

const path = "/Users/demo/src/walker/src/walk.rs";

const scene: Scene = {
  description: "The built-in editor on a Rust source file",
  size: { width: 900, height: 640 },
  window: "editor",
  state: editor({ vfs_id: 0, path }, "rust"),
  files: { [path]: { url: walk, mime: "text/x-rust" } },
};

export default scene;
