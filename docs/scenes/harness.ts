/// App-frame entry for a scene: stands in for the Tauri backend with
/// `mockIPC`, then boots the real app on the scene window's route.

import type { Channel } from "@tauri-apps/api/core";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import { marked } from "marked";

import type {
  FileDetails,
  MainWindowState,
  MarkdownNode,
  ResolvedPreferences,
  RuntimeState,
  VfsPath,
  TableDelimiter,
  ViewerMode,
  ViewerState,
} from "../../src/lib/bindings";
import { scenes } from "./registry";
import type { SceneStatus } from "./types";

type Defaults = {
  preferences: ResolvedPreferences;
  runtime_state: RuntimeState;
};

const DEFAULTS_URL = "/target/scenes/defaults.json";

/// Rendered only once content arrived by a route other than IPC. Monaco
/// shows one empty line before the model is filled, so a one-line editor
/// fixture needs its own `ready`.
const READY: Record<string, string | undefined> = {
  editor: ".monaco-editor .view-line + .view-line",
};

/// Commands the app fires for side effects the shot doesn't show.
const IGNORED = new Set([
  "init",
  "zoom",
  "focus",
  "set_viewport",
  "terminal_focus",
  "terminal_write",
  "set_editor_dirty",
  "update_runtime_state",
  "cancel_dnd",
]);

const status: SceneStatus = {
  steps: [],
  errors: [],
  unmocked: [],
  settled: () => settled(),
};
(window as unknown as { __scene: SceneStatus }).__scene = status;

const fail = (message: string): never => {
  status.errors.push(message);
  throw new Error(message);
};

const name = new URLSearchParams(location.search).get("scene") ?? "";
const scene = scenes[name] ?? fail(`no scene named "${name}"`);
const readySelectors = [READY[scene.window], scene.ready].filter(
  (s): s is string => !!s,
);
status.steps = scene.steps ?? [];

const response = await fetch(DEFAULTS_URL);
if (!response.headers.get("content-type")?.includes("json")) {
  fail(
    `${DEFAULTS_URL} is missing; \`npm run screenshots\` writes it, or run ` +
      "`cargo run -p newt --features specta-bindings -- " +
      "--export-scene-defaults target/scenes/defaults.json`",
  );
}
const defaults = (await response.json()) as Defaults;
const preferences = defaults.preferences;
preferences.locale = "en-US";
scene.preferences?.(preferences);
const runtimeState = defaults.runtime_state;
scene.runtimeState?.(runtimeState);

const state = structuredClone(scene.state);
const event = { main: "main_window", viewer: "viewer", editor: "editor" }[
  scene.window
];
let version = 0;
/// A copy each time: the mock event bus hands the page this very object,
/// and a mutated one in place reads as unchanged.
const publish = () => {
  published = true;
  return emit(`update:${event}`, {
    state: structuredClone(state),
    version: version++,
  });
};

const reportTitle = (title: string) =>
  window.parent.postMessage({ sceneTitle: scene.title ?? title }, "*");

const fixture = (path: VfsPath) =>
  scene.files?.[path.path] ?? fail(`no fixture for ${path.path}`);

const fixtureBytes = new Map<string, Promise<Uint8Array>>();
/// `render_markdown`, with marked standing in for comrak; the fixtures are
/// trusted, so nothing is sanitized.
const renderMarkdown = async (path: VfsPath): Promise<MarkdownNode[]> => {
  const source = new TextDecoder().decode(await bytesOf(path));
  const html = marked.parse(source, { gfm: true, async: false });
  const body = new DOMParser().parseFromString(html, "text/html").body;
  const toNode = (node: Node): MarkdownNode =>
    node instanceof Element
      ? {
          tag: node.localName,
          attrs: [...node.attributes].map((a) => [a.name, a.value]),
          children: [...node.childNodes].map(toNode),
        }
      : (node.textContent ?? "");
  return [...body.childNodes]
    .filter((node) => node instanceof Element || node.textContent?.trim())
    .map(toNode);
};

const bytesOf = (path: VfsPath) => {
  const url = fixture(path).url;
  if (!fixtureBytes.has(url)) {
    fixtureBytes.set(
      url,
      fetch(url)
        .then((r) => r.arrayBuffer())
        .then((b) => new Uint8Array(b)),
    );
  }
  return fixtureBytes.get(url)!;
};

// `buildFileUrl` appends `/<vfs_id>` and then replaces the query, so a base
// ending in `?` resolves to the fixture itself.
const pointAtFixture = (viewer: ViewerState) => {
  if (!viewer.file_path) return;
  const url = new URL(fixture(viewer.file_path).url, location.href);
  viewer.file_server_base = `${url.href}?`;
};
if (scene.window === "viewer") pointAtFixture(state as ViewerState);
if (scene.window === "main") pointAtFixture((state as MainWindowState).preview);

const terminalsWritten = new Set<number>();
let terminalOutput: Channel<ArrayBuffer> | undefined;

const handlers: Record<string, (args: any) => unknown> = {
  ping: () => {
    void publish();
    return null;
  },
  get_preferences: () => preferences,
  get_runtime_state: () => runtimeState,
  file_details: async ({ path }: { path: VfsPath }): Promise<FileDetails> => ({
    size: (await bytesOf(path)).length,
    mime_type: fixture(path).mime,
    is_dir: false,
    is_symlink: false,
    symlink_target: null,
    user: { name: "demo" },
    group: { name: "staff" },
    mode: 0o100644,
    modified: null,
    accessed: null,
    created: null,
  }),
  // Raw responses arrive as an ArrayBuffer.
  read_file: async ({ path }: { path: VfsPath }) =>
    (await bytesOf(path)).slice().buffer,
  render_markdown: ({ path }: { path: VfsPath }) => renderMarkdown(path),
  read_file_range: async ({
    path,
    offset,
    length,
  }: {
    path: VfsPath;
    offset: number;
    length: number;
  }) => (await bytesOf(path)).slice(offset, offset + length).buffer,
  sniff_viewer_encoding: () => {
    const viewer = state as ViewerState;
    viewer.encoding.detected ??= { encoding: "UTF-8", bom_len: 0 };
    void publish();
    return null;
  },
  report_table_detection: ({
    delimiter,
    header,
  }: {
    delimiter: TableDelimiter;
    header: boolean;
  }) => {
    const viewer = state as ViewerState;
    viewer.table.detected_delimiter = delimiter;
    viewer.table.detected_header = header;
    void publish();
    return null;
  },
  // The viewer pushes its auto-detected mode on load, and shows it until
  // the state echoes it back, as Rust's does; `settled` then applies the
  // scene's mode.
  set_viewer_mode: ({ mode }: { mode: ViewerMode }) => {
    (state as ViewerState).mode = mode;
    void publish();
    return null;
  },
  // Quick View: the main window's counterparts, on its `preview`.
  sniff_preview_encoding: () => {
    const { preview } = state as MainWindowState;
    preview.encoding.detected ??= { encoding: "UTF-8", bom_len: 0 };
    void publish();
    return null;
  },
  report_preview_table_detection: ({
    delimiter,
    header,
  }: {
    delimiter: TableDelimiter;
    header: boolean;
  }) => {
    const { preview } = state as MainWindowState;
    preview.table.detected_delimiter = delimiter;
    preview.table.detected_header = header;
    void publish();
    return null;
  },
  set_preview_mode: ({ mode }: { mode: ViewerMode }) => {
    (state as MainWindowState).preview.mode = mode;
    void publish();
    return null;
  },
  preview_focused: () => null,
  set_preview_encoding: () => null,
  set_preview_table_option: () => null,
  encoding_catalogue: () => [
    { label: "Unicode", encodings: ["UTF-8", "UTF-16LE", "UTF-16BE"] },
  ],
  set_editor_language: () => {
    void publish();
    return null;
  },
  set_window_title: ({ title }: { title: string }) => {
    reportTitle(title);
    return null;
  },
  attach_terminal_output: ({ channel }: { channel: Channel<ArrayBuffer> }) => {
    terminalOutput = channel;
    return null;
  },
  terminal_resize: ({ handle }: { handle: number }) => {
    const output = scene.window === "main" && scene.terminals?.[handle];
    if (output && terminalOutput && !terminalsWritten.has(handle)) {
      terminalsWritten.add(handle);
      const data = new TextEncoder().encode(output.replace(/\r?\n/g, "\r\n"));
      const frame = new Uint8Array(4 + data.length);
      new DataView(frame.buffer).setUint32(0, handle, true);
      frame.set(data, 4);
      terminalOutput.onmessage(frame.buffer);
    }
    return null;
  },
  "plugin:dialog|message": ({ message }: { message: string }) => {
    status.errors.push(`error dialog: ${message}`);
    return null;
  },
  ...scene.commands,
};

let inFlight = 0;
let published = false;

mockWindows("main");
mockIPC(
  async (cmd, args) => {
    const handler = handlers[cmd];
    if (handler) {
      inFlight++;
      try {
        return await handler(args ?? {});
      } finally {
        inFlight--;
      }
    }
    if (!IGNORED.has(cmd) && !status.unmocked.includes(cmd)) {
      status.unmocked.push(cmd);
      console.warn(`[scene] unmocked command: ${cmd}`);
    }
    return null;
  },
  { shouldMockEvents: true },
);

if (scene.window === "main") reportTitle(scene.state.window_title);
// Rust titles a viewer window when opening it; the webview never does.
if (scene.window === "viewer") {
  reportTitle(`${scene.state.display_path ?? ""} - Viewer`);
}

const frame = () => new Promise((r) => requestAnimationFrame(r));

/// The viewer whose mode the scene names: the viewer window's own, or Quick
/// View's.
const sceneViewer = (s: typeof state): ViewerState | null =>
  scene.window === "viewer"
    ? (s as ViewerState)
    : scene.window === "main"
      ? (s as MainWindowState).preview
      : null;

/// Resolves once the app has nothing left to load (see `idle`). A scene
/// whose viewer mode isn't the detected one gets it then, the way a user's
/// switch would arrive, and settles again.
async function settled(): Promise<void> {
  await quiet();
  const viewer = sceneViewer(state);
  const mode = sceneViewer(scene.state)?.mode;
  if (viewer && mode && viewer.mode !== mode) {
    viewer.mode = mode;
    await publish();
    await quiet();
  }
}

/// State delivered, no IPC in flight, fonts and images loaded, the
/// window's ready selectors present — and still so after two frames, so
/// follow-up requests are caught.
async function quiet(): Promise<void> {
  const idle = () =>
    published &&
    inFlight === 0 &&
    document.fonts.status === "loaded" &&
    [...document.images].every((img) => img.complete) &&
    readySelectors.every((s) => document.querySelector(s));
  for (let stable = 0; stable < 2;) {
    await frame();
    stable = idle() ? stable + 1 : 0;
  }
}

history.replaceState(
  null,
  "",
  { main: "/", viewer: "/viewer", editor: "/editor" }[scene.window],
);
await import("../../src/main");
