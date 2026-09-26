/// Outer document for a scene: macOS window chrome around the app frame.
/// Without `?scene=`, an index of every scene.

import "./frame.css";
import { scenes } from "./registry";

const name = new URLSearchParams(location.search).get("scene");
const scene = name ? scenes[name] : undefined;

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
    src: `/docs/scenes/scene.html?scene=${encodeURIComponent(name!)}`,
  });
  iframe.style.width = `${scene.size.width}px`;
  iframe.style.height = `${scene.size.height}px`;
  window.addEventListener("message", (e) => {
    if (e.source === iframe.contentWindow && "sceneTitle" in e.data) {
      title.textContent = e.data.sceneTitle;
    }
  });
  document.body.append(
    el("div", { className: "backdrop" }, [
      el("div", { className: "window" }, [
        el("div", { className: "titlebar" }, [
          el("span", { className: "lights" }, [el("i"), el("i"), el("i")]),
          title,
        ]),
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
