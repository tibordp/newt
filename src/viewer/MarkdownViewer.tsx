import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";

import styles from "./Viewer.module.scss";
import { commands } from "../lib/bindings";
import { safe, unwrap } from "../lib/ipc";
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
  const hostRef = useRef<HTMLDivElement>(null);
  const [bytes, setBytes] = useState<Uint8Array | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    if (fileSize > MAX_RENDER_BYTES) {
      safe(commands.setViewerMode("text"));
      return;
    }
    let cancelled = false;
    setBytes(null);
    setNotice(null);
    (async () => {
      try {
        const data = new Uint8Array(
          (await unwrap(
            commands.readFile(vfsPath, MAX_RENDER_BYTES),
          )) as number[],
        );
        if (cancelled) return;
        if (needsSniff) {
          safe(
            commands.sniffViewerEncoding(
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
                commands.openInViewer({
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
    [vfsPath],
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
    hostRef.current?.focus();
  }, []);

  return (
    <div className={styles.viewer}>
      <div
        ref={hostRef}
        className={styles.viewerContent}
        tabIndex={-1}
        role="document"
        aria-label={filePath}
        aria-busy={bytes === null && notice === null}
        style={{ outline: "none", background: "var(--color-bg)" }}
      />
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
