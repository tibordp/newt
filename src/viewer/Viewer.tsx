import { invoke } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useSearchParams } from "react-router-dom";

import styles from "./Viewer.module.scss";
import { safeCommand, useRemoteState } from "../lib/ipc";
import type { VfsPath } from "../lib/types";

interface FileInfo {
  size: number;
  mime_type: string | null;
  is_dir: boolean;
  is_symlink: boolean;
  symlink_target: string | null;
  user: unknown;
  group: unknown;
  mode: unknown;
  modified: number | null;
  accessed: number | null;
  created: number | null;
}

const TEXT_MIME_PREFIXES = ["text/"];
const TEXT_MIME_TYPES = new Set([
  "application/json",
  "application/xml",
  "application/javascript",
  "application/typescript",
  "application/xhtml+xml",
  "application/x-sh",
  "application/x-csh",
  "application/x-httpd-php",
  "application/graphql",
  "application/sql",
  "application/x-yaml",
  "application/toml",
  "application/x-perl",
  "application/x-ruby",
  "application/x-python",
  "application/x-lua",
  "application/wasm",
  "application/ld+json",
  "application/manifest+json",
  "application/schema+json",
  "image/svg+xml",
]);

function isTextMime(mime: string | null): boolean {
  if (!mime) return false;
  if (TEXT_MIME_PREFIXES.some((p) => mime.startsWith(p))) return true;
  if (TEXT_MIME_TYPES.has(mime)) return true;
  // Catch-all for +xml, +json suffixes
  if (mime.endsWith("+xml") || mime.endsWith("+json")) return true;
  return false;
}

function isImageMime(mime: string | null): boolean {
  if (!mime) return false;
  return mime.startsWith("image/");
}

function isAudioMime(mime: string | null): boolean {
  if (!mime) return false;
  return mime.startsWith("audio/");
}

function isVideoMime(mime: string | null): boolean {
  if (!mime) return false;
  return mime.startsWith("video/");
}

function isPdfMime(mime: string | null): boolean {
  return mime === "application/pdf";
}

type ViewerMode = "text" | "hex" | "image" | "audio" | "video" | "pdf";

function detectAutoMode(mime: string | null): ViewerMode {
  if (isVideoMime(mime)) return "video";
  if (isAudioMime(mime)) return "audio";
  if (isPdfMime(mime)) return "pdf";
  if (isImageMime(mime)) return "image";
  if (isTextMime(mime)) return "text";
  return "hex";
}

interface FileChunk {
  data: number[];
  offset: number;
  total_size: number;
}

const CHUNK_SIZE = 128 * 1024;
const HEX_BYTES_PER_ROW = 16;

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function formatHexOffset(offset: number): string {
  return offset.toString(16).padStart(8, "0").toUpperCase();
}

function hexByte(b: number): string {
  return b.toString(16).padStart(2, "0").toUpperCase();
}

function printableAscii(b: number): string {
  return b >= 0x20 && b <= 0x7e ? String.fromCharCode(b) : ".";
}

// --- Chunked text loading ---

/**
 * Scan back at most 4 bytes from the end of a Uint8Array to find an
 * incomplete UTF-8 multi-byte sequence. Returns the number of trailing
 * bytes that form an incomplete sequence (0 if the tail is complete).
 */
function findIncompleteUtf8Tail(bytes: Uint8Array): number {
  const len = bytes.length;
  if (len === 0) return 0;

  // Check up to the last 4 bytes for a lead byte of an incomplete sequence
  for (let i = 1; i <= Math.min(4, len); i++) {
    const b = bytes[len - i];
    if ((b & 0x80) === 0) {
      // ASCII byte — everything before this (and this byte) is complete
      return 0;
    }
    if ((b & 0xc0) === 0xc0) {
      // This is a lead byte. Determine expected sequence length.
      let seqLen: number;
      if ((b & 0xe0) === 0xc0) seqLen = 2;
      else if ((b & 0xf0) === 0xe0) seqLen = 3;
      else if ((b & 0xf8) === 0xf0) seqLen = 4;
      else return 0; // Invalid lead byte, let TextDecoder handle it
      // If we have fewer bytes than the sequence requires, it's incomplete
      return i < seqLen ? i : 0;
    }
    // Otherwise it's a continuation byte (10xxxxxx), keep scanning back
  }
  // Scanned 4 continuation bytes without finding a lead — malformed, skip
  return 0;
}

interface TextLoadingState {
  lines: string[];
  bytesLoaded: number;
  done: boolean;
}

function useChunkedTextLoader(
  filePath: VfsPath,
  fileSize: number,
  enabled: boolean,
): TextLoadingState {
  const [state, setState] = useState<TextLoadingState>({
    lines: [],
    bytesLoaded: 0,
    done: false,
  });

  useEffect(() => {
    if (!enabled) return;

    // Empty file: immediately done
    if (fileSize === 0) {
      setState({ lines: [""], bytesLoaded: 0, done: true });
      return;
    }

    let cancelled = false;
    let partialLine = "";
    let carry = new Uint8Array(0);
    let accLines: string[] = [];
    let loaded = 0;

    (async () => {
      const decoder = new TextDecoder("utf-8", { fatal: false });

      while (loaded < fileSize && !cancelled) {
        const length = Math.min(CHUNK_SIZE, fileSize - loaded);
        const chunk: FileChunk = await invoke("read_file_range", {
          path: filePath,
          offset: loaded,
          length,
        });
        if (cancelled) break;

        let bytes = new Uint8Array(chunk.data);
        loaded += bytes.length;

        // Prepend carry from previous chunk
        if (carry.length > 0) {
          const merged = new Uint8Array(carry.length + bytes.length);
          merged.set(carry);
          merged.set(bytes, carry.length);
          bytes = merged;
          carry = new Uint8Array(0);
        }

        // Handle incomplete UTF-8 at the end (unless this is the last chunk)
        if (loaded < fileSize) {
          const tail = findIncompleteUtf8Tail(bytes);
          if (tail > 0) {
            carry = bytes.slice(bytes.length - tail);
            bytes = bytes.slice(0, bytes.length - tail);
          }
        }

        const text = decoder.decode(bytes, { stream: loaded < fileSize });
        const parts = text.split("\n");

        // Prepend carried partial line to first element
        parts[0] = partialLine + parts[0];
        // Pop last element as new partial line
        partialLine = parts.pop()!;

        // Append complete lines
        accLines = accLines.concat(parts);

        if (!cancelled) {
          const snapshot = accLines.slice();
          const currentLoaded = loaded;
          setState({
            lines: snapshot,
            bytesLoaded: currentLoaded,
            done: false,
          });
        }

        // If we got fewer bytes than expected, file was truncated
        if (chunk.data.length < length) break;
      }

      if (!cancelled) {
        // Flush remaining partial line
        accLines.push(partialLine);
        setState({
          lines: accLines,
          bytesLoaded: loaded,
          done: true,
        });
      }
    })();

    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [JSON.stringify(filePath), fileSize, enabled]);

  return state;
}

// --- Text mode rendering ---

interface TextViewerProps {
  lines: string[];
  filePath: string;
  fileSize: number;
  bytesLoaded: number;
  loading: boolean;
}

function TextViewer({
  lines,
  filePath,
  fileSize,
  bytesLoaded,
  loading,
}: TextViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [containerHeight, setContainerHeight] = useState(0);
  const lineHeight = 18;

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

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

  const totalHeight = lines.length * lineHeight;
  const startIdx = Math.max(0, Math.floor(scrollTop / lineHeight) - 5);
  const endIdx = Math.min(
    lines.length,
    Math.ceil((scrollTop + containerHeight) / lineHeight) + 5,
  );
  const visibleLines = lines.slice(startIdx, endIdx);

  const lineNumWidth = Math.max(4, String(lines.length).length);

  const handleScroll = useCallback((e: React.UIEvent<HTMLDivElement>) => {
    setScrollTop(e.currentTarget.scrollTop);
  }, []);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const el = containerRef.current;
      if (!el) return;

      if (e.key === "a" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        return;
      }

      const pageSize = Math.floor(containerHeight / lineHeight);
      switch (e.key) {
        case "Escape":
          safeCommand("close_window");
          e.preventDefault();
          break;
        case "ArrowUp":
          el.scrollTop -= lineHeight;
          e.preventDefault();
          break;
        case "ArrowDown":
          el.scrollTop += lineHeight;
          e.preventDefault();
          break;
        case "PageUp":
          el.scrollTop -= pageSize * lineHeight;
          e.preventDefault();
          break;
        case "PageDown":
          el.scrollTop += pageSize * lineHeight;
          e.preventDefault();
          break;
        case "Home":
          el.scrollTop = 0;
          e.preventDefault();
          break;
        case "End":
          el.scrollTop = totalHeight;
          e.preventDefault();
          break;
      }
    },
    [containerHeight, totalHeight],
  );

  const currentLine = Math.floor(scrollTop / lineHeight) + 1;

  const gutterWidth = `${lineNumWidth + 1}ch`;

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
        <div style={{ height: totalHeight, position: "relative" }}>
          <div
            className={styles.viewerGutter}
            style={{
              position: "absolute",
              top: startIdx * lineHeight,
              left: 0,
              width: gutterWidth,
            }}
          >
            {visibleLines.map((_, i) => {
              const lineNum = startIdx + i + 1;
              return (
                <div key={startIdx + i} className={styles.gutterLine}>
                  {String(lineNum).padStart(lineNumWidth, " ")}
                </div>
              );
            })}
          </div>
          <pre
            className={styles.viewerText}
            style={{
              position: "absolute",
              top: startIdx * lineHeight,
              left: gutterWidth,
              right: 0,
            }}
          >
            {visibleLines.map((line, i) => (
              <div key={startIdx + i} className={styles.viewerLine}>
                {line}
                {"\n"}
              </div>
            ))}
          </pre>
        </div>
      </div>
      <div className={styles.viewerStatus}>
        <span>{filePath}</span>
        <span className={styles.statusSeparator}>|</span>
        <span>Text</span>
        <span className={styles.statusSeparator}>|</span>
        <span>
          Line {currentLine} / {lines.length}
          {loading ? "+" : ""}
        </span>
        <span className={styles.statusSeparator}>|</span>
        <span>{formatSize(fileSize)}</span>
        {loading && (
          <>
            <span className={styles.statusSeparator}>|</span>
            <span>Loading {Math.round((bytesLoaded / fileSize) * 100)}%</span>
          </>
        )}
      </div>
    </div>
  );
}

// --- Hex mode rendering ---

interface HexViewerProps {
  filePath: string;
  fileSize: number;
  chunkCache: React.MutableRefObject<Map<number, Uint8Array>>;
  loadChunk: (chunkIndex: number) => Promise<void>;
}

const MAX_SCROLL_HEIGHT = 16_000_000; // stay under browser element height limit

function HexViewer({
  filePath,
  fileSize,
  chunkCache,
  loadChunk,
}: HexViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [topRow, setTopRow] = useState(0);
  const [containerHeight, setContainerHeight] = useState(0);
  const [, forceUpdate] = useState(0);
  const rowHeight = 18;

  // Ref to detect programmatic scrollTop changes and avoid feedback loops
  const programmaticScrollRef = useRef<number | null>(null);
  // Accumulator for pixel-based wheel deltas
  const wheelAccumulatorRef = useRef(0);

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  const totalRows = Math.ceil(fileSize / HEX_BYTES_PER_ROW);
  const visibleRows = Math.floor(containerHeight / rowHeight);
  const maxRow = Math.max(0, totalRows - visibleRows);
  const scrollableHeight = Math.min(totalRows * rowHeight, MAX_SCROLL_HEIGHT);

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

  // Sync scrollbar position when topRow changes
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const maxScrollTop = scrollableHeight - containerHeight;
    if (maxScrollTop <= 0) return;
    const newScrollTop =
      maxRow > 0 ? (clampedTopRow / maxRow) * maxScrollTop : 0;
    programmaticScrollRef.current = newScrollTop;
    el.scrollTop = newScrollTop;
  }, [clampedTopRow, maxRow, scrollableHeight, containerHeight]);

  // Calculate visible rows with overscan
  const startRow = Math.max(0, clampedTopRow - 5);
  const endRow = Math.min(totalRows, clampedTopRow + visibleRows + 5);

  // Determine which chunks we need
  const startByte = startRow * HEX_BYTES_PER_ROW;
  const endByte = endRow * HEX_BYTES_PER_ROW;
  const startChunk = Math.floor(startByte / CHUNK_SIZE);
  const endChunk = Math.floor(endByte / CHUNK_SIZE);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      let loaded = false;
      for (let ci = startChunk; ci <= endChunk; ci++) {
        if (!chunkCache.current.has(ci)) {
          await loadChunk(ci);
          loaded = true;
        }
      }
      if (loaded && !cancelled) {
        forceUpdate((n) => n + 1);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [startChunk, endChunk, loadChunk]);

  const getByteAt = useCallback(
    (offset: number): number | undefined => {
      if (offset >= fileSize) return undefined;
      const ci = Math.floor(offset / CHUNK_SIZE);
      const chunk = chunkCache.current.get(ci);
      if (!chunk) return undefined;
      const localOffset = offset - ci * CHUNK_SIZE;
      if (localOffset >= chunk.length) return undefined;
      return chunk[localOffset];
    },
    [fileSize],
  );

  const renderRow = useCallback(
    (rowIdx: number) => {
      const offset = rowIdx * HEX_BYTES_PER_ROW;
      const hexParts: string[] = [];
      const asciiParts: string[] = [];

      for (let i = 0; i < HEX_BYTES_PER_ROW; i++) {
        const b = getByteAt(offset + i);
        if (b === undefined) {
          hexParts.push("  ");
          asciiParts.push(" ");
        } else {
          hexParts.push(hexByte(b));
          asciiParts.push(printableAscii(b));
        }
      }

      // Group hex bytes with an extra space every 8 bytes
      const hexLeft = hexParts.slice(0, 8).join(" ");
      const hexRight = hexParts.slice(8, 16).join(" ");
      const hex = `${hexLeft}  ${hexRight}`;

      return {
        gutter: `${formatHexOffset(offset)}  ${hex}`,
        ascii: asciiParts.join(""),
      };
    },
    [getByteAt],
  );

  const rowData: { gutter: string; ascii: string }[] = [];
  for (let r = startRow; r < endRow; r++) {
    rowData.push(renderRow(r));
  }

  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      const scrollTop = e.currentTarget.scrollTop;
      // If this scroll event was triggered by our programmatic update, ignore it
      if (
        programmaticScrollRef.current !== null &&
        Math.abs(scrollTop - programmaticScrollRef.current) <= 1
      ) {
        programmaticScrollRef.current = null;
        return;
      }
      programmaticScrollRef.current = null;

      // User-initiated scrollbar drag — reverse-map to topRow
      const maxScrollTop = scrollableHeight - containerHeight;
      if (maxScrollTop > 0) {
        const newTopRow = Math.round((scrollTop / maxScrollTop) * maxRow);
        setTopRow(Math.max(0, Math.min(newTopRow, maxRow)));
      }
    },
    [scrollableHeight, containerHeight, maxRow],
  );

  // Register wheel handler as non-passive so preventDefault() works
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault();
      let rowDelta: number;
      if (e.deltaMode === 1) {
        // Line mode — deltaY is already in lines
        rowDelta = e.deltaY;
      } else {
        // Pixel mode — accumulate until we cross a row boundary
        wheelAccumulatorRef.current += e.deltaY;
        rowDelta = Math.trunc(wheelAccumulatorRef.current / rowHeight);
        wheelAccumulatorRef.current -= rowDelta * rowHeight;
      }
      if (rowDelta !== 0) {
        setTopRow((prev) => Math.max(0, Math.min(prev + rowDelta, maxRow)));
      }
    };
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, [maxRow, rowHeight]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "a" && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        return;
      }

      switch (e.key) {
        case "Escape":
          safeCommand("close_window");
          e.preventDefault();
          break;
        case "ArrowUp":
          setTopRow((prev) => Math.max(0, prev - 1));
          e.preventDefault();
          break;
        case "ArrowDown":
          setTopRow((prev) => Math.min(maxRow, prev + 1));
          e.preventDefault();
          break;
        case "PageUp":
          setTopRow((prev) => Math.max(0, prev - visibleRows));
          e.preventDefault();
          break;
        case "PageDown":
          setTopRow((prev) => Math.min(maxRow, prev + visibleRows));
          e.preventDefault();
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
    [maxRow, visibleRows],
  );

  const currentOffset = clampedTopRow * HEX_BYTES_PER_ROW;

  // Read current scrollTop for row positioning
  const currentScrollTop = containerRef.current?.scrollTop ?? 0;

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
        <div style={{ height: scrollableHeight, position: "relative" }}>
          <div
            className={`${styles.viewerGutter} ${styles.hexGutter}`}
            style={{
              position: "absolute",
              top: currentScrollTop,
              left: 0,
            }}
          >
            {rowData.map((row, i) => (
              <div key={startRow + i} className={styles.gutterLine}>
                {row.gutter}
              </div>
            ))}
          </div>
          <pre
            className={styles.viewerText}
            style={{
              position: "absolute",
              top: currentScrollTop,
              left: "61ch",
              right: 0,
            }}
          >
            {rowData.map((row, i) => (
              <div
                key={startRow + i}
                className={`${styles.viewerLine} ${styles.hexAscii}`}
              >
                {row.ascii}
                {"\n"}
              </div>
            ))}
          </pre>
        </div>
      </div>
      <div className={styles.viewerStatus}>
        <span>{filePath}</span>
        <span className={styles.statusSeparator}>|</span>
        <span>Hex</span>
        <span className={styles.statusSeparator}>|</span>
        <span>
          Offset {formatHexOffset(currentOffset)} / {formatHexOffset(fileSize)}
        </span>
        <span className={styles.statusSeparator}>|</span>
        <span>{formatSize(fileSize)}</span>
      </div>
    </div>
  );
}

// --- Image mode rendering ---

interface ImageViewerProps {
  filePath: string;
  fileUrl: string;
  fileSize: number;
}

function clampView(
  z: number,
  px: number,
  py: number,
  ns: { w: number; h: number },
  cw: number,
  ch: number,
) {
  const minZoom = Math.min(cw / ns.w, ch / ns.h, 1);
  const zoom = Math.max(z, minZoom);
  const imgW = ns.w * zoom;
  const imgH = ns.h * zoom;
  return {
    zoom,
    pan: {
      x: imgW <= cw ? (cw - imgW) / 2 : Math.min(0, Math.max(cw - imgW, px)),
      y: imgH <= ch ? (ch - imgH) / 2 : Math.min(0, Math.max(ch - imgH, py)),
    },
  };
}

function ImageViewer({ filePath, fileUrl, fileSize }: ImageViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [naturalSize, setNaturalSize] = useState<{
    w: number;
    h: number;
  } | null>(null);
  const [imageError, setImageError] = useState(false);
  const dragStart = useRef<{
    x: number;
    y: number;
    panX: number;
    panY: number;
  } | null>(null);

  // Keep a ref to latest state so native event listeners can read it
  const stateRef = useRef({ zoom, pan, naturalSize });
  stateRef.current = { zoom, pan, naturalSize };

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  const applyView = useCallback((z: number, px: number, py: number) => {
    const container = containerRef.current;
    const ns = stateRef.current.naturalSize;
    if (!ns || !container) return;
    const v = clampView(
      z,
      px,
      py,
      ns,
      container.clientWidth,
      container.clientHeight,
    );
    setZoom(v.zoom);
    setPan(v.pan);
  }, []);

  // Re-clamp on container resize (e.g. window resize while at min zoom)
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver(() => {
      const { zoom: z, pan: p, naturalSize: ns } = stateRef.current;
      if (!ns) return;
      const v = clampView(
        z,
        p.x,
        p.y,
        ns,
        container.clientWidth,
        container.clientHeight,
      );
      setZoom(v.zoom);
      setPan(v.pan);
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  const resetView = useCallback(() => {
    const container = containerRef.current;
    const ns = stateRef.current.naturalSize;
    if (!ns || !container) return;
    const cw = container.clientWidth;
    const ch = container.clientHeight;
    const z = Math.min(cw / ns.w, ch / ns.h, 1);
    applyView(z, (cw - ns.w * z) / 2, (ch - ns.h * z) / 2);
  }, [applyView]);

  const handleLoad = useCallback(() => {
    const img = imgRef.current;
    const container = containerRef.current;
    if (!img || !container) return;
    const ns = { w: img.naturalWidth, h: img.naturalHeight };
    setNaturalSize(ns);
    const cw = container.clientWidth;
    const ch = container.clientHeight;
    const z = Math.min(cw / ns.w, ch / ns.h, 1);
    setZoom(z);
    setPan({ x: (cw - ns.w * z) / 2, y: (ch - ns.h * z) / 2 });
  }, []);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
        case "Escape":
          safeCommand("close_window");
          e.preventDefault();
          break;
        case "0":
          resetView();
          e.preventDefault();
          break;
      }
    },
    [resetView],
  );

  // Non-passive wheel listener so we can preventDefault (React wheel events are passive)
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handler = (e: WheelEvent) => {
      const { zoom: curZoom, pan: curPan, naturalSize: ns } = stateRef.current;
      if (!ns) return;

      const rect = container.getBoundingClientRect();
      const minZoom = Math.min(rect.width / ns.w, rect.height / ns.h, 1);
      const maxZoom = 50;

      // If already at the limit in the scroll direction, let the event pass through
      const zoomingOut = e.deltaY > 0;
      if (
        (zoomingOut && curZoom <= minZoom) ||
        (!zoomingOut && curZoom >= maxZoom)
      )
        return;

      e.preventDefault();

      const mouseX = e.clientX - rect.left;
      const mouseY = e.clientY - rect.top;

      const factor = zoomingOut ? 0.9 : 1 / 0.9;
      const rawZoom = Math.min(curZoom * factor, maxZoom);

      // Image-space point under cursor
      const imgX = (mouseX - curPan.x) / curZoom;
      const imgY = (mouseY - curPan.y) / curZoom;

      // Pan that keeps the same image point under cursor
      const rawPanX = mouseX - imgX * rawZoom;
      const rawPanY = mouseY - imgY * rawZoom;

      const v = clampView(
        rawZoom,
        rawPanX,
        rawPanY,
        ns,
        rect.width,
        rect.height,
      );
      setZoom(v.zoom);
      setPan(v.pan);
    };

    container.addEventListener("wheel", handler, { passive: false });
    return () => container.removeEventListener("wheel", handler);
  }, []);

  // Drag-to-pan: mousedown on container, mousemove/mouseup on window
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (e.button === 0 || e.button === 1) {
      const container = containerRef.current;
      const { zoom: z, pan: curPan, naturalSize: ns } = stateRef.current;
      if (!ns || !container) return;
      // Don't start drag if image fits entirely within container
      if (
        ns.w * z <= container.clientWidth &&
        ns.h * z <= container.clientHeight
      )
        return;
      e.preventDefault();
      dragStart.current = {
        x: e.clientX,
        y: e.clientY,
        panX: curPan.x,
        panY: curPan.y,
      };
    }
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    const handleMouseMove = (e: MouseEvent) => {
      if (!dragStart.current || !container) return;
      const ns = stateRef.current.naturalSize;
      if (!ns) return;
      const dx = e.clientX - dragStart.current.x;
      const dy = e.clientY - dragStart.current.y;
      const v = clampView(
        stateRef.current.zoom,
        dragStart.current.panX + dx,
        dragStart.current.panY + dy,
        ns,
        container.clientWidth,
        container.clientHeight,
      );
      setPan(v.pan);
    };
    const handleMouseUp = () => {
      dragStart.current = null;
    };
    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };
  }, []);

  const zoomPercent = Math.round(zoom * 100);

  return (
    <div
      className={styles.viewer}
      ref={viewerRef}
      tabIndex={-1}
      onKeyDown={handleKeyDown}
    >
      <div
        className={styles.imageContent}
        ref={containerRef}
        onMouseDown={handleMouseDown}
      >
        {imageError ? (
          <div className={styles.imageErrorMessage}>
            Unable to display image preview
          </div>
        ) : (
          <img
            ref={imgRef}
            className={styles.imagePreview}
            src={fileUrl}
            alt={filePath}
            onLoad={handleLoad}
            onError={() => setImageError(true)}
            draggable={false}
            style={{
              transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
              transformOrigin: "0 0",
            }}
          />
        )}
      </div>
      <div className={styles.viewerStatus}>
        <span>{filePath}</span>
        <span className={styles.statusSeparator}>|</span>
        <span>Image</span>
        {naturalSize && (
          <>
            <span className={styles.statusSeparator}>|</span>
            <span>
              {naturalSize.w} x {naturalSize.h}
            </span>
          </>
        )}
        {!imageError && (
          <>
            <span className={styles.statusSeparator}>|</span>
            <span>{zoomPercent}%</span>
          </>
        )}
        <span className={styles.statusSeparator}>|</span>
        <span>{formatSize(fileSize)}</span>
      </div>
    </div>
  );
}

// --- Media mode rendering (audio + video) ---

interface MediaViewerProps {
  tag: "audio" | "video";
  filePath: string;
  fileUrl: string;
  fileSize: number;
}

function MediaViewer({ tag, filePath, fileUrl, fileSize }: MediaViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const [mediaError, setMediaError] = useState<string | null>(null);

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      safeCommand("close_window");
      e.preventDefault();
    }
  }, []);

  const modeName = tag === "audio" ? "Audio" : "Video";

  return (
    <div
      className={styles.viewer}
      ref={viewerRef}
      tabIndex={-1}
      onKeyDown={handleKeyDown}
    >
      <div className={styles.mediaContent}>
        {mediaError ? (
          <div className={styles.imageErrorMessage}>
            Unable to play {modeName.toLowerCase()} preview: {mediaError}
          </div>
        ) : tag === "audio" ? (
          <audio
            className={styles.audioPlayer}
            controls
            src={fileUrl}
            onError={(e) => {
              const el = e.currentTarget;
              const err = el.error;
              const detail = err
                ? `${err.message} (code ${err.code})`
                : "unknown error";
              console.error(
                `${modeName} error:`,
                detail,
                "src:",
                fileUrl,
                "networkState:",
                el.networkState,
                "readyState:",
                el.readyState,
              );
              setMediaError(detail);
            }}
          />
        ) : (
          <video
            className={styles.videoPlayer}
            controls
            src={fileUrl}
            onError={(e) => {
              const el = e.currentTarget;
              const err = el.error;
              const detail = err
                ? `${err.message} (code ${err.code})`
                : "unknown error";
              console.error(
                `${modeName} error:`,
                detail,
                "src:",
                fileUrl,
                "networkState:",
                el.networkState,
                "readyState:",
                el.readyState,
              );
              setMediaError(detail);
            }}
          />
        )}
      </div>
      <div className={styles.viewerStatus}>
        <span>{filePath}</span>
        <span className={styles.statusSeparator}>|</span>
        <span>{modeName}</span>
        <span className={styles.statusSeparator}>|</span>
        <span>{formatSize(fileSize)}</span>
      </div>
    </div>
  );
}

// --- PDF mode rendering ---

interface PdfViewerProps {
  filePath: string;
  fileUrl: string;
  fileSize: number;
}

function PdfViewer({ filePath, fileUrl, fileSize }: PdfViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const [pdfError, setPdfError] = useState(false);

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      safeCommand("close_window");
      e.preventDefault();
    }
  }, []);

  return (
    <div
      className={styles.viewer}
      ref={viewerRef}
      tabIndex={-1}
      onKeyDown={handleKeyDown}
    >
      {pdfError ? (
        <div className={styles.mediaContent}>
          <div className={styles.imageErrorMessage}>
            PDF preview not available
          </div>
        </div>
      ) : (
        <embed
          className={styles.pdfEmbed}
          type="application/pdf"
          src={fileUrl}
          onError={() => setPdfError(true)}
        />
      )}
      <div className={styles.viewerStatus}>
        <span>{filePath}</span>
        <span className={styles.statusSeparator}>|</span>
        <span>PDF</span>
        <span className={styles.statusSeparator}>|</span>
        <span>{formatSize(fileSize)}</span>
      </div>
    </div>
  );
}

// --- Main Viewer component ---

function Viewer() {
  const [searchParams] = useSearchParams();
  const displayPath = searchParams.get("path") || "";
  const filePath: VfsPath = JSON.parse(
    searchParams.get("vfs_path") ||
      `{"vfs_id":0,"path":${JSON.stringify(displayPath)}}`,
  );
  const fileServerBase = searchParams.get("file_server_base") || "";
  const fileUrl = `${fileServerBase}/${filePath.vfs_id}${filePath.path}`;

  const viewerState = useRemoteState<{ mode: string }>("viewer");

  const [info, setInfo] = useState<FileInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  const chunkCache = useRef(new Map<number, Uint8Array>());

  // Fetch file info on mount and push auto-detected mode to Rust
  useEffect(() => {
    if (!displayPath) return;
    document.title = displayPath;

    (async () => {
      try {
        const fi: FileInfo = await invoke("file_details", { path: filePath });
        setInfo(fi);
        const mode = detectAutoMode(fi.mime_type);
        invoke("set_viewer_mode", { mode }).catch(() => {});
      } catch (e: any) {
        setError(e.toString());
        await message(e.toString(), { kind: "error", title: "Error" });
      }
    })();
  }, [displayPath]);

  const currentMode = (viewerState?.mode as ViewerMode) ?? null;

  // Preload first hex chunk when switching to hex mode (or when auto-detected as hex)
  useEffect(() => {
    if (!info || currentMode !== "hex") return;
    if (chunkCache.current.has(0)) return;

    (async () => {
      try {
        const chunk: FileChunk = await invoke("read_file_range", {
          path: filePath,
          offset: 0,
          length: CHUNK_SIZE,
        });
        chunkCache.current.set(0, new Uint8Array(chunk.data));
      } catch (e: any) {
        console.error("Failed to preload first chunk", e);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentMode, info]);

  // Chunked text loading — enabled only in text mode
  const textState = useChunkedTextLoader(
    filePath,
    info?.size ?? 0,
    info !== null && currentMode === "text",
  );

  const loadChunk = useCallback(
    async (chunkIndex: number) => {
      if (chunkCache.current.has(chunkIndex)) return;
      const offset = chunkIndex * CHUNK_SIZE;
      try {
        const chunk: FileChunk = await invoke("read_file_range", {
          path: filePath,
          offset,
          length: CHUNK_SIZE,
        });
        chunkCache.current.set(chunkIndex, new Uint8Array(chunk.data));
      } catch (e: any) {
        console.error("Failed to load chunk", chunkIndex, e);
      }
    },
    [displayPath],
  );

  // Focus the viewer on mount
  const viewerRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    viewerRef.current?.focus();
  }, [info, error]);

  const onKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      safeCommand("close_window");
      e.preventDefault();
    }
  }, []);

  if (error) {
    return (
      <div
        className={styles.viewer}
        ref={viewerRef}
        tabIndex={-1}
        onKeyDown={onKeyDown}
      >
        <div className={styles.viewerContent} />
        <div className={styles.viewerStatus}>
          <span className={styles.statusError}>{error}</span>
        </div>
      </div>
    );
  }

  if (!info || !currentMode) {
    return (
      <div
        className={styles.viewer}
        ref={viewerRef}
        tabIndex={-1}
        onKeyDown={onKeyDown}
      >
        <div className={styles.viewerContent} />
        <div className={styles.viewerStatus}>
          <span>Loading...</span>
        </div>
      </div>
    );
  }

  if (currentMode === "text") {
    return (
      <TextViewer
        lines={textState.lines}
        filePath={displayPath}
        fileSize={info.size}
        bytesLoaded={textState.bytesLoaded}
        loading={!textState.done}
      />
    );
  }

  if (currentMode === "image") {
    return (
      <ImageViewer
        filePath={displayPath}
        fileUrl={fileUrl}
        fileSize={info.size}
      />
    );
  }

  if (currentMode === "audio" || currentMode === "video") {
    return (
      <MediaViewer
        tag={currentMode}
        filePath={displayPath}
        fileUrl={fileUrl}
        fileSize={info.size}
      />
    );
  }

  if (currentMode === "pdf") {
    return (
      <PdfViewer
        filePath={displayPath}
        fileUrl={fileUrl}
        fileSize={info.size}
      />
    );
  }

  return (
    <HexViewer
      filePath={displayPath}
      fileSize={info.size}
      chunkCache={chunkCache}
      loadChunk={loadChunk}
    />
  );
}

export default Viewer;
