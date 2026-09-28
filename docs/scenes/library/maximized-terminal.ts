import { docsColumns } from "../demo";
import type { Scene } from "../types";
import terminal from "./terminal";

/// The terminal scene with the layout maximized: focus is in the shell, so
/// the terminal panel fills the window.
const state = structuredClone(
  terminal.window === "main" ? terminal.state : null,
)!;
state.display_options.maximized = true;

const scene: Scene = {
  description: "The terminal maximized, filling the window",
  size: { width: 1100, height: 768 },
  ...docsColumns(1100),
  window: "main",
  state,
  terminals: terminal.window === "main" ? terminal.terminals : undefined,
};

export default scene;
