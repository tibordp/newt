import { behindDialog, docsColumns, PROJECT } from "../demo";
import type { QuickOpenHit } from "../../../src/lib/bindings";
import type { Scene } from "../types";

// Ranked and highlighted as the backend ranks "visit" over the demo tree.
const hit = (
  rel: string,
  highlights: number[],
  is_dir = false,
): QuickOpenHit => ({
  rel,
  is_dir,
  highlights,
});
const results = [
  hit("src/visitor", [4, 5, 6, 7, 8], true),
  hit("docs/visitors.md", [5, 6, 7, 8, 9]),
  hit("tests/visitor.rs", [6, 7, 8, 9, 10]),
  hit("examples/visit_sizes.rs", [9, 10, 11, 12, 13]),
  hit("src/visitor/mod.rs", [4, 5, 6, 7, 8]),
  hit("src/visitor/sorted.rs", [4, 5, 6, 7, 8]),
  hit("src/visitor/parallel.rs", [4, 5, 6, 7, 8]),
];

const scene: Scene = {
  description: "Go to File finding entries under the project by name",
  size: { width: 1000, height: 600 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "quick_open",
    data: {
      root: { vfs_id: 0, path: PROJECT },
      root_display: PROJECT,
      update: {
        query: "visit",
        results,
        matched: results.length,
        walked: 1284,
        walking: false,
        unreadable: 0,
        truncated: false,
        error: null,
      },
    },
    context: { pane_handle: 0 },
  }),
  commands: {
    quick_open_query: () => null,
  },
  steps: [{ type: "visit" }],
};

export default scene;
