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
import { safeCommand } from "../lib/ipc";
import type { VfsPath } from "../lib/types";

interface FileInfo {
  size: number;
  is_binary: boolean;
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
  enabled: boolean
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

function TextViewer({ lines, filePath, fileSize, bytesLoaded, loading }: TextViewerProps) {
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
    Math.ceil((scrollTop + containerHeight) / lineHeight) + 5
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
    [containerHeight, totalHeight]
  );

  const currentLine = Math.floor(scrollTop / lineHeight) + 1;

  const gutterWidth = `${lineNumWidth + 1}ch`;

  return (
    <div className={styles.viewer} ref={viewerRef} tabIndex={-1} onKeyDown={handleKeyDown}>
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
          Line {currentLine} / {lines.length}{loading ? "+" : ""}
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

function HexViewer({
  filePath,
  fileSize,
  chunkCache,
  loadChunk,
}: HexViewerProps) {
  const viewerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [containerHeight, setContainerHeight] = useState(0);
  const [, forceUpdate] = useState(0);
  const rowHeight = 18;

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  const totalRows = Math.ceil(fileSize / HEX_BYTES_PER_ROW);
  const totalHeight = totalRows * rowHeight;

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

  // Calculate visible rows
  const startRow = Math.max(0, Math.floor(scrollTop / rowHeight) - 5);
  const endRow = Math.min(
    totalRows,
    Math.ceil((scrollTop + containerHeight) / rowHeight) + 5
  );

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
    [fileSize]
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
    [getByteAt]
  );

  const rowData: { gutter: string; ascii: string }[] = [];
  for (let r = startRow; r < endRow; r++) {
    rowData.push(renderRow(r));
  }

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

      const pageSize = Math.floor(containerHeight / rowHeight);
      switch (e.key) {
        case "Escape":
          safeCommand("close_window");
          e.preventDefault();
          break;
        case "ArrowUp":
          el.scrollTop -= rowHeight;
          e.preventDefault();
          break;
        case "ArrowDown":
          el.scrollTop += rowHeight;
          e.preventDefault();
          break;
        case "PageUp":
          el.scrollTop -= pageSize * rowHeight;
          e.preventDefault();
          break;
        case "PageDown":
          el.scrollTop += pageSize * rowHeight;
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
    [containerHeight, totalHeight]
  );

  const currentOffset = Math.floor(scrollTop / rowHeight) * HEX_BYTES_PER_ROW;

  return (
    <div className={styles.viewer} ref={viewerRef} tabIndex={-1} onKeyDown={handleKeyDown}>
      <div
        className={styles.viewerContent}
        ref={containerRef}
        onScroll={handleScroll}
      >
        <div style={{ height: totalHeight, position: "relative" }}>
          <div
            className={`${styles.viewerGutter} ${styles.hexGutter}`}
            style={{
              position: "absolute",
              top: startRow * rowHeight,
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
              top: startRow * rowHeight,
              left: "61ch",
              right: 0,
            }}
          >
            {rowData.map((row, i) => (
              <div key={startRow + i} className={`${styles.viewerLine} ${styles.hexAscii}`}>
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

// --- Main Viewer component ---

function Viewer() {
  const [searchParams] = useSearchParams();
  const displayPath = searchParams.get("path") || "";
  const filePath: VfsPath = JSON.parse(
    searchParams.get("vfs_path") || `{"vfs_id":0,"path":${JSON.stringify(displayPath)}}`
  );

  const [info, setInfo] = useState<FileInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  const chunkCache = useRef(new Map<number, Uint8Array>());

  // Fetch file info (and first binary chunk) on mount
  useEffect(() => {
    if (!displayPath) return;
    document.title = displayPath;

    (async () => {
      try {
        const fi: FileInfo = await invoke("file_details", { path: filePath });
        setInfo(fi);

        if (fi.is_binary) {
          // Binary file: load first chunk into cache
          const chunk: FileChunk = await invoke("read_file_range", {
            path: filePath,
            offset: 0,
            length: CHUNK_SIZE,
          });
          chunkCache.current.set(0, new Uint8Array(chunk.data));
        }
      } catch (e: any) {
        setError(e.toString());
        await message(e.toString(), { kind: "error", title: "Error" });
      }
    })();
  }, [displayPath]);

  // Chunked text loading — enabled once we know it's a text file
  const textState = useChunkedTextLoader(
    filePath,
    info?.size ?? 0,
    info !== null && !info.is_binary
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
    [displayPath]
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
      <div className={styles.viewer} ref={viewerRef} tabIndex={-1} onKeyDown={onKeyDown}>
        <div className={styles.viewerContent} />
        <div className={styles.viewerStatus}>
          <span className={styles.statusError}>{error}</span>
        </div>
      </div>
    );
  }

  if (!info) {
    return (
      <div className={styles.viewer} ref={viewerRef} tabIndex={-1} onKeyDown={onKeyDown}>
        <div className={styles.viewerContent} />
        <div className={styles.viewerStatus}>
          <span>Loading...</span>
        </div>
      </div>
    );
  }

  if (!info.is_binary) {
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
