import { viewer } from "../builders";
import readme from "../fixtures/README.md?url";
import type { Scene } from "../types";

const path = "/Users/demo/src/brewlog/README.md";

const scene: Scene = {
  description: "Markdown viewer, rendered",
  size: { width: 820, height: 620 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "markdown"),
  files: { [path]: { url: readme, mime: "text/markdown" } },
};

export default scene;
