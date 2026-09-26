import { docsColumns } from "../demo";
import type { Scene } from "../types";
import pack from "./pack-to-s3";

/// pack-to-s3, moved to the background: the operation leaves the progress
/// dialog and is reported by the operations panel instead.
const state = structuredClone(pack.window === "main" ? pack.state : null)!;
for (const op of Object.values(state.operations)) {
  if (op) op.backgrounded = true;
}
state.foreground_operation_id = null;

const scene: Scene = {
  description: "A pack running in the background, in the operations panel",
  size: { width: 1100, height: 768 },
  ...docsColumns(1100),
  window: "main",
  state,
};

export default scene;
