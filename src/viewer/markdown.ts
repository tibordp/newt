import type { MarkdownNode } from "../lib/bindings";

/// What following a link in a rendered document does.
export type LinkTarget =
  | { kind: "web"; url: string }
  | { kind: "anchor"; id: string }
  | { kind: "file"; path: string }
  | { kind: "none" };

const WEB_SCHEMES = new Set(["http:", "https:", "mailto:"]);

/// `href` resolved against the directory of the document at wire path
/// `base`, `..` and `.` folded; a leading `/` is the filesystem root. The
/// query and fragment are dropped and percent-escapes decoded.
export function resolveRelative(base: string, href: string): string | null {
  const bare = href.split(/[?#]/)[0];
  if (!bare) return null;
  let decoded = bare;
  try {
    decoded = decodeURIComponent(bare);
  } catch {
    // A stray `%` is part of the name.
  }
  const parts = decoded.startsWith("/")
    ? []
    : base.split("/").filter(Boolean).slice(0, -1);
  for (const segment of decoded.split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") parts.pop();
    else parts.push(segment);
  }
  return "/" + parts.join("/");
}

/// Classify a link from a rendered document at wire path `base`. Only web
/// and mail URLs leave the viewer; any other scheme is ignored.
export function classifyLink(href: string | null, base: string): LinkTarget {
  if (!href) return { kind: "none" };
  if (href.startsWith("#")) {
    let id = href.slice(1);
    try {
      id = decodeURIComponent(id);
    } catch {
      // Keep it as written.
    }
    return { kind: "anchor", id };
  }
  const scheme = /^[a-z][a-z0-9+.-]*:/i.exec(href)?.[0].toLowerCase();
  if (scheme) {
    return WEB_SCHEMES.has(scheme)
      ? { kind: "web", url: href }
      : { kind: "none" };
  }
  if (href.startsWith("//")) return { kind: "none" };
  const path = resolveRelative(base, href);
  return path ? { kind: "file", path } : { kind: "none" };
}

/// The prefix of every id in a rendered document, as on GitHub; matches
/// `ID_PREFIX` in `src-tauri/src/viewer/markdown.rs`.
export const ID_PREFIX = "user-content-";

/// How long one slice of building may hold the main thread.
const SLICE_MS = 8;

/// A task boundary without `setTimeout`'s nesting clamp: input and
/// rendering get their turn between slices.
const nextTask = () =>
  new Promise<void>((resolve) => {
    const { port1, port2 } = new MessageChannel();
    port1.onmessage = () => resolve();
    port2.postMessage(null);
  });

/// Build a document from `render_markdown` into `parent`, a slice of
/// top-level blocks per task so a long one doesn't hold up input and paint.
/// `imageUrl` maps each image's `src` to what the page may load, or null to
/// drop it. Stops early once `cancelled()`.
export async function buildMarkdown(
  parent: Element,
  nodes: MarkdownNode[],
  imageUrl: (src: string) => string | null,
  cancelled: () => boolean,
): Promise<void> {
  let next = 0;
  while (next < nodes.length) {
    const start = performance.now();
    const slice = document.createDocumentFragment();
    while (next < nodes.length && performance.now() - start < SLICE_MS) {
      slice.appendChild(createNode(nodes[next++], imageUrl));
    }
    parent.appendChild(slice);
    if (next < nodes.length) {
      await nextTask();
      if (cancelled()) return;
    }
  }
}

function createNode(
  node: MarkdownNode,
  imageUrl: (src: string) => string | null,
): Node {
  if (typeof node === "string") return document.createTextNode(node);
  const el = document.createElement(node.tag);
  for (const [name, value] of node.attrs) {
    if (node.tag === "img" && name === "src") {
      const url = imageUrl(value);
      if (url) el.setAttribute("src", url);
    } else {
      el.setAttribute(name, value);
    }
  }
  // The CSS numbers lists itself (see DOCUMENT_CSS); carry `start` over.
  const start = node.tag === "ol" ? Number(el.getAttribute("start")) : NaN;
  if (Number.isFinite(start) && el.hasAttribute("start")) {
    el.style.counterReset = `item ${start - 1}`;
  }
  for (const child of node.children) {
    el.appendChild(createNode(child, imageUrl));
  }
  return el;
}
