import { viewer } from "../builders";
import icon from "../../../app-icon.png?url";
import type { Scene } from "../types";

const path = "/Users/demo/Pictures/app-icon.png";

const scene: Scene = {
  description: "Hex viewer on a PNG",
  size: { width: 820, height: 560 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "hex"),
  files: { [path]: { url: icon, mime: "image/png" } },
};

export default scene;
