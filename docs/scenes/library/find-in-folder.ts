import { behindDialog, docsColumns, PROJECT } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "The Find in Folder dialog, searching Rust files for TODO",
  size: { width: 1000, height: 560 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "search",
    context: { pane_handle: 0 },
    data: {
      path: { vfs_id: 0, path: PROJECT },
      display_path: PROJECT,
      prefill: {
        name_pattern: "*.rs",
        content_pattern: "TODO",
        content_is_regex: false,
        case_sensitive: false,
        follow_symlinks: false,
        content_size_cap: 10 * 1024 * 1024,
      },
      defaults: {
        case_sensitive: false,
        content_is_regex: false,
        follow_symlinks: false,
      },
    },
  }),
};

export default scene;
