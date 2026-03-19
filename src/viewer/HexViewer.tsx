import React, {
  useCallback,
  useEffect,
  useLayoutEffect,
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
  HEX_BYTES_PER_ROW,
  MAX_SCROLL_HEIGHT,
  LruChunkCache,
  formatHexOffset,
  formatSize,
  hexByte,
  printableAscii,
  type ViewerMode,
  type VfsPath,
} from "./helpers";
import { ModeToggle } from "./ModeToggle";

export interface HexViewerProps {
  filePath: string;
  vfsPath: VfsPath;
  fileSize: number;
  chunkCache: React.MutableRefObject<LruChunkCache>;
  loadChunk: (chunkIndex: number) => Promise<void>;
  autoMode: ViewerMode;
}

interface HexSelection {
  anchor: number; // byte offset
  head: number; // byte offset
}

// Pre-computed padding styles for hex byte spans
// paddingRight: highlight extends into the gap (continuous between selected bytes)
const HEX_BYTE_PAD: (React.CSSProperties | undefined)[] = Array.from(
  { length: HEX_BYTES_PER_ROW },
  (_, j) =>
    j === HEX_BYTES_PER_ROW - 1
      ? undefined
      : j === 7
        ? { paddingRight: "2ch" }
        : { paddingRight: "1ch" },
);
// marginRight: highlight stops at the byte boundary (for last selected byte)
const HEX_BYTE_MAR: (React.CSSProperties | undefined)[] = Array.from(
  { length: HEX_BYTES_PER_ROW },
  (_, j) =>
    j === HEX_BYTES_PER_ROW - 1
      ? undefined
      : j === 7
        ? { marginRight: "2ch" }
        : { marginRight: "1ch" },
);

export function HexViewer({
  filePath,
  vfsPath,
  fileSize,
  chunkCache,
  loadChunk: loadChunkRaw,
  autoMode,
}: HexViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const hexColRef = useRef<HTMLDivElement>(null);
  const asciiColRef = useRef<HTMLDivElement>(null);
  const [topRow, setTopRow] = useState(0);
  const [containerHeight, setContainerHeight] = useState(0);
  const [, forceUpdate] = useState(0);
  const rowHeight = 18;

  // Selection state
  const [hexSelection, setHexSelection] = useState<HexSelection | null>(null);
  const hexSelectionRef = useRef<HexSelection | null>(null);
  hexSelectionRef.current = hexSelection;
  const [activeColumn, setActiveColumn] = useState<"hex" | "ascii">("hex");
  const activeColumnRef = useRef<"hex" | "ascii">("hex");
  activeColumnRef.current = activeColumn;
  const isDraggingRef = useRef(false);
  const lastMouseRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const autoScrollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const charWidthRef = useRef(8);
  const startRowRef = useRef(0);

  // Track the actual extent of the file as we discover it by reading.
  // Starts with stat-reported size, grows if we read beyond it (pseudo-files),
  // and finalizes when a read returns less than requested (EOF).
  const [discoveredSize, setDiscoveredSize] = useState(fileSize);
  const eofFoundRef = useRef(false);

  // Reset when file changes
  useEffect(() => {
    setDiscoveredSize(fileSize);
    eofFoundRef.current = false;
    setHexSelection(null);
  }, [fileSize]);

  // Wrap loadChunk to track discovered size
  const loadChunk = useCallback(
    async (chunkIndex: number) => {
      await loadChunkRaw(chunkIndex);
      const chunk = chunkCache.current.get(chunkIndex);
      if (chunk) {
        const chunkEnd = chunkIndex * CHUNK_SIZE + chunk.length;
        setDiscoveredSize((prev) => Math.max(prev, chunkEnd));
        if (chunk.length < CHUNK_SIZE) {
          eofFoundRef.current = true;
        }
      }
    },
    [loadChunkRaw, chunkCache],
  );

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  // Measure monospace character width
  useEffect(() => {
    const col = hexColRef.current ?? asciiColRef.current;
    if (!col) return;
    const span = document.createElement("span");
    span.style.cssText =
      "position:absolute;visibility:hidden;pointer-events:none;";
    span.textContent = "X";
    col.appendChild(span);
    charWidthRef.current = span.getBoundingClientRect().width;
    col.removeChild(span);
  }, []);

  // For unknown-size files (fileSize=0, e.g. /dev/urandom), add extra rows
  // so the user can scroll further and trigger more reads. For known-size
  // files, discoveredSize already starts at fileSize, so no extra rows needed.
  const extraRows = !eofFoundRef.current && fileSize === 0 ? 100 : 0;
  const totalRows = Math.ceil(discoveredSize / HEX_BYTES_PER_ROW) + extraRows;
  const visibleRows = Math.floor(containerHeight / rowHeight);
  const maxRow = Math.max(0, totalRows - visibleRows);
  const naturalHeight = totalRows * rowHeight;
  const scrollableHeight = Math.min(naturalHeight, MAX_SCROLL_HEIGHT);
  // Scale factor for mapping scroll position ↔ row when content exceeds
  // the browser's max element height. scale=1 when no compression needed.
  const scale =
    naturalHeight > MAX_SCROLL_HEIGHT ? naturalHeight / MAX_SCROLL_HEIGHT : 1;

  // Clamp topRow if maxRow changes (e.g. on resize)
  const clampedTopRow = Math.max(0, Math.min(topRow, maxRow));
  if (clampedTopRow !== topRow) {
    setTopRow(clampedTopRow);
  }

  useLayoutEffect(() => {
    if (containerRef.current) {
      setContainerHeight(containerRef.current.clientHeight);
    }
  }, []);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => {
      setContainerHeight(el.clientHeight);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // Calculate visible rows with overscan
  const startRow = Math.max(0, clampedTopRow - 5);
  const endRow = Math.min(totalRows, clampedTopRow + visibleRows + 5);
  startRowRef.current = startRow;

  // Determine which chunks we need
  const startByte = startRow * HEX_BYTES_PER_ROW;
  const endByte = endRow * HEX_BYTES_PER_ROW;
  const startChunk = Math.floor(startByte / CHUNK_SIZE);
  const endChunk = Math.floor(endByte / CHUNK_SIZE);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      let newlyLoaded = false;
      for (let ci = startChunk; ci <= endChunk; ci++) {
        // Always call loadChunk even if cached — the wrapper tracks
        // discoveredSize/EOF which matters when chunks were pre-loaded
        // by text mode before switching to hex.
        const wasCached = chunkCache.current.has(ci);
        await loadChunk(ci);
        if (!wasCached) newlyLoaded = true;
      }
      if (newlyLoaded && !cancelled) {
        forceUpdate((n) => n + 1);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [startChunk, endChunk, loadChunk]);

  const getByteAt = useCallback(
    (offset: number): number | undefined => {
      const ci = Math.floor(offset / CHUNK_SIZE);
      const chunk = chunkCache.current.get(ci);
      if (!chunk) return undefined;
      const localOffset = offset - ci * CHUNK_SIZE;
      if (localOffset >= chunk.length) return undefined;
      return chunk[localOffset];
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [discoveredSize],
  );

  // --- Selection helpers ---

  const isByteSelected = (offset: number): boolean => {
    if (!hexSelection) return false;
    const start = Math.min(hexSelection.anchor, hexSelection.head);
    const end = Math.max(hexSelection.anchor, hexSelection.head);
    return offset >= start && offset <= end;
  };

  const getByteFromMouse = useCallback(
    (
      clientX: number,
      clientY: number,
      column: "hex" | "ascii",
    ): number | undefined => {
      // Try data-byte-offset first
      const el = document.elementFromPoint(clientX, clientY);
      if (el instanceof HTMLElement && el.dataset.byteOffset !== undefined) {
        return parseInt(el.dataset.byteOffset, 10);
      }

      // Fallback: coordinate-based calculation
      const colEl = column === "hex" ? hexColRef.current : asciiColRef.current;
      if (!colEl) return undefined;
      const colRect = colEl.getBoundingClientRect();
      const relY = clientY - colRect.top;
      const row = startRowRef.current + Math.floor(relY / rowHeight);
      if (row < 0) return 0;
      const relX = clientX - colRect.left;
      const cw = charWidthRef.current;

      let byteInRow: number;
      if (column === "ascii") {
        byteInRow = Math.max(0, Math.min(Math.round(relX / cw), 15));
      } else {
        // Hex layout: byte j position in characters: j < 8 ? j*3 : j*3+1
        // Each byte is 2 chars wide, with 1ch padding (or 2ch after byte 7)
        const charPos = relX / cw;
        if (charPos < 0) {
          byteInRow = 0;
        } else {
          // Find closest byte
          let best = 0;
          let bestDist = Infinity;
          for (let j = 0; j < HEX_BYTES_PER_ROW; j++) {
            const center = j < 8 ? j * 3 + 1 : j * 3 + 2;
            const dist = Math.abs(charPos - center);
            if (dist < bestDist) {
              bestDist = dist;
              best = j;
            }
          }
          byteInRow = best;
        }
      }

      return row * HEX_BYTES_PER_ROW + byteInRow;
    },
    [rowHeight],
  );

  const copyHexSelection = useCallback(
    (format?: "hex" | "ascii") => {
      const sel = hexSelectionRef.current;
      if (!sel) return;
      const start = Math.min(sel.anchor, sel.head);
      const end = Math.max(sel.anchor, sel.head);
      const fmt = format ?? activeColumnRef.current;
      invoke("copy_viewer_range", {
        path: vfsPath,
        offset: start,
        length: end - start + 1,
        format: fmt,
      }).catch((e) => console.error("copy failed:", e));
    },
    [vfsPath],
  );

  const selectAll = useCallback(() => {
    setHexSelection({
      anchor: 0,
      head: Math.max(0, discoveredSize - 1),
    });
  }, [discoveredSize]);

  const [goToOpen, setGoToOpen] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);

  const goToOffset = useCallback(() => {
    setGoToOpen(true);
  }, []);

  const openSearch = useCallback(() => {
    setSearchOpen(true);
  }, []);

  // When a search match is found, scroll to the byte offset and select the range
  const handleSearchMatch = useCallback(
    (match: { offset: number; length: number }) => {
      setHexSelection({
        anchor: match.offset,
        head: match.offset + match.length - 1,
      });
      const row = Math.floor(match.offset / HEX_BYTES_PER_ROW);
      const el = containerRef.current;
      if (el) {
        el.scrollTop = (row * rowHeight) / scale;
      }
    },
    [rowHeight, scale],
  );

  const handleGoToSubmit = useCallback(
    (value: string) => {
      const offset = parseInt(value, 16);
      if (isNaN(offset) || offset < 0) return;
      const row = Math.floor(offset / HEX_BYTES_PER_ROW);
      const el = containerRef.current;
      if (el) {
        el.scrollTop = (row * rowHeight) / scale;
      }
    },
    [rowHeight, scale],
  );

  // Menu event listener (for native menu Copy/Select All/Go to Offset)
  const copyHexSelectionRef = useRef(copyHexSelection);
  copyHexSelectionRef.current = copyHexSelection;
  const selectAllRef = useRef(selectAll);
  selectAllRef.current = selectAll;
  const goToOffsetRef = useRef(goToOffset);
  goToOffsetRef.current = goToOffset;

  useEffect(() => {
    const unlisten = listen<string>("viewer-menu", (event) => {
      switch (event.payload) {
        case "copy":
          copyHexSelectionRef.current();
          break;
        case "select_all":
          selectAllRef.current();
          break;
        case "goto":
          goToOffsetRef.current();
          break;
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const handleHexMouseDown = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      e.preventDefault();
      // Determine column from click position
      const asciiEl = asciiColRef.current;
      const column: "hex" | "ascii" =
        asciiEl && e.clientX >= asciiEl.getBoundingClientRect().left
          ? "ascii"
          : "hex";
      setActiveColumn(column);
      const bo = getByteFromMouse(e.clientX, e.clientY, column);
      if (bo === undefined) return;

      if (e.shiftKey && hexSelectionRef.current) {
        setHexSelection({ ...hexSelectionRef.current, head: bo });
      } else {
        setHexSelection({ anchor: bo, head: bo });
      }
      isDraggingRef.current = true;
    },
    [getByteFromMouse],
  );

  // Window-level mousemove/mouseup for drag selection
  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!isDraggingRef.current) return;
      lastMouseRef.current = { x: e.clientX, y: e.clientY };
      const bo = getByteFromMouse(
        e.clientX,
        e.clientY,
        activeColumnRef.current,
      );
      if (bo !== undefined) {
        setHexSelection((prev) => (prev ? { ...prev, head: bo } : null));
      }

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
              container.scrollTop -= rowHeight;
            } else if (m.y > r.bottom - margin) {
              container.scrollTop += rowHeight;
            }
            const b = getByteFromMouse(m.x, m.y, activeColumnRef.current);
            if (b !== undefined) {
              setHexSelection((prev) => (prev ? { ...prev, head: b } : null));
            }
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
  }, [getByteFromMouse, rowHeight]);

  // Simple scroll-driven positioning: derive topRow from scrollTop,
  // render visible rows at their natural positions using a top spacer.
  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      const scrollTop = e.currentTarget.scrollTop;
      setTopRow(
        Math.max(
          0,
          Math.min(Math.floor((scrollTop * scale) / rowHeight), maxRow),
        ),
      );
    },
    [maxRow, rowHeight, scale],
  );

  // Sync scrollbar to topRow for keyboard-driven changes. For mouse-driven
  // scrolling, topRow was derived from scrollTop so they already match and
  // the threshold check prevents a feedback loop.
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const target = (clampedTopRow * rowHeight) / scale;
    const rowPx = rowHeight / scale;
    if (Math.abs(el.scrollTop - target) >= rowPx) {
      el.scrollTop = target;
    }
  }, [clampedTopRow, rowHeight, scale]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
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
        copyHexSelection();
        return;
      }

      if (e.key === "g" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        goToOffset();
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
          if (hexSelectionRef.current) {
            setHexSelection(null);
            e.stopPropagation();
            e.preventDefault();
          }
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
    [visibleRows, maxRow, selectAll, copyHexSelection, goToOffset, openSearch],
  );

  const currentOffset = clampedTopRow * HEX_BYTES_PER_ROW;
  const topSpacerHeight = (startRow * rowHeight) / scale;

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
              <div style={{ display: "flex" }} onMouseDown={handleHexMouseDown}>
                {/* Offset column - non-selectable */}
                <div className={styles.hexOffsetCol}>
                  {Array.from({ length: endRow - startRow }, (_, i) => (
                    <div key={startRow + i} className={styles.hexRow}>
                      {formatHexOffset((startRow + i) * HEX_BYTES_PER_ROW)}
                    </div>
                  ))}
                </div>
                {/* Hex bytes column - selectable */}
                <div ref={hexColRef} className={styles.hexBytesCol}>
                  {Array.from({ length: endRow - startRow }, (_, i) => {
                    const rowIdx = startRow + i;
                    const rowOffset = rowIdx * HEX_BYTES_PER_ROW;
                    return (
                      <div key={rowIdx} className={styles.hexRow}>
                        {Array.from({ length: HEX_BYTES_PER_ROW }, (_, j) => {
                          const bo = rowOffset + j;
                          const b = getByteAt(bo);
                          const sel = isByteSelected(bo);
                          const cls = sel
                            ? activeColumn === "hex"
                              ? styles.selActive
                              : styles.selInactive
                            : undefined;
                          // Use margin on last selected byte so highlight
                          // doesn't extend into the trailing gap
                          const nextSel = sel && !isByteSelected(bo + 1);
                          return (
                            <span
                              key={j}
                              className={cls}
                              style={
                                nextSel ? HEX_BYTE_MAR[j] : HEX_BYTE_PAD[j]
                              }
                              data-byte-offset={bo}
                            >
                              {b !== undefined ? hexByte(b) : "  "}
                            </span>
                          );
                        })}
                      </div>
                    );
                  })}
                </div>
                {/* ASCII column - selectable */}
                <div ref={asciiColRef} className={styles.hexAsciiCol}>
                  {Array.from({ length: endRow - startRow }, (_, i) => {
                    const rowIdx = startRow + i;
                    const rowOffset = rowIdx * HEX_BYTES_PER_ROW;
                    return (
                      <div key={rowIdx} className={styles.hexRow}>
                        {Array.from({ length: HEX_BYTES_PER_ROW }, (_, j) => {
                          const bo = rowOffset + j;
                          const b = getByteAt(bo);
                          const sel = isByteSelected(bo);
                          const cls = sel
                            ? activeColumn === "ascii"
                              ? styles.selActive
                              : styles.selInactive
                            : undefined;
                          return (
                            <span key={j} className={cls} data-byte-offset={bo}>
                              {b !== undefined ? printableAscii(b) : " "}
                            </span>
                          );
                        })}
                      </div>
                    );
                  })}
                </div>
              </div>
            </CM.Trigger>
            <CM.Portal>
              <CM.Content className={menuStyles.content} loop>
                <CM.Item
                  className={menuStyles.item}
                  disabled={!hexSelection}
                  onSelect={() => copyHexSelection("hex")}
                >
                  Copy as Hex
                </CM.Item>
                <CM.Item
                  className={menuStyles.item}
                  disabled={!hexSelection}
                  onSelect={() => copyHexSelection("ascii")}
                >
                  Copy as Text
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
                  onSelect={() => goToOffset()}
                >
                  Go to Offset...
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
        mode="hex"
        onMatch={handleSearchMatch}
        onNoMatch={() => {}}
      />
      <GoToBar
        open={goToOpen}
        onClose={() => {
          setGoToOpen(false);
          viewerRef.current?.focus();
        }}
        label="Go to offset (hex)"
        placeholder="0"
        onSubmit={handleGoToSubmit}
      />
      <div
        className={styles.viewerStatus}
        onContextMenu={(e) => e.preventDefault()}
      >
        <span className={styles.statusText}>
          <span>{filePath}</span>
          <span className={styles.statusSeparator}>|</span>
          <span>Hex</span>
          <span className={styles.statusSeparator}>|</span>
          <span>
            Offset {formatHexOffset(currentOffset)} /{" "}
            {formatHexOffset(discoveredSize)}
            {!eofFoundRef.current && discoveredSize > 0 && "+"}
          </span>
          {hexSelection &&
            hexSelection.anchor !== hexSelection.head &&
            (() => {
              const selStart = Math.min(hexSelection.anchor, hexSelection.head);
              const selEnd = Math.max(hexSelection.anchor, hexSelection.head);
              const byteCount = selEnd - selStart + 1;
              return (
                <>
                  <span className={styles.statusSeparator}>|</span>
                  <span>
                    Sel: {formatHexOffset(selStart)}
                    {"\u2013"}
                    {formatHexOffset(selEnd)} ({formatSize(byteCount)})
                  </span>
                </>
              );
            })()}
          <span className={styles.statusSeparator}>|</span>
          <span>{formatSize(fileSize)}</span>
        </span>
        <ModeToggle currentMode="hex" autoMode={autoMode} />
      </div>
    </div>
  );
}
