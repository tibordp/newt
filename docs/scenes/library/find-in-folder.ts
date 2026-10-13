import { behindDialog, docsColumns, PROJECT } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description:
    "The Find in Folder dialog, searching Rust files changed in the last two weeks for TODO",
  size: { width: 1000, height: 900 },
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
        exclude_pattern: "target",
        content_pattern: "TODO",
        content_is_regex: false,
        case_sensitive: false,
        whole_word: false,
        not_containing: false,
        follow_symlinks: false,
        skip_hidden: false,
        gitignore: true,
        one_file_system: false,
        skip_packages: false,
        filters: {
          kind: "any",
          min_size: null,
          max_size: null,
          modified: {
            newer_than: { ago: { seconds: 14 * 86_400 } },
            older_than: null,
          },
          accessed: { newer_than: null, older_than: null },
          created: { newer_than: null, older_than: null },
          user: null,
          group: null,
          mode: null,
          attributes: 0,
          max_depth: null,
          limit: null,
        },
        content_size_cap: 10 * 1024 * 1024,
      },
      defaults: {
        case_sensitive: false,
        content_is_regex: false,
        whole_word: false,
        follow_symlinks: false,
        skip_hidden: false,
        gitignore: true,
        one_file_system: false,
        skip_packages: false,
      },
      metadata_traits: { unix_owner: true, windows_attributes: false },
    },
  }),
};

export default scene;
