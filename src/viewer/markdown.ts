import DOMPurify from "dompurify";
import { marked } from "marked";

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

/// GitHub's heading anchor: lowercase, punctuation dropped, each space a
/// hyphen.
export function slugify(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
}

/// Heading ids carry GitHub's prefix, as GitHub's do, so `#anchor` links
/// written for GitHub resolve the same way.
export const HEADING_ID_PREFIX = "user-content-";

/// Render GitHub-flavored Markdown to a sanitized DOM fragment, to be
/// inserted as is — never serialized and parsed again, which is where
/// mutation XSS lives. Headings get GitHub-style ids for `#anchor` links;
/// `imageUrl` maps each image's `src` to what the page may load, or null
/// to drop it.
export function renderMarkdown(
  source: string,
  imageUrl: (src: string) => string | null,
): DocumentFragment {
  const html = marked.parse(source, { gfm: true, async: false });
  const fragment = DOMPurify.sanitize(html, {
    RETURN_DOM_FRAGMENT: true,
    FORBID_TAGS: ["style", "form"],
    FORBID_ATTR: ["style"],
  });
  const seen = new Map<string, number>();
  fragment.querySelectorAll("h1, h2, h3, h4, h5, h6").forEach((heading) => {
    const slug = slugify(heading.textContent ?? "");
    const n = seen.get(slug) ?? 0;
    seen.set(slug, n + 1);
    heading.id = HEADING_ID_PREFIX + (n > 0 ? `${slug}-${n}` : slug);
  });
  fragment.querySelectorAll("img").forEach((img) => {
    const src = img.getAttribute("src");
    const url = src ? imageUrl(src) : null;
    if (url) img.setAttribute("src", url);
    else img.removeAttribute("src");
    img.removeAttribute("srcset");
  });
  return fragment;
}
