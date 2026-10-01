import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import { ContextMenu as CM } from "../lib/menus";
import styles from "./Viewer.module.scss";
import menuStyles from "../main_window/Menu.module.scss";
import { commands } from "../lib/bindings";
import { safe, unwrap, unwrapBytes } from "../lib/ipc";
import { useFormatBytes } from "../lib/size";
import { ModeToggle } from "./ModeToggle";
import {
  SNIFF_PREFIX_LEN,
  buildFileUrl,
  makeDecoder,
  type ViewerMode,
  type VfsPath,
} from "./helpers";
import {
  HEADING_ID_PREFIX,
  classifyLink,
  renderMarkdown,
  resolveRelative,
} from "./markdown";
import { useViewerHost } from "./host";
import { REFOCUS_EVENT } from "../main_window/types";

/// Larger files open as text: rendering is all-at-once.
const MAX_RENDER_BYTES = 4 * 1024 * 1024;

/// The document's stylesheet, scoped to its shadow root. The app's theme
/// tokens inherit into it, so it follows theme changes as they happen.
const DOCUMENT_CSS = `
:host { display: block; }
article { max-width: 880px; margin: 0 auto; padding: 24px 32px 48px;
  color: var(--color-fg); overflow-wrap: break-word;
  font: 15px/1.6 system-ui, -apple-system, "Segoe UI", sans-serif;
  user-select: text; -webkit-user-select: text; cursor: auto; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 1.4em 0 0.6em; }
article > :first-child { margin-top: 0; }
h1, h2 { padding-bottom: 0.3em; border-bottom: 1px solid var(--color-border); }
h1 { font-size: 2em; } h2 { font-size: 1.5em; } h3 { font-size: 1.25em; }
a { color: var(--color-accent); text-decoration: none; cursor: pointer; }
a:hover { text-decoration: underline; }
code, pre { font-family: var(--font-mono); font-size: 0.875em; }
code { background: var(--color-chrome); padding: 0.15em 0.35em; border-radius: 4px; }
pre { background: var(--color-chrome); padding: 12px 16px; border-radius: 6px; overflow: auto; }
pre code { background: none; padding: 0; }
blockquote { margin: 0 0 1em; padding: 0 1em; color: var(--color-fg-muted);
  border-left: 3px solid var(--color-border); }
table { border-collapse: collapse; display: block; overflow: auto; margin: 0 0 1em; }
th, td { border: 1px solid var(--color-border); padding: 6px 12px; }
th { background: var(--color-chrome); }
img { max-width: 100%; }
hr { border: none; border-top: 1px solid var(--color-border); margin: 1.5em 0; }
li > input[type=checkbox] { margin-right: 0.4em; }
/* Bullets and numbers are generated content, not list markers: WebKit
   paints a selection set from script over ::marker but not one made by
   dragging, and leaves it painted once the selection is gone. Generated
   content is never part of a selection. */
/* Blocks off screen skip style, layout and paint until they scroll into
   view; until then they count as a few lines tall. */
article > * { content-visibility: auto; contain-intrinsic-size: auto 3em; }
/* Containment keeps a block's edge margins inside it instead of letting
   them merge with its own, so drop the ones that used to escape. */
blockquote > :first-child,
article > :is(ul, ol) > li:first-child > :first-child { margin-top: 0; }
blockquote > :last-child,
article > details > :last-child,
article > :is(ul, ol) > li:last-child > :last-child { margin-bottom: 0; }
ul, ol { list-style: none; padding-left: 2em; }
li { position: relative; }
/* Shapes drawn with borders, which forced colors keep. */
ul > li::before { content: ""; position: absolute; right: calc(100% + 0.7em);
  top: 0.62em; box-sizing: border-box; width: 0.36em; height: 0.36em;
  border: 0.18em solid currentColor; border-radius: 50%; }
ul ul > li::before { border-width: 1px; }
ul ul ul > li::before { box-sizing: content-box; width: 0; height: 0;
  border-width: 0.18em; border-radius: 0; }
ol { counter-reset: item; }
ol > li { counter-increment: item; }
ol > li::before { content: counter(item) "."; position: absolute;
  right: calc(100% + 0.4em); font-variant-numeric: tabular-nums; }
`;

interface MarkdownViewerProps {
  filePath: string;
  vfsPath: VfsPath;
  fileServerBase: string;
  fileSize: number;
  autoMode: ViewerMode;
  encoding: string;
  bomLen: number;
  encodingLabel: string | null;
  needsSniff: boolean;
}

/**
 * Rendered GitHub-flavored Markdown, sanitized and placed in a shadow
 * root: the document's styles and ids stay out of the viewer's, and the
 * viewer's CSP forbids inline script. Web links open in the browser,
 * relative ones open the file in this window, anchors scroll.
 */
export function MarkdownViewer({
  filePath,
  vfsPath,
  fileServerBase,
  fileSize,
  autoMode,
  encoding,
  bomLen,
  encodingLabel,
  needsSniff,
}: MarkdownViewerProps) {
  const formatSize = useFormatBytes();
  const viewerHost = useViewerHost();
  const hostRef = useRef<HTMLDivElement>(null);
  // The selection as the menu opened: opening it moves focus, which
  // drops a selection in the document's shadow tree.
  const menuSelection = useRef<{
    text: string;
    range: StaticRange | null;
  } | null>(null);
  const [hasSelection, setHasSelection] = useState(false);
  const [bytes, setBytes] = useState<Uint8Array | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    if (fileSize > MAX_RENDER_BYTES) {
      safe(viewerHost.setMode("text"));
      return;
    }
    let cancelled = false;
    setBytes(null);
    setNotice(null);
    (async () => {
      try {
        const data = await unwrapBytes(
          commands.readFile(vfsPath, MAX_RENDER_BYTES),
        );
        if (cancelled) return;
        if (needsSniff) {
          safe(
            viewerHost.sniffEncoding(
              Array.from(data.subarray(0, SNIFF_PREFIX_LEN)),
              true,
            ),
          );
        }
        setBytes(data);
      } catch (e) {
        if (!cancelled) setNotice(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filePath, fileSize]);

  const imageUrl = useCallback(
    (src: string): string | null => {
      if (/^data:image\//i.test(src) || /^https?:/i.test(src)) return src;
      if (/^[a-z][a-z0-9+.-]*:/i.test(src) || src.startsWith("//")) {
        return null;
      }
      const path = resolveRelative(vfsPath.path, src);
      return path ? buildFileUrl(fileServerBase, vfsPath.vfs_id, path) : null;
    },
    [fileServerBase, vfsPath],
  );

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const root = host.shadowRoot ?? host.attachShadow({ mode: "open" });
    const style = document.createElement("style");
    style.textContent = DOCUMENT_CSS;
    const article = document.createElement("article");
    if (bytes) {
      const source = makeDecoder(encoding).decode(bytes.subarray(bomLen));
      article.appendChild(renderMarkdown(source, imageUrl));
      // The CSS numbers lists itself (see DOCUMENT_CSS); carry `start` over.
      for (const ol of article.querySelectorAll("ol[start]")) {
        const start = Number(ol.getAttribute("start"));
        if (Number.isFinite(start)) {
          (ol as HTMLElement).style.counterReset = `item ${start - 1}`;
        }
      }
    }
    root.replaceChildren(style, article);
    host.scrollTop = 0;
  }, [bytes, encoding, bomLen, imageUrl]);

  const followLink = useCallback(
    (href: string | null) => {
      const target = classifyLink(href, vfsPath.path);
      switch (target.kind) {
        case "web":
          safe(commands.openUrl(target.url));
          break;
        case "anchor": {
          const root = hostRef.current?.shadowRoot;
          (
            root?.getElementById(HEADING_ID_PREFIX + target.id) ??
            root?.getElementById(HEADING_ID_PREFIX + target.id.toLowerCase())
          )?.scrollIntoView();
          break;
        }
        case "file":
          void (async () => {
            try {
              await unwrap(
                viewerHost.openFile({
                  vfs_id: vfsPath.vfs_id,
                  path: target.path,
                }),
              );
            } catch (e) {
              setNotice(String(e));
            }
          })();
          break;
      }
    },
    [vfsPath, viewerHost],
  );

  // On the shadow root, where the target is the real element rather than
  // the host the event is retargeted to outside.
  useEffect(() => {
    const root = hostRef.current?.shadowRoot;
    if (!root) return;
    const onClick = (e: Event) => {
      const link = (e.target as Element | null)?.closest?.("a");
      if (!link) return;
      e.preventDefault();
      followLink(link.getAttribute("href"));
    };
    root.addEventListener("click", onClick);
    return () => root.removeEventListener("click", onClick);
  }, [followLink, bytes]);

  useEffect(() => {
    if (!viewerHost.embedded) hostRef.current?.focus();
  }, [viewerHost.embedded]);

  // The document's selection, if any. The document lives in the host's
  // shadow root, where WebKit reports a selection as collapsed and anchored
  // on the host's parent; only its text and its composed range are reliable.
  const readSelection = () => {
    const sel = document.getSelection() as
      | (Selection & {
          getComposedRanges?: (o: {
            shadowRoots: ShadowRoot[];
          }) => StaticRange[];
        })
      | null;
    const host = hostRef.current;
    const root = host?.shadowRoot;
    const text = sel?.toString();
    if (!sel || !host || !root || !text) return null;
    if (sel.getComposedRanges) {
      const range = sel.getComposedRanges({ shadowRoots: [root] })[0];
      return range && root.contains(range.startContainer)
        ? { text, range }
        : null;
    }
    const anchor = sel.anchorNode;
    return anchor && (anchor.contains(host) || host.contains(anchor))
      ? { text, range: null }
      : null;
  };

  const restoreMenuSelection = () => {
    const range = menuSelection.current?.range;
    if (range) {
      // addRange refuses a range in a shadow tree (WebKit).
      document
        .getSelection()
        ?.setBaseAndExtent(
          range.startContainer,
          range.startOffset,
          range.endContainer,
          range.endOffset,
        );
    }
  };

  return (
    <div className={styles.viewer}>
      <CM.Root
        onOpenChange={(open) => {
          if (open) setHasSelection(menuSelection.current !== null);
        }}
      >
        <CM.Trigger
          asChild
          onContextMenu={() => {
            menuSelection.current = readSelection();
          }}
        >
          <div
            ref={hostRef}
            className={styles.viewerContent}
            tabIndex={-1}
            role="document"
            aria-label={filePath}
            aria-busy={bytes === null && notice === null}
            style={{ outline: "none", background: "var(--color-bg)" }}
          />
        </CM.Trigger>
        <CM.Portal>
          <CM.Content
            className={menuStyles.content}
            loop
            // The menu taking focus clears the selection, and so does
            // focus leaving it; put it back each time.
            onFocus={restoreMenuSelection}
            // Clicking away from the menu clicks away from the selection.
            onPointerDownOutside={() => {
              menuSelection.current = null;
            }}
            onCloseAutoFocus={(e) => {
              // Back to the document — or in Quick View to the file list,
              // which owns the keys there.
              e.preventDefault();
              if (viewerHost.embedded) {
                window.dispatchEvent(new Event(REFOCUS_EVENT));
              } else {
                hostRef.current?.focus();
              }
              restoreMenuSelection();
            }}
          >
            <CM.Item
              className={menuStyles.item}
              disabled={!hasSelection}
              onSelect={() => {
                const text = menuSelection.current?.text;
                if (text) navigator.clipboard.writeText(text);
              }}
            >
              Copy
            </CM.Item>
            <CM.Item
              className={menuStyles.item}
              onSelect={() => {
                const article =
                  hostRef.current?.shadowRoot?.querySelector("article");
                if (!article) return;
                // Selected as the menu closes, after focus has gone back.
                menuSelection.current = {
                  text: article.textContent ?? "",
                  range: new StaticRange({
                    startContainer: article,
                    startOffset: 0,
                    endContainer: article,
                    endOffset: article.childNodes.length,
                  }),
                };
              }}
            >
              Select All
            </CM.Item>
          </CM.Content>
        </CM.Portal>
      </CM.Root>
      <div
        className={styles.viewerStatus}
        onContextMenu={(e) => e.preventDefault()}
      >
        <span className={styles.statusText}>
          <span title={filePath}>{filePath}</span>
          <span className={styles.statusSeparator} aria-hidden>
            |
          </span>
          <span>Markdown</span>
          {encodingLabel && (
            <>
              <span className={styles.statusSeparator} aria-hidden>
                |
              </span>
              <span>{encodingLabel}</span>
            </>
          )}
          <span className={styles.statusSeparator} aria-hidden>
            |
          </span>
          <span>{formatSize(fileSize)}</span>
          {notice && (
            <>
              <span className={styles.statusSeparator} aria-hidden>
                |
              </span>
              <span className={styles.statusError} role="alert">
                {notice}
              </span>
            </>
          )}
        </span>
        <ModeToggle currentMode="markdown" autoMode={autoMode} />
      </div>
    </div>
  );
}
