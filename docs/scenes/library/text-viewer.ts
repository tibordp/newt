import { viewer } from "../builders";
import notes from "../fixtures/release-notes.md?url";
import type { Scene } from "../types";

const path = "/Users/demo/Downloads/release-notes.md";

const scene: Scene = {
  description: "Text viewer on a Markdown file",
  size: { width: 820, height: 620 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "text"),
  files: { [path]: { url: notes, mime: "text/markdown" } },
};

export default scene;
