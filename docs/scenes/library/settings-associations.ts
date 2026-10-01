import { behindDialog } from "../demo";
import type {
  AssociationCell,
  AssociationRow,
  AssociationTable,
  BrowseFormat,
  EnterChoice,
  Origin,
  ViewerMode,
} from "../../../src/lib/bindings";
import type { Scene } from "../types";

const builtIn: Origin = { source: "built_in" };
const fileType: Origin = { source: "file_type" };
const none: Origin = { source: "default" };

const cell = <T>(
  set: T | null,
  inherited: T | null = null,
  origin: Origin = none,
): AssociationCell<T> => ({ set, inherited, origin });

const row = (
  pattern: string,
  fields: Partial<{
    kind: "file" | "directory";
    customized: boolean;
    shared_with: string[];
    enter: AssociationCell<EnterChoice>;
    format: AssociationCell<BrowseFormat>;
    viewer: AssociationCell<ViewerMode>;
    language: AssociationCell<string>;
  }>,
): AssociationRow => ({
  pattern,
  kind: "file",
  customized: false,
  shared_with: [],
  enter: cell<EnterChoice>(null),
  format: cell<BrowseFormat>(null),
  viewer: cell(null, "text", fileType),
  language: cell(null, "plaintext", fileType),
  ...fields,
});

const browse: EnterChoice = { action: "browse" };

const table: AssociationTable = {
  name: null,
  candidate: null,
  rows: [
    row("build", {
      kind: "directory",
      customized: true,
      enter: cell({ action: "command", command: "Build" }),
    }),
    row("*.log", {
      customized: true,
      enter: cell({ action: "view" }),
      viewer: cell("text", "text", fileType),
    }),
    row("*.nupkg", {
      customized: true,
      shared_with: ["*.vsix"],
      enter: cell(browse),
      format: cell(null, "zip", builtIn),
      viewer: cell<ViewerMode>(null, null, fileType),
      language: cell<string>(null, null, fileType),
    }),
    row("*.vsix", {
      customized: true,
      shared_with: ["*.nupkg"],
      enter: cell(browse),
      format: cell(null, "zip", builtIn),
      viewer: cell<ViewerMode>(null, null, fileType),
      language: cell<string>(null, null, fileType),
    }),
    row("*.zip", {
      customized: true,
      enter: cell(null, browse, builtIn),
      format: cell(null, "zip", builtIn),
      viewer: cell("hex", null, fileType),
      language: cell<string>(null, null, fileType),
    }),
    row("*.7z", {
      enter: cell(null, browse, builtIn),
      format: cell(null, "7z", builtIn),
      viewer: cell<ViewerMode>(null, null, fileType),
    }),
    row("Dockerfile", { language: cell(null, "dockerfile", builtIn) }),
    row("*.docx", {
      format: cell(null, "zip", builtIn),
      viewer: cell<ViewerMode>(null, null, fileType),
      language: cell<string>(null, null, fileType),
    }),
    row("*.gz", {
      enter: cell(null, browse, builtIn),
      format: cell(null, "compressed", builtIn),
      viewer: cell<ViewerMode>(null, null, fileType),
    }),
    row("*.iso", {
      enter: cell(null, browse, builtIn),
      format: cell(null, "disc", builtIn),
      viewer: cell<ViewerMode>(null, null, fileType),
    }),
    row("*.rs", { language: cell(null, "rust", builtIn) }),
    row("*.tar.gz", {
      enter: cell(null, browse, builtIn),
      format: cell(null, "tar", builtIn),
      viewer: cell<ViewerMode>(null, null, fileType),
    }),
    row("*.ts", {
      viewer: cell(null, "text", builtIn),
      language: cell(null, "typescript", builtIn),
    }),
  ],
};

const scene: Scene = {
  description: "The Settings dialog's Associations tab",
  size: { width: 1100, height: 720 },
  window: "main",
  state: behindDialog({
    type: "settings",
    context: { pane_handle: 0 },
    data: { can_reveal: true, association: null },
  }),
  preferences: (prefs) => {
    prefs.user_commands = [
      { title: "Build", run: "make -C {{ file.path }}", terminal: true },
    ];
  },
  commands: {
    association_table: () => table,
    editor_languages: () => [
      { id: "plaintext", label: "Plain Text" },
      { id: "dockerfile", label: "Dockerfile" },
      { id: "rust", label: "Rust" },
      { id: "typescript", label: "TypeScript" },
    ],
  },
  steps: [{ click: 'role=tab[name="Associations"]' }],
};

export default scene;
