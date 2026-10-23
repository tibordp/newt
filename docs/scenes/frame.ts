/// Outer document for a scene: window chrome around the app frame, for
/// `?platform=` (macOS by default). Without `?scene=`, an index of every
/// scene.

import "./frame.css";
import { loadFonts } from "./fonts";
import type { PlatformStyle } from "../../src/lib/bindings";
import { scenes } from "./registry";
import type { Scene } from "./types";

const params = new URLSearchParams(location.search);
const name = params.get("scene");
const scene = name ? scenes[name] : undefined;
const platform = params.get("platform") ?? "macos";
await loadFonts(platform as PlatformStyle);

/// How many window controls each title bar has, trailing; `frame.css`
/// draws them. macOS draws its lights leading, over the app's own strip.
const CONTROLS: Record<string, number> = {
  macos: 0,
  windows: 3,
  kde: 3,
};

/// The menu bar under the title bar: a viewer's or editor's menus, as Rust
/// builds them off macOS (`viewer::build_menu`, `editor::build_menu`).
/// GNOME has them in the header bar's primary menu instead.
function menuBar(scene: Scene): string[] {
  if (platform !== "windows" && platform !== "kde") return [];
  if (scene.window === "editor") return ["File", "View", "Language"];
  if (scene.window !== "viewer") return [];
  const { mode } = scene.state;
  return [
    "File",
    ...(["text", "hex", "table", "dissect"].includes(mode) ? ["Edit"] : []),
    "View",
    ...(mode === "table" ? ["Table"] : []),
    ...(["text", "table", "markdown"].includes(mode) ? ["Encoding"] : []),
  ];
}

/// Title bars the app draws itself, which its frame takes in: the strip
/// under macOS's title bar, GNOME's header bar (none of the frame's own).
const DRAWN_TITLEBAR: Record<string, number> = {
  macos: 32,
  gnome: 46,
};

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> = {},
  children: Node[] = [],
): HTMLElementTagNameMap[K] {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
}

if (scene) {
  const title = el("span", { className: "title" });
  const iframe = el("iframe", {
    src:
      `/docs/scenes/scene.html?scene=${encodeURIComponent(name!)}` +
      `&platform=${encodeURIComponent(platform)}`,
  });
  const overlay = DRAWN_TITLEBAR[platform] ?? 0;
  iframe.style.width = `${scene.size.width}px`;
  iframe.style.height = `${scene.size.height + overlay}px`;
  window.addEventListener("message", (e) => {
    if (e.source === iframe.contentWindow && "sceneTitle" in e.data) {
      title.textContent = e.data.sceneTitle;
    }
  });
  const menus = menuBar(scene);
  const controls = Array.from({ length: CONTROLS[platform] ?? 0 }, () =>
    el("i"),
  );
  document.body.append(
    el("div", { className: "backdrop" }, [
      el("div", { className: `window ${platform}` }, [
        el("div", { className: "titlebar" }, [
          el("span", { className: "lights" }, [el("i"), el("i"), el("i")]),
          title,
          el("span", { className: "controls" }, controls),
        ]),
        ...(menus.length
          ? [
              el(
                "div",
                { className: "menubar" },
                menus.map((label) => el("span", { textContent: label })),
              ),
            ]
          : []),
        iframe,
      ]),
    ]),
  );
} else {
  const entries = Object.entries(scenes).sort(([a], [b]) => a.localeCompare(b));
  document.body.append(
    el("div", { className: "index" }, [
      el("h1", { textContent: name ? `No scene "${name}"` : "Scenes" }),
      el(
        "ul",
        {},
        entries.map(([n, s]) =>
          el("li", {}, [
            el("a", {
              href: `?scene=${encodeURIComponent(n)}`,
              textContent: n,
            }),
            el("span", { textContent: ` — ${s.description}` }),
          ]),
        ),
      ),
    ]),
  );
}

/// For the runner.
(window as unknown as { __scenes: unknown }).__scenes = Object.fromEntries(
  Object.entries(scenes).map(([n, s]) => [
    n,
    { schemes: s.schemes ?? ["light", "dark"] },
  ]),
);
