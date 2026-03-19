import React, {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import * as CM from "@radix-ui/react-context-menu";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import styles from "./Viewer.module.scss";
import menuStyles from "../main_window/Menu.module.scss";
import { GoToBar } from "./GoToDialog";
import { SearchBar } from "./SearchBar";
import {
  CHUNK_SIZE,
  MAX_SCROLL_HEIGHT,
  LruChunkCache,
  collectBytes,
  formatHexOffset,
  formatSize,
  type ViewerMode,
  type VfsPath,
} from "./helpers";
import { ModeToggle } from "./ModeToggle";

interface TextPosition {
  line: number;
  col: number;
}

interface TextSelection {
  anchor: TextPosition;
  head: TextPosition;
}

/**
 * Get the character offset within a line element at a given screen point.
 * Uses caretRangeFromPoint (WebKit/Chrome) for accurate positioning that
 * handles variable-width characters (CJK double-width, combining marks, etc).
 */
function getColAtPoint(
  x: number,
  y: number,
  lineEl: Element,
  lineTextLength: number,
): number {
  // caretRangeFromPoint requires user-select != none in WebKitGTK,
  // so temporarily enable it on the parent pre element.
  const pre = lineEl.parentElement as HTMLElement | null;
  if (pre) {
    pre.style.setProperty("user-select", "text");
    pre.style.setProperty("-webkit-user-select", "text");
  }
  const caretRange = (document as any).caretRangeFromPoint?.(
    x,
    y,
  ) as Range | null;
  if (pre) {
    pre.style.removeProperty("user-select");
    pre.style.removeProperty("-webkit-user-select");
  }
  if (caretRange && lineEl.contains(caretRange.startContainer)) {
    // Walk text nodes to compute absolute character offset
    let offset = 0;
    const walker = document.createTreeWalker(lineEl, NodeFilter.SHOW_TEXT);
    let node: Node | null;
    while ((node = walker.nextNode())) {
      if (node === caretRange.startContainer) {
        return Math.min(offset + caretRange.startOffset, lineTextLength);
      }
      offset += (node as Text).length;
    }
    return Math.min(offset, lineTextLength);
  }
  return lineTextLength;
}

function compareTextPos(a: TextPosition, b: TextPosition): number {
  return a.line !== b.line ? a.line - b.line : a.col - b.col;
}

function orderedTextSel(sel: TextSelection): [TextPosition, TextPosition] {
  return compareTextPos(sel.anchor, sel.head) <= 0
    ? [sel.anchor, sel.head]
    : [sel.head, sel.anchor];
}

export interface TextViewerProps {
  filePath: string;
  vfsPath: VfsPath;
  fileSize: number;
  chunkCache: React.MutableRefObject<LruChunkCache>;
  loadChunk: (chunkIndex: number) => Promise<void>;
  autoMode: ViewerMode;
}

/**
 * Text viewer with on-demand chunk loading. Builds a line-start index
 * incrementally by scanning chunk bytes for newlines (0x0A — safe in
 * UTF-8 since it never appears as a continuation byte). Only the chunks
 * covering the visible line range are kept in memory.
 */
export function TextViewer({
  filePath,
  vfsPath,
  fileSize,
  chunkCache,
  loadChunk,
  autoMode,
}: TextViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const textPreRef = useRef<HTMLPreElement>(null);
  const [topRow, setTopRow] = useState(0);
  const [containerHeight, setContainerHeight] = useState(0);
  const lineHeight = 18;

  // Selection state
  const [selection, setSelection] = useState<TextSelection | null>(null);
  const selectionRef = useRef<TextSelection | null>(null);
  selectionRef.current = selection;
  const isDraggingRef = useRef(false);
  const lastMouseRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const autoScrollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const startLineRef = useRef(0);
  const lineCountRef = useRef(1);

  // Line index: lineStarts[i] = byte offset where line i begins.
  // Scanning proceeds sequentially from byte 0; all chunks up to
  // scannedTo are guaranteed to be in chunkCache.
  const lineStartsRef = useRef<number[]>([0]);
  const scannedToRef = useRef(0);
  const eofFoundRef = useRef(false);
  const [lineCount, setLineCount] = useState(1);
  const [scanTarget, setScanTarget] = useState(200);

  // Reset on file change
  useEffect(() => {
    lineStartsRef.current = [0];
    scannedToRef.current = 0;
    eofFoundRef.current = false;
    setLineCount(1);
    setScanTarget(200);
    setTopRow(0);
    setSelection(null);
  }, [filePath]);

  // Scan effect: load chunks sequentially and record newline positions
  useEffect(() => {
    let cancelled = false;

    (async () => {
      while (
        lineStartsRef.current.length <= scanTarget &&
        !eofFoundRef.current &&
        !cancelled
      ) {
        const ci = Math.floor(scannedToRef.current / CHUNK_SIZE);
        if (!chunkCache.current.has(ci)) {
          await loadChunk(ci);
        }
        if (cancelled) break;
        const chunk = chunkCache.current.get(ci);
        if (!chunk) break;

        const chunkStart = ci * CHUNK_SIZE;
        const scanFrom = scannedToRef.current - chunkStart;

        for (let i = scanFrom; i < chunk.length; i++) {
          if (chunk[i] === 0x0a) {
            lineStartsRef.current.push(chunkStart + i + 1);
          }
        }

        scannedToRef.current = chunkStart + chunk.length;
        if (chunk.length < CHUNK_SIZE) {
          eofFoundRef.current = true;
        }

        if (!cancelled) {
          setLineCount(lineStartsRef.current.length);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scanTarget, filePath]);

  // Focus
  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  // Container height
  useLayoutEffect(() => {
    if (containerRef.current)
      setContainerHeight(containerRef.current.clientHeight);
  }, []);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const obs = new ResizeObserver(() => setContainerHeight(el.clientHeight));
    obs.observe(el);
    return () => obs.disconnect();
  }, []);

  // Layout calculations — same pattern as HexViewer.
  // For known-size files, estimate remaining lines from average line length
  // so the scrollbar reflects the full file, not just scanned lines.
  let extraLines = 0;
  if (!eofFoundRef.current) {
    if (fileSize > 0 && scannedToRef.current > 0 && lineCount > 1) {
      const avgLineLen = scannedToRef.current / lineCount;
      const estimatedTotal = Math.ceil(fileSize / avgLineLen);
      extraLines = Math.max(0, estimatedTotal - lineCount);
    } else {
      extraLines = 100; // unknown size or not enough data to estimate
    }
  }
  const totalRows = lineCount + extraLines;
  const visibleRows = Math.floor(containerHeight / lineHeight);
  const maxRow = Math.max(0, totalRows - visibleRows);
  const naturalHeight = totalRows * lineHeight;
  const scrollableHeight = Math.min(naturalHeight, MAX_SCROLL_HEIGHT);
  const scale =
    naturalHeight > MAX_SCROLL_HEIGHT ? naturalHeight / MAX_SCROLL_HEIGHT : 1;

  const clampedTopRow = Math.max(0, Math.min(topRow, maxRow));
  if (clampedTopRow !== topRow) setTopRow(clampedTopRow);

  const startLine = Math.max(0, clampedTopRow - 5);
  const endLine = Math.min(totalRows, clampedTopRow + visibleRows + 5);

  // Keep refs in sync for use in event handlers
  startLineRef.current = startLine;
  lineCountRef.current = lineCount;

  // Extend scan target when approaching end of indexed lines
  useEffect(() => {
    if (!eofFoundRef.current && endLine + 50 > lineStartsRef.current.length) {
      setScanTarget((prev) => Math.max(prev, endLine + 200));
    }
  }, [endLine]);

  // Ensure chunks for visible lines are loaded (may have been evicted by LRU)
  const ls = lineStartsRef.current;
  const actualEndLine = Math.min(endLine, ls.length);
  const visStartByte = startLine < ls.length ? ls[startLine] : 0;
  const visEndByte =
    actualEndLine > 0
      ? actualEndLine < ls.length
        ? ls[actualEndLine]
        : scannedToRef.current
      : 0;
  const visStartChunk = Math.floor(visStartByte / CHUNK_SIZE);
  const visEndChunk =
    visEndByte > visStartByte
      ? Math.floor((visEndByte - 1) / CHUNK_SIZE)
      : visStartChunk;
  const [renderGen, setRenderGen] = useState(0);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      let loaded = false;
      for (let ci = visStartChunk; ci <= visEndChunk; ci++) {
        if (!chunkCache.current.has(ci)) {
          await loadChunk(ci);
          loaded = true;
        }
      }
      if (loaded && !cancelled) setRenderGen((n) => n + 1);
    })();
    return () => {
      cancelled = true;
    };
  }, [visStartChunk, visEndChunk, loadChunk, chunkCache]);

  // Memoize visible line extraction — only recompute when the visible
  // range changes or new chunks load (renderGen).
  const visibleLineTexts = useMemo(() => {
    if (startLine >= actualEndLine) return [];
    const startByte = ls[startLine];
    const endByte =
      actualEndLine < ls.length ? ls[actualEndLine] : scannedToRef.current;
    if (endByte <= startByte)
      return Array(actualEndLine - startLine).fill("") as string[];
    const bytes = collectBytes(chunkCache.current, startByte, endByte);
    const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
    const parts = text.split("\n");
    return parts.slice(0, actualEndLine - startLine);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [startLine, actualEndLine, lineCount, renderGen]);

  const lineNumWidth = Math.max(4, String(lineCount).length);
  const gutterWidth = `${lineNumWidth + 1}ch`;

  // --- Selection helpers ---

  const getPositionFromMouse = useCallback(
    (clientX: number, clientY: number): TextPosition => {
      const pre = textPreRef.current;
      if (!pre) return { line: 0, col: 0 };
      const preRect = pre.getBoundingClientRect();
      const line = Math.max(
        0,
        Math.min(
          startLineRef.current +
            Math.floor((clientY - preRect.top) / lineHeight),
          lineCountRef.current - 1,
        ),
      );

      // If click is left of the text area (i.e. in the gutter), col = 0
      const preRect2 = pre.getBoundingClientRect();
      let col: number;
      if (clientX < preRect2.left) {
        col = 0;
      } else {
        const lineIdx = line - startLineRef.current;
        const lineEl = pre.children[lineIdx];
        // textContent includes trailing "\n", subtract 1 for line text length
        const lineTextLen = lineEl
          ? Math.max(0, (lineEl.textContent?.length ?? 1) - 1)
          : 0;
        col = lineEl ? getColAtPoint(clientX, clientY, lineEl, lineTextLen) : 0;
      }

      return { line, col };
    },
    [lineHeight],
  );

  const getLineText = useCallback(
    (lineIdx: number): string => {
      const ls = lineStartsRef.current;
      if (lineIdx < 0 || lineIdx >= ls.length) return "";
      const start = ls[lineIdx];
      const end =
        lineIdx + 1 < ls.length ? ls[lineIdx + 1] : scannedToRef.current;
      if (end <= start) return "";
      const bytes = collectBytes(chunkCache.current, start, end);
      const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
      return text.replace(/\n$/, "");
    },
    [chunkCache],
  );

  // Convert a (line, col) text position to an absolute byte offset.
  // col=0 → lineStart, col=lineLen → lineEnd (before \n). Uses TextEncoder
  // to handle multi-byte UTF-8 characters. Only needs the line's chunks in
  // cache (always true for visible/recently-visible lines).
  const colToByteOffset = useCallback(
    (line: number, col: number): number => {
      const ls = lineStartsRef.current;
      if (line >= ls.length) return scannedToRef.current;
      const lineStart = ls[line];
      if (col === 0) return lineStart;
      const lineEnd =
        line + 1 < ls.length ? ls[line + 1] : scannedToRef.current;
      const lineBytes = collectBytes(chunkCache.current, lineStart, lineEnd);
      const lineText = new TextDecoder("utf-8", { fatal: false })
        .decode(lineBytes)
        .replace(/\n$/, "");
      if (col >= lineText.length) {
        return lineStart + new TextEncoder().encode(lineText).length;
      }
      return (
        lineStart + new TextEncoder().encode(lineText.slice(0, col)).length
      );
    },
    [chunkCache],
  );

  // Compute the byte range [start, end) for the current selection
  const selectionByteRange = useCallback((): [number, number] | null => {
    const sel = selectionRef.current;
    if (!sel) return null;
    const [start, end] = orderedTextSel(sel);
    return [
      colToByteOffset(start.line, start.col),
      colToByteOffset(end.line, end.col),
    ];
  }, [colToByteOffset]);

  const copySelection = useCallback(() => {
    const range = selectionByteRange();
    if (!range) return;
    const [startByte, endByte] = range;
    if (endByte <= startByte) return;
    invoke("copy_viewer_range", {
      path: vfsPath,
      offset: startByte,
      length: endByte - startByte,
      format: "text",
    }).catch((e) => console.error("copy failed:", e));
  }, [vfsPath, selectionByteRange]);

  const selectAll = useCallback(() => {
    const lastLine = lineCountRef.current - 1;
    const lastLineText = getLineText(lastLine);
    setSelection({
      anchor: { line: 0, col: 0 },
      head: { line: lastLine, col: lastLineText.length },
    });
  }, [getLineText]);

  const [goToOpen, setGoToOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);

  const goToLine = useCallback(() => {
    setGoToOpen(true);
  }, []);

  const openSearch = useCallback(() => {
    setSearchOpen(true);
  }, []);

  // When a search match is found, scroll to the byte offset and select the range
  const handleSearchMatch = useCallback(
    (match: { offset: number; length: number }) => {
      // Find which line contains this byte offset using the line index
      const ls = lineStartsRef.current;
      let matchLine = 0;
      for (let i = 1; i < ls.length; i++) {
        if (ls[i] > match.offset) break;
        matchLine = i;
      }

      // Compute column from byte offset within the line
      const lineStartByte = ls[matchLine];
      const lineEndByte =
        matchLine + 1 < ls.length ? ls[matchLine + 1] : scannedToRef.current;
      const lineBytes = collectBytes(
        chunkCache.current,
        lineStartByte,
        lineEndByte,
      );
      const lineText = new TextDecoder("utf-8", { fatal: false }).decode(
        lineBytes,
      );
      const prefixBytes = match.offset - lineStartByte;
      // Count characters in the prefix bytes
      const prefixText = new TextDecoder("utf-8", { fatal: false }).decode(
        lineBytes.subarray(0, prefixBytes),
      );
      const startCol = prefixText.length;

      // Find end position
      const endByteOffset = match.offset + match.length;
      let endLine = matchLine;
      for (let i = matchLine + 1; i < ls.length; i++) {
        if (ls[i] > endByteOffset) break;
        endLine = i;
      }
      const endLineStartByte = ls[endLine];
      const endLineEndByte =
        endLine + 1 < ls.length ? ls[endLine + 1] : scannedToRef.current;
      const endLineBytes = collectBytes(
        chunkCache.current,
        endLineStartByte,
        endLineEndByte,
      );
      const endPrefixBytes = endByteOffset - endLineStartByte;
      const endPrefixText = new TextDecoder("utf-8", { fatal: false }).decode(
        endLineBytes.subarray(0, endPrefixBytes),
      );
      const endCol = endPrefixText.length;

      // Set selection and scroll to match
      setSelection({
        anchor: { line: matchLine, col: startCol },
        head: { line: endLine, col: endCol },
      });

      setTopRow(Math.max(0, matchLine));

      // After React renders the selection highlight, scroll horizontally
      // to make it visible (for long lines).
      requestAnimationFrame(() => {
        const container = containerRef.current;
        const highlight = container?.querySelector(`.${styles.selActive}`);
        if (highlight && container) {
          const hRect = highlight.getBoundingClientRect();
          const cRect = container.getBoundingClientRect();
          if (hRect.left < cRect.left) {
            container.scrollLeft -= cRect.left - hRect.left + 20;
          } else if (hRect.right > cRect.right) {
            container.scrollLeft += hRect.right - cRect.right + 20;
          }
        }
      });
    },
    [chunkCache, lineHeight, scale],
  );

  const handleGoToSubmit = useCallback(
    (value: string) => {
      const lineNumber = parseInt(value, 10);
      if (isNaN(lineNumber) || lineNumber < 1) return;
      const el = containerRef.current;
      if (el) {
        el.scrollTop = ((lineNumber - 1) * lineHeight) / scale;
      }
    },
    [lineHeight, scale],
  );

  // Menu event listener (for native menu Copy/Select All/Go to Line)
  const copySelectionRef = useRef(copySelection);
  copySelectionRef.current = copySelection;
  const selectAllRef = useRef(selectAll);
  selectAllRef.current = selectAll;
  const goToLineRef = useRef(goToLine);
  goToLineRef.current = goToLine;

  useEffect(() => {
    const unlisten = listen<string>("viewer-menu", (event) => {
      switch (event.payload) {
        case "copy":
          copySelectionRef.current();
          break;
        case "select_all":
          selectAllRef.current();
          break;
        case "goto":
          goToLineRef.current();
          break;
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const handleTextMouseDown = useCallback(
    (e: React.MouseEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const pos = getPositionFromMouse(e.clientX, e.clientY);
      if (e.shiftKey && selectionRef.current) {
        const newSel = { ...selectionRef.current, head: pos };
        setSelection(newSel);
      } else {
        setSelection({ anchor: pos, head: pos });
      }
      isDraggingRef.current = true;
    },
    [getPositionFromMouse],
  );

  // Window-level mousemove/mouseup for drag selection
  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!isDraggingRef.current) return;
      lastMouseRef.current = { x: e.clientX, y: e.clientY };
      const pos = getPositionFromMouse(e.clientX, e.clientY);
      setSelection((prev) => (prev ? { ...prev, head: pos } : null));

      // Auto-scroll at edges
      const container = containerRef.current;
      if (!container) return;
      const rect = container.getBoundingClientRect();
      const margin = 20;

      if (e.clientY < rect.top + margin || e.clientY > rect.bottom - margin) {
        if (!autoScrollRef.current) {
          autoScrollRef.current = setInterval(() => {
            const m = lastMouseRef.current;
            const r = container.getBoundingClientRect();
            if (m.y < r.top + margin) {
              container.scrollTop -= lineHeight;
            } else if (m.y > r.bottom - margin) {
              container.scrollTop += lineHeight;
            }
            const p = getPositionFromMouse(m.x, m.y);
            setSelection((prev) => (prev ? { ...prev, head: p } : null));
          }, 50);
        }
      } else if (autoScrollRef.current) {
        clearInterval(autoScrollRef.current);
        autoScrollRef.current = null;
      }
    };

    const handleMouseUp = () => {
      isDraggingRef.current = false;
      if (autoScrollRef.current) {
        clearInterval(autoScrollRef.current);
        autoScrollRef.current = null;
      }
    };

    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
      if (autoScrollRef.current) {
        clearInterval(autoScrollRef.current);
        autoScrollRef.current = null;
      }
    };
  }, [getPositionFromMouse, lineHeight]);

  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      setTopRow(Math.floor((e.currentTarget.scrollTop * scale) / lineHeight));
    },
    [lineHeight, scale],
  );

  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const target = (clampedTopRow * lineHeight) / scale;
    const rowPx = lineHeight / scale;
    if (Math.abs(el.scrollTop - target) >= rowPx) {
      el.scrollTop = target;
    }
  }, [clampedTopRow, lineHeight, scale]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      // Don't intercept keys meant for input elements (e.g. search bar)
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      )
        return;

      if (e.key === "a" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        selectAll();
        return;
      }

      if (e.key === "c" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        copySelection();
        return;
      }

      if (e.key === "g" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        goToLine();
        return;
      }

      if (e.key === "f" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        openSearch();
        return;
      }

      const scrollBy = (rows: number) => {
        setTopRow((prev) => Math.max(0, Math.min(prev + rows, maxRow)));
        e.preventDefault();
      };

      switch (e.key) {
        case "Escape":
          if (selectionRef.current) {
            setSelection(null);
            e.stopPropagation();
            e.preventDefault();
          }
          // If not consumed, bubbles to parent (closes window)
          break;
        case "ArrowUp":
          scrollBy(-1);
          break;
        case "ArrowDown":
          scrollBy(1);
          break;
        case "PageUp":
          scrollBy(-visibleRows);
          break;
        case "PageDown":
          scrollBy(visibleRows);
          break;
        case "Home":
          setTopRow(0);
          e.preventDefault();
          break;
        case "End":
          setTopRow(maxRow);
          e.preventDefault();
          break;
      }
    },
    [visibleRows, maxRow, selectAll, copySelection, goToLine, openSearch],
  );

  const currentLine = clampedTopRow + 1;
  const topSpacerHeight = (startLine * lineHeight) / scale;

  const selRange = selection ? orderedTextSel(selection) : null;
  const renderLineContent = (
    text: string,
    lineIdx: number,
  ): React.ReactNode => {
    if (!selRange) return text;
    const [start, end] = selRange;
    if (lineIdx < start.line || lineIdx > end.line) return text;
    const s = lineIdx === start.line ? Math.min(start.col, text.length) : 0;
    const e =
      lineIdx === end.line ? Math.min(end.col, text.length) : text.length;
    if (s >= e) return text;
    return (
      <>
        {s > 0 ? text.slice(0, s) : null}
        <span className={styles.selActive}>{text.slice(s, e)}</span>
        {e < text.length ? text.slice(e) : null}
      </>
    );
  };

  return (
    <div
      className={styles.viewer}
      ref={viewerRef}
      tabIndex={-1}
      onKeyDown={handleKeyDown}
    >
      <div
        className={styles.viewerContent}
        ref={containerRef}
        onScroll={handleScroll}
      >
        <div style={{ height: scrollableHeight }}>
          <div style={{ height: topSpacerHeight }} />
          <CM.Root>
            <CM.Trigger asChild>
              <div
                style={{ display: "flex" }}
                onMouseDown={handleTextMouseDown}
              >
                <div
                  className={styles.viewerGutter}
                  style={{ width: gutterWidth }}
                >
                  {visibleLineTexts.map((_, i) => (
                    <div key={startLine + i} className={styles.gutterLine}>
                      {String(startLine + i + 1).padStart(lineNumWidth, " ")}
                    </div>
                  ))}
                </div>
                <pre
                  ref={textPreRef}
                  className={styles.viewerText}
                  style={{ flex: 1, margin: 0 }}
                >
                  {visibleLineTexts.map((line, i) => (
                    <div key={startLine + i} className={styles.viewerLine}>
                      {renderLineContent(line, startLine + i)}
                      {"\n"}
                    </div>
                  ))}
                </pre>
              </div>
            </CM.Trigger>
            <CM.Portal>
              <CM.Content className={menuStyles.content} loop>
                <CM.Item
                  className={menuStyles.item}
                  disabled={!selection}
                  onSelect={() => copySelection()}
                >
                  Copy
                </CM.Item>
                <CM.Item
                  className={menuStyles.item}
                  onSelect={() => selectAll()}
                >
                  Select All
                </CM.Item>
                <CM.Separator className={menuStyles.separator} />
                <CM.Item
                  className={menuStyles.item}
                  onSelect={() => goToLine()}
                >
                  Go to Line...
                </CM.Item>
              </CM.Content>
            </CM.Portal>
          </CM.Root>
        </div>
      </div>
      <SearchBar
        open={searchOpen}
        onClose={() => {
          setSearchOpen(false);
          viewerRef.current?.focus();
        }}
        vfsPath={vfsPath}
        fileSize={fileSize}
        mode="text"
        onMatch={handleSearchMatch}
        onNoMatch={() => {}}
      />
      <GoToBar
        open={goToOpen}
        onClose={() => {
          setGoToOpen(false);
          viewerRef.current?.focus();
        }}
        label="Go to line"
        placeholder="1"
        onSubmit={handleGoToSubmit}
      />
      <div
        className={styles.viewerStatus}
        onContextMenu={(e) => e.preventDefault()}
      >
        <span className={styles.statusText}>
          <span>{filePath}</span>
          <span className={styles.statusSeparator}>|</span>
          <span>Text</span>
          <span className={styles.statusSeparator}>|</span>
          <span>
            Line {currentLine} / {lineCount}
            {!eofFoundRef.current && "+"}
          </span>
          {selRange &&
            compareTextPos(selRange[0], selRange[1]) !== 0 &&
            (() => {
              const [start, end] = selRange;
              const startByte = colToByteOffset(start.line, start.col);
              const endByte = colToByteOffset(end.line, end.col);
              const byteCount = endByte - startByte;
              const isSameLine = start.line === end.line;
              const posText = isSameLine
                ? `L${start.line + 1} C${start.col + 1}\u2013C${end.col + 1}`
                : `L${start.line + 1} C${start.col + 1} \u2013 L${end.line + 1} C${end.col + 1}`;
              return (
                <>
                  <span className={styles.statusSeparator}>|</span>
                  <span>
                    Sel: {posText} ({formatHexOffset(startByte)}
                    {"\u2013"}
                    {formatHexOffset(endByte)}, {formatSize(byteCount)})
                  </span>
                </>
              );
            })()}
          <span className={styles.statusSeparator}>|</span>
          <span>{formatSize(fileSize)}</span>
        </span>
        <ModeToggle currentMode="text" autoMode={autoMode} />
      </div>
    </div>
  );
}
