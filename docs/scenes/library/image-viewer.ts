import { viewer } from "../builders";
import icon from "../../../app-icon.png?url";
import type { Scene } from "../types";

const path = "/Users/demo/Pictures/app-icon.png";

const scene: Scene = {
  description: "Image viewer",
  size: { width: 820, height: 620 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "image"),
  files: { [path]: { url: icon, mime: "image/png" } },
};

export default scene;
