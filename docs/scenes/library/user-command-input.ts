import { behindDialog, docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "A user command asking for input before it runs",
  size: { width: 1000, height: 560 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog(
    {
      type: "user_command_input",
      context: { pane_handle: 0 },
      data: {
        command_index: 0,
        command_title: "Create checksum",
        prompts: [{ label: "Output file", default: "Cargo.lock.sha256" }],
        confirms: ["Create a SHA-256 file?"],
      },
    },
    { focused: "Cargo.lock" },
  ),
};

export default scene;
