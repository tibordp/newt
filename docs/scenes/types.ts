import type {
  EditorState,
  MainWindowState,
  ResolvedPreferences,
  RuntimeState,
  ViewerState,
} from "../../src/lib/bindings";

/// Performed by the runner with Playwright, in order, once the scene has
/// rendered. Keys use Playwright's names ("Meta+F", "ArrowDown").
export type Step =
  | { press: string }
  | { type: string }
  | { click: string }
  | { waitFor: string };

/// Content served for a path the window reads (`file_details`,
/// `read_file`, the viewer's file server). `url` is a Vite asset URL,
/// typically `import x from "../fixtures/foo.rs?url"`.
export type Fixture = { url: string; mime: string | null };

type SceneCommon = {
  /// One line on what the shot shows; listed on the scene index page.
  description: string;
  /// Webview size in CSS px, excluding the title bar.
  size: { width: number; height: number };
  /// Title-bar text, when it differs from what the window would set.
  title?: string;
  /// Mutate the Rust defaults (`--export-scene-defaults`) in place.
  preferences?: (prefs: ResolvedPreferences) => void;
  runtimeState?: (state: RuntimeState) => void;
  /// Keyed by `VfsPath.path`.
  files?: Record<string, Fixture>;
  /// Extra IPC handlers by command name, e.g. `get_hot_paths`.
  commands?: Record<string, (args: Record<string, unknown>) => unknown>;
  /// Selector inside the app whose presence means the shot is ready, on
  /// top of the window's default.
  ready?: string;
  steps?: Step[];
  /// Defaults to both.
  schemes?: ("light" | "dark")[];
};

export type Scene = SceneCommon &
  (
    | {
        window: "main";
        state: MainWindowState;
        /// Raw terminal output by handle, written once the terminal
        /// reports its size. Plain `\n` is translated to CRLF.
        terminals?: Record<number, string>;
      }
    | { window: "viewer"; state: ViewerState }
    | { window: "editor"; state: EditorState }
  );

/// What the harness publishes on the app frame's `window.__scene` for the
/// runner.
export type SceneStatus = {
  steps: Step[];
  /// Error popups (`plugin:dialog|message`) and harness failures; a
  /// non-empty list fails the capture.
  errors: string[];
  unmocked: string[];
  settled: () => Promise<void>;
};
