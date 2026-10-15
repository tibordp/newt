import { viewer } from "../builders";
import log from "../fixtures/access_log?url";
import type { Scene } from "../types";

const path = "/var/log/apache2/access.log";

const scene: Scene = {
  description: "Text viewer on a multi-gigabyte web server log",
  size: { width: 820, height: 620 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "text"),
  files: {
    [path]: { url: log, mime: "text/plain", size: 2_418_316_544 },
  },
};

export default scene;
