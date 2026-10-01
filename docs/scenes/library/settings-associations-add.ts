import type { AssociationRow } from "../../../src/lib/bindings";
import type { Scene } from "../types";
import base from "./settings-associations";

/// What `*.rtf` would inherit: nothing sets it yet.
const rtf = (kind: "file" | "directory"): AssociationRow => ({
  pattern: "*.rtf",
  kind,
  customized: false,
  shared_with: [],
  enter: { set: null, inherited: null, origin: { source: "default" } },
  format: { set: null, inherited: null, origin: { source: "default" } },
  viewer: { set: null, inherited: "text", origin: { source: "file_type" } },
  language: {
    set: null,
    inherited: "plaintext",
    origin: { source: "file_type" },
  },
});

const scene: Scene = {
  ...base,
  description: "Adding a pattern on the Associations tab",
  commands: {
    ...base.commands,
    association_row: (args) => rtf(args.kind as "file" | "directory"),
  },
  steps: [
    { click: 'role=tab[name="Associations"]' },
    { click: 'input[aria-label="Add a pattern"]' },
    { type: ".rtf" },
  ],
};

export default scene;
