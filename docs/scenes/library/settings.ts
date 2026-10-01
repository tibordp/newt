import { behindDialog } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "The Settings dialog",
  size: { width: 1100, height: 720 },
  window: "main",
  state: behindDialog({
    type: "settings",
    context: { pane_handle: 0 },
    data: { can_reveal: true, association: null },
  }),
};

export default scene;
