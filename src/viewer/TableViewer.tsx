import React, {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import * as CM from "@radix-ui/react-context-menu";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

import styles from "./Viewer.module.scss";
import tableStyles from "./TableViewer.module.scss";
import menuStyles from "../main_window/Menu.module.scss";
import { safe, unwrap } from "../lib/ipc";
import {
  commands,
  type TableDelimiter,
  type TableOptions,
} from "../lib/bindings";
import { useFormatBytes } from "../lib/size";
import { useCommandShortcuts, useScopedBindings } from "../lib/scopedBindings";
import { GoToBar } from "./GoToDialog";
import { SearchBar, type SearchMatch } from "./SearchBar";
import { ModeToggle } from "./ModeToggle";
import {
  CHUNK_SIZE,
  MAX_SCROLL_HEIGHT,
  SNIFF_PREFIX_LEN,
  LruChunkCache,
  collectBytes,
  makeDecoder,
  newlineScan,
  type FileChunk,
  type ViewerMode,
  type VfsPath,
} from "./helpers";
import {
  DELIMITER_CHARS,
  RowScanner,
  columnName,
  detectDelimiter,
  detectHeader,
  fieldAtOffset,
  formatRows,
  isNumeric,
  parseRow,
  parseRows,
  type Dialect,
} from "./table";
import { useEmbeddedCopy, useViewerHost } from "./host";

const ROW_HEIGHT = 22;
const DEFAULT_WIDTH = 100;
const MIN_WIDTH = 24;
const MAX_FIT_WIDTH = 480;
const CELL_PADDING = 14;
/// Select mode's text box: line height, and how far it may grow over its
/// neighbours — wider, then taller as long lines wrap — before it scrolls.
const BOX_LINE_HEIGHT = 18;
const BOX_BORDER = 2;
const BOX_PADDING_X = 5;
const BOX_MAX_WIDTH = 640;
const BOX_MAX_LINES = 12;
/// Same cap as the text viewer's copy.
const MAX_COPY_BYTES = 10 * 1024 * 1024;

interface Cell {
  row: number;
  col: number;
}

/// `columns`/`rows`/`all` span to the end of the table in the other axis,
/// however much of it has been scanned.
type SelectionKind = "cells" | "columns" | "rows" | "all";

interface Selection {
  kind: SelectionKind;
  anchor: Cell;
  head: Cell;
}

/// Inclusive bounds; `Infinity` runs to the last row or column.
interface Rect {
  r0: number;
  r1: number;
  c0: number;
  c1: number;
}

function selectionRect(sel: Selection): Rect {
  const r0 = Math.min(sel.anchor.row, sel.head.row);
  const r1 = Math.max(sel.anchor.row, sel.head.row);
  const c0 = Math.min(sel.anchor.col, sel.head.col);
  const c1 = Math.max(sel.anchor.col, sel.head.col);
  switch (sel.kind) {
    case "columns":
      return { r0: 0, r1: Infinity, c0, c1 };
    case "rows":
      return { r0, r1, c0: 0, c1: Infinity };
    case "all":
      return { r0: 0, r1: Infinity, c0: 0, c1: Infinity };
    default:
      return { r0, r1, c0, c1 };
  }
}

const inRect = (rect: Rect | null, row: number, col: number) =>
  rect !== null &&
  row >= rect.r0 &&
  row <= rect.r1 &&
  col >= rect.c0 &&
  col <= rect.c1;

/// Largest index whose value is ≤ `x` in an ascending array.
function floorIndex(values: number[], x: number): number {
  let lo = 0;
  let hi = values.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (values[mid] <= x) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

function extensionHint(path: string): TableDelimiter {
  return /\.(tsv|tab)$/i.test(path) ? "tab" : "comma";
}

const displayText = (value: string) => value.replace(/\r?\n/g, "\u21b5");

/// Put the caret at the start or end of a text field; `extend` moves the
/// selection's moving end there instead, keeping its anchor.
function moveCaret(box: HTMLTextAreaElement, start: boolean, extend: boolean) {
  const to = start ? 0 : box.value.length;
  if (!extend) {
    box.setSelectionRange(to, to);
    return;
  }
  const anchor =
    box.selectionDirection === "backward"
      ? box.selectionEnd
      : box.selectionStart;
  box.setSelectionRange(
    Math.min(anchor, to),
    Math.max(anchor, to),
    to < anchor ? "backward" : "forward",
  );
}

interface TableViewerProps {
  filePath: string;
  vfsPath: VfsPath;
  fileSize: number;
  chunkCache: React.MutableRefObject<LruChunkCache>;
  loadChunk: (chunkIndex: number) => Promise<void>;
  autoMode: ViewerMode;
  encoding: string;
  bomLen: number;
  encodingLabel: string | null;
  needsSniff: boolean;
  options: TableOptions;
}

/**
 * Spreadsheet-style view of a delimited file. Rows are found by a
 * streaming scan of the raw chunks that carries quote state across chunk
 * boundaries (a quoted field may hold newlines), extended as the view
 * approaches the scanned end; only the rows on screen are decoded.
 */
export function TableViewer({
  filePath,
  vfsPath,
  fileSize,
  chunkCache,
  loadChunk,
  autoMode,
  encoding,
  bomLen,
  encodingLabel,
  needsSniff,
  options,
}: TableViewerProps) {
  const formatSize = useFormatBytes();
  const viewerHost = useViewerHost();
  const shortcuts = useCommandShortcuts();
  const gridId = useId();
  const viewerRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const headerRef = useRef<HTMLDivElement>(null);
  const scan = newlineScan(encoding);

  // --- Detection: delimiter and header row, from the first chunk ---

  const [detected, setDetected] = useState<{
    delimiter: TableDelimiter;
    header: boolean;
    sampleRows: string[][];
  } | null>(null);
  const sniffSentRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      if (!chunkCache.current.has(0)) await loadChunk(0);
      const chunk = chunkCache.current.get(0);
      if (cancelled || !chunk) return;
      if (needsSniff && !sniffSentRef.current) {
        sniffSentRef.current = true;
        safe(
          viewerHost.sniffEncoding(
            Array.from(chunk.subarray(0, SNIFF_PREFIX_LEN)),
            chunk.length < CHUNK_SIZE,
          ),
        );
      }
      const complete = chunk.length < CHUNK_SIZE;
      const sample = makeDecoder(encoding).decode(chunk.subarray(bomLen));
      const auto = detectDelimiter(
        sample,
        options.quoted,
        complete,
        extensionHint(filePath),
      );
      const delimiter = options.delimiter ?? auto;
      const dialect = {
        delimiter: DELIMITER_CHARS[delimiter],
        quoted: options.quoted,
      };
      const header = detectHeader(sample, dialect, complete);
      const rows = parseRows(sample, dialect);
      setDetected({
        delimiter,
        header,
        sampleRows: complete ? rows : rows.slice(0, -1),
      });
      safe(viewerHost.reportTableDetection(auto, header));
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filePath, encoding, bomLen, options.delimiter, options.quoted]);

  const dialect: Dialect | null = useMemo(
    () =>
      detected && {
        delimiter: DELIMITER_CHARS[detected.delimiter],
        quoted: options.quoted,
      },
    [detected, options.quoted],
  );
  const header = options.header ?? detected?.header ?? false;
  const headerRows = header ? 1 : 0;
  const scanKey = dialect
    ? `${dialect.delimiter}:${dialect.quoted}:${scan}:${bomLen}`
    : null;

  // --- Row index ---

  // rowStarts[i] = byte offset where file row i begins. All chunks up to
  // scannedTo have been fed to the scanner, in order.
  const rowStartsRef = useRef<number[]>([bomLen]);
  const scannerRef = useRef<RowScanner | null>(null);
  const scannedToRef = useRef(bomLen);
  const eofRef = useRef(false);
  // Bumped when the index is rebuilt; an old scan loop stops at its next step.
  const genRef = useRef(0);
  const scanPromiseRef = useRef<Promise<void> | null>(null);
  const targetRowsRef = useRef(200);
  const targetByteRef = useRef(0);
  const [maxFields, setMaxFields] = useState(1);
  // Bumped whenever bytes become renderable (chunk loaded / scan advanced).
  const [renderGen, setRenderGen] = useState(0);

  const pump = useCallback((): Promise<void> => {
    if (scanPromiseRef.current) return scanPromiseRef.current;
    const gen = genRef.current;
    const run = (async () => {
      while (
        gen === genRef.current &&
        scannerRef.current &&
        !eofRef.current &&
        (rowStartsRef.current.length <= targetRowsRef.current ||
          scannedToRef.current <= targetByteRef.current)
      ) {
        const ci = Math.floor(scannedToRef.current / CHUNK_SIZE);
        if (!chunkCache.current.has(ci)) await loadChunk(ci);
        if (gen !== genRef.current) return;
        const chunk = chunkCache.current.get(ci);
        if (!chunk) return;
        const chunkStart = ci * CHUNK_SIZE;
        scannerRef.current.push(
          chunk,
          chunkStart,
          scannedToRef.current,
          rowStartsRef.current,
        );
        scannedToRef.current = chunkStart + chunk.length;
        if (chunk.length < CHUNK_SIZE) eofRef.current = true;
        setMaxFields(scannerRef.current.maxFields);
        setRenderGen((n) => n + 1);
      }
    })();
    const promise = run.finally(() => {
      if (scanPromiseRef.current === promise) scanPromiseRef.current = null;
    });
    scanPromiseRef.current = promise;
    return promise;
  }, [chunkCache, loadChunk]);

  /// Scan until `done`, the end of the file, or a scan that stops making
  /// progress (a chunk that failed to load).
  const scanUntil = useCallback(
    async (done: () => boolean) => {
      const gen = genRef.current;
      while (!done() && !eofRef.current && gen === genRef.current) {
        const before = scannedToRef.current;
        await pump();
        if (scannedToRef.current === before) break;
      }
    },
    [pump],
  );

  const ensureFileRows = useCallback(
    (rows: number) => {
      targetRowsRef.current = Math.max(targetRowsRef.current, rows + 1);
      return scanUntil(() => rowStartsRef.current.length > rows + 1);
    },
    [scanUntil],
  );

  const ensureByte = useCallback(
    (offset: number) => {
      targetByteRef.current = Math.max(targetByteRef.current, offset);
      return scanUntil(() => scannedToRef.current > offset);
    },
    [scanUntil],
  );

  const ensureEof = useCallback(
    () => ensureFileRows(Number.MAX_SAFE_INTEGER - 1),
    [ensureFileRows],
  );

  useEffect(() => {
    genRef.current++;
    scanPromiseRef.current = null;
    rowStartsRef.current = [bomLen];
    scannedToRef.current = bomLen;
    eofRef.current = false;
    targetRowsRef.current = 200;
    targetByteRef.current = 0;
    scannerRef.current = dialect ? new RowScanner(dialect, scan) : null;
    setMaxFields(1);
    setRenderGen((n) => n + 1);
    if (dialect) void pump();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filePath, scanKey]);

  /// File rows whose end is known. The last start is a row only once the
  /// end of the file shows it isn't the empty line after a final newline.
  const fileRowsKnown = useCallback(() => {
    const s = rowStartsRef.current;
    return eofRef.current && s[s.length - 1] < scannedToRef.current
      ? s.length
      : s.length - 1;
  }, []);
  const starts = rowStartsRef.current;
  const knownFileRows = fileRowsKnown();
  const knownRows = Math.max(0, knownFileRows - headerRows);

  const rowEnd = useCallback(
    (fileRow: number) =>
      fileRow + 1 < rowStartsRef.current.length
        ? rowStartsRef.current[fileRow + 1]
        : scannedToRef.current,
    [],
  );

  // --- Layout ---

  const [topRow, setTopRow] = useState(0);
  const [scrollLeft, setScrollLeft] = useState(0);
  const [viewport, setViewport] = useState({ width: 0, height: 0 });

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const update = () =>
      setViewport({ width: el.clientWidth, height: el.clientHeight });
    update();
    const obs = new ResizeObserver(update);
    obs.observe(el);
    return () => obs.disconnect();
  }, []);

  let totalRows = knownRows;
  if (!eofRef.current) {
    const scannedRows = Math.max(1, knownFileRows);
    const avgRowBytes = (scannedToRef.current - bomLen) / scannedRows;
    const estimate =
      fileSize > 0 && knownFileRows > 1
        ? Math.ceil((fileSize - bomLen) / avgRowBytes) - headerRows
        : knownRows + 100;
    totalRows = Math.max(knownRows + 1, estimate);
  }
  const visibleRows = Math.max(
    1,
    Math.floor((viewport.height - ROW_HEIGHT) / ROW_HEIGHT),
  );
  const maxTopRow = Math.max(0, totalRows - visibleRows);
  const naturalHeight = totalRows * ROW_HEIGHT;
  const scrollableHeight = Math.min(naturalHeight, MAX_SCROLL_HEIGHT);
  const scale =
    naturalHeight > MAX_SCROLL_HEIGHT ? naturalHeight / MAX_SCROLL_HEIGHT : 1;
  const clampedTopRow = Math.max(0, Math.min(topRow, maxTopRow));
  if (clampedTopRow !== topRow) setTopRow(clampedTopRow);
  const startRow = Math.max(0, clampedTopRow - 3);
  const endRow = Math.min(totalRows, clampedTopRow + visibleRows + 3);

  useEffect(() => {
    if (!eofRef.current && endRow + headerRows + 50 > knownFileRows) {
      targetRowsRef.current = Math.max(
        targetRowsRef.current,
        endRow + headerRows + 200,
      );
      void pump();
    }
  }, [endRow, headerRows, knownFileRows, pump]);

  const columnCount = Math.max(
    maxFields,
    detected?.sampleRows.reduce((n, r) => Math.max(n, r.length), 1) ?? 1,
  );

  // --- Visible rows ---

  // The chunks holding the visible rows, and the header row's, which may
  // have been evicted since the scan passed them.
  const chunkSpan = (start: number, end: number): [number, number] => [
    Math.floor(start / CHUNK_SIZE),
    Math.floor(Math.max(start, end - 1) / CHUNK_SIZE),
  ];
  const firstVisibleFileRow = Math.min(
    startRow + headerRows,
    Math.max(0, starts.length - 1),
  );
  const lastVisibleFileRow = Math.min(endRow + headerRows, knownFileRows) - 1;
  const [visStartChunk, visEndChunk] = chunkSpan(
    starts[firstVisibleFileRow],
    lastVisibleFileRow >= firstVisibleFileRow
      ? rowEnd(lastVisibleFileRow)
      : starts[firstVisibleFileRow],
  );
  const [headStartChunk, headEndChunk] =
    header && knownFileRows > 0
      ? chunkSpan(starts[0], rowEnd(0))
      : [visStartChunk, visStartChunk];
  useEffect(() => {
    let cancelled = false;
    (async () => {
      let loaded = false;
      const spans = [
        [headStartChunk, headEndChunk],
        [visStartChunk, visEndChunk],
      ];
      for (const [from, to] of spans) {
        for (let ci = from; ci <= to; ci++) {
          if (!chunkCache.current.has(ci)) {
            await loadChunk(ci);
            loaded = true;
          }
        }
      }
      if (loaded && !cancelled) setRenderGen((n) => n + 1);
    })();
    return () => {
      cancelled = true;
    };
  }, [
    headStartChunk,
    headEndChunk,
    visStartChunk,
    visEndChunk,
    loadChunk,
    chunkCache,
  ]);

  const readRow = useCallback(
    (fileRow: number): string[] => {
      if (!dialect) return [];
      const start = rowStartsRef.current[fileRow];
      const bytes = collectBytes(chunkCache.current, start, rowEnd(fileRow));
      return parseRow(makeDecoder(encoding).decode(bytes), dialect);
    },
    [chunkCache, dialect, encoding, rowEnd],
  );

  const hasFirstRow = knownFileRows > 0;
  const headerFields = useMemo(
    () => (header && hasFirstRow ? readRow(0) : null),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [header, hasFirstRow, renderGen, readRow],
  );

  const visibleData = useMemo(() => {
    const rows: string[][] = [];
    for (let r = startRow; r < Math.min(endRow, knownRows); r++) {
      rows.push(readRow(r + headerRows));
    }
    return rows;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [startRow, endRow, knownRows, headerRows, renderGen, readRow]);

  // --- Columns ---

  const measureCtx = useMemo(
    () => document.createElement("canvas").getContext("2d"),
    [],
  );
  const measure = useCallback(
    (texts: string[], label?: string): number => {
      const el = scrollRef.current;
      if (!measureCtx || !el) return DEFAULT_WIDTH;
      const style = getComputedStyle(el);
      measureCtx.font = style.font;
      let width = 0;
      for (const t of texts) {
        width = Math.max(width, measureCtx.measureText(displayText(t)).width);
      }
      if (label !== undefined) {
        measureCtx.font = `600 ${style.fontSize} ${style.fontFamily}`;
        width = Math.max(width, measureCtx.measureText(label).width);
      }
      return Math.ceil(width) + CELL_PADDING;
    },
    [measureCtx],
  );

  const [widths, setWidths] = useState<number[]>([]);

  // Initial widths fit the first chunk's rows, capped so one long value
  // doesn't push everything else off screen.
  useEffect(() => {
    if (!detected) return;
    const rows = detected.sampleRows.slice(0, 200);
    const cols = rows.reduce((n, r) => Math.max(n, r.length), 0);
    const fitted: number[] = [];
    for (let c = 0; c < cols; c++) {
      const label = header ? (rows[0]?.[c] ?? "") : columnName(c);
      const values = rows.slice(header ? 1 : 0).map((r) => r[c] ?? "");
      fitted.push(
        Math.min(300, Math.max(48, measure(values.slice(0, 100), label))),
      );
    }
    setWidths(fitted);
  }, [detected, header, measure]);

  const widthOf = useCallback(
    (c: number) => widths[c] ?? DEFAULT_WIDTH,
    [widths],
  );

  const offsets = useMemo(() => {
    const out = [0];
    for (let c = 0; c < columnCount; c++) out.push(out[c] + widthOf(c));
    return out;
  }, [columnCount, widthOf]);
  const totalWidth = offsets[columnCount];

  const gutterWidth = useMemo(
    () => measure([String(Math.max(totalRows, 1))]) + 8,
    [measure, totalRows],
  );

  const firstCol = Math.min(
    Math.max(0, columnCount - 1),
    floorIndex(offsets, scrollLeft),
  );
  const lastCol = Math.min(
    columnCount - 1,
    floorIndex(offsets, scrollLeft + Math.max(0, viewport.width - gutterWidth)),
  );

  const labelOf = useCallback(
    (c: number) => {
      const name = headerFields?.[c];
      return name ? name : columnName(c);
    },
    [headerFields],
  );

  const setColumnWidth = useCallback((c: number, width: number) => {
    setWidths((prev) => {
      const next = [...prev];
      while (next.length <= c) next.push(DEFAULT_WIDTH);
      next[c] = Math.max(MIN_WIDTH, Math.round(width));
      return next;
    });
  }, []);

  const autoSizeColumn = useCallback(
    (c: number) => {
      const texts = visibleData.map((r) => r[c] ?? "");
      setColumnWidth(c, Math.min(MAX_FIT_WIDTH, measure(texts, labelOf(c))));
    },
    [labelOf, visibleData, setColumnWidth, measure],
  );

  // --- Selection ---

  const [selection, setSelection] = useState<Selection | null>(null);
  const selectionRef = useRef(selection);
  selectionRef.current = selection;
  const [notice, setNotice] = useState<string | null>(null);

  // --- Select mode: one cell's text in a read-only box, for selecting
  // part of it ---

  const [editing, setEditing] = useState<Cell | null>(null);
  const editingRef = useRef(editing);
  editingRef.current = editing;
  const editRef = useRef<HTMLTextAreaElement>(null);

  const leaveSelectMode = useCallback(() => {
    setEditing(null);
    viewerRef.current?.focus();
  }, []);

  useEffect(() => {
    setSelection(null);
    setEditing(null);
    setNotice(null);
    setTopRow(0);
  }, [filePath, scanKey, header]);

  const rect = selection ? selectionRect(selection) : null;

  /// Scroll the cell into view.
  const reveal = useCallback(
    (cell: Cell) => {
      if (cell.row < clampedTopRow) setTopRow(cell.row);
      else if (cell.row >= clampedTopRow + visibleRows)
        setTopRow(cell.row - visibleRows + 1);
      const el = scrollRef.current;
      if (!el) return;
      const x0 = offsets[cell.col] ?? 0;
      const x1 = offsets[cell.col + 1] ?? x0;
      const view = el.clientWidth - gutterWidth;
      if (x0 < el.scrollLeft) el.scrollLeft = x0;
      else if (x1 > el.scrollLeft + view) el.scrollLeft = x1 - view;
    },
    [clampedTopRow, visibleRows, offsets, gutterWidth],
  );

  /// Put the cursor on a cell, clamped to the rows scanned so far — read
  /// live, as async callers move it after extending the scan. No rows, no
  /// cursor.
  const moveTo = useCallback(
    (cell: Cell, extend: boolean) => {
      const rows = fileRowsKnown() - headerRows;
      if (rows <= 0) return;
      const target = {
        row: Math.max(0, Math.min(cell.row, rows - 1)),
        col: Math.max(0, Math.min(cell.col, columnCount - 1)),
      };
      setSelection((prev) =>
        extend && prev
          ? { kind: "cells", anchor: prev.anchor, head: target }
          : { kind: "cells", anchor: target, head: target },
      );
      setNotice(null);
      reveal(target);
    },
    [fileRowsKnown, headerRows, columnCount, reveal],
  );

  // The cursor starts on A1, once there is a row to put it on.
  const cursorPlacedRef = useRef(false);
  useEffect(() => {
    cursorPlacedRef.current = false;
  }, [filePath, scanKey, header]);
  useEffect(() => {
    if (cursorPlacedRef.current || knownRows === 0) return;
    cursorPlacedRef.current = true;
    const a1 = { row: 0, col: 0 };
    setSelection((prev) => prev ?? { kind: "cells", anchor: a1, head: a1 });
  }, [knownRows, filePath, scanKey, header]);

  // --- Mouse ---

  const dragRef = useRef<SelectionKind | null>(null);
  const lastMouseRef = useRef({ x: 0, y: 0 });
  const autoScrollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const cellFromPoint = useCallback(
    (x: number, y: number): Partial<Cell> | null => {
      const el = scrollRef.current;
      if (!el) return null;
      const r = el.getBoundingClientRect();
      const header = headerRef.current?.offsetHeight ?? ROW_HEIGHT;
      const px = Math.min(Math.max(x, r.left + gutterWidth + 1), r.right - 2);
      const py = Math.min(Math.max(y, r.top + header + 1), r.bottom - 2);
      const hit = document
        .elementFromPoint(px, py)
        ?.closest<HTMLElement>("[data-row],[data-col]");
      if (!hit) return null;
      const { row, col } = hit.dataset;
      return {
        row: row === undefined ? undefined : Number(row),
        col: col === undefined ? undefined : Number(col),
      };
    },
    [gutterWidth],
  );

  const startDrag = (e: React.MouseEvent, kind: SelectionKind, cell: Cell) => {
    if (e.button !== 0) return;
    e.preventDefault();
    setEditing(null);
    viewerRef.current?.focus();
    setNotice(null);
    setSelection((prev) =>
      e.shiftKey && prev && prev.kind === kind
        ? { ...prev, head: cell }
        : { kind, anchor: cell, head: cell },
    );
    dragRef.current = kind;
  };

  useEffect(() => {
    const update = (x: number, y: number) => {
      const kind = dragRef.current;
      const hit = cellFromPoint(x, y);
      if (!kind || !hit) return;
      setSelection((prev) =>
        prev
          ? {
              ...prev,
              head: {
                row: hit.row ?? prev.head.row,
                col: hit.col ?? prev.head.col,
              },
            }
          : prev,
      );
    };
    const onMove = (e: MouseEvent) => {
      if (!dragRef.current) return;
      lastMouseRef.current = { x: e.clientX, y: e.clientY };
      update(e.clientX, e.clientY);
      const el = scrollRef.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      const outside =
        e.clientY < r.top + ROW_HEIGHT ||
        e.clientY > r.bottom ||
        e.clientX < r.left + gutterWidth ||
        e.clientX > r.right;
      if (outside && !autoScrollRef.current) {
        autoScrollRef.current = setInterval(() => {
          const m = lastMouseRef.current;
          const b = el.getBoundingClientRect();
          if (m.y < b.top + ROW_HEIGHT) el.scrollTop -= ROW_HEIGHT;
          else if (m.y > b.bottom) el.scrollTop += ROW_HEIGHT;
          if (m.x < b.left + gutterWidth) el.scrollLeft -= DEFAULT_WIDTH / 2;
          else if (m.x > b.right) el.scrollLeft += DEFAULT_WIDTH / 2;
          update(m.x, m.y);
        }, 50);
      } else if (!outside && autoScrollRef.current) {
        clearInterval(autoScrollRef.current);
        autoScrollRef.current = null;
      }
    };
    const onUp = () => {
      dragRef.current = null;
      if (autoScrollRef.current) {
        clearInterval(autoScrollRef.current);
        autoScrollRef.current = null;
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      if (autoScrollRef.current) clearInterval(autoScrollRef.current);
    };
  }, [cellFromPoint, gutterWidth]);

  const resizeRef = useRef<{ col: number; x: number; width: number } | null>(
    null,
  );
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      const r = resizeRef.current;
      if (r) setColumnWidth(r.col, r.width + e.clientX - r.x);
    };
    const onUp = () => {
      resizeRef.current = null;
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [setColumnWidth]);

  // --- Scrolling ---

  const handleScroll = useCallback(
    (e: React.UIEvent<HTMLDivElement>) => {
      const el = e.currentTarget;
      setTopRow(Math.floor((el.scrollTop * scale) / ROW_HEIGHT));
      setScrollLeft(el.scrollLeft);
    },
    [scale],
  );

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const target = (clampedTopRow * ROW_HEIGHT) / scale;
    if (Math.abs(el.scrollTop - target) >= ROW_HEIGHT / scale) {
      el.scrollTop = target;
    }
  }, [clampedTopRow, scale]);

  // --- Commands ---

  /// Bytes [start, end), from the chunk cache where it has them. Copied
  /// out rather than read back from the cache, which a large range would
  /// evict from as it went.
  const readRange = useCallback(
    async (start: number, end: number): Promise<Uint8Array> => {
      const out = new Uint8Array(Math.max(0, end - start));
      for (
        let ci = Math.floor(start / CHUNK_SIZE);
        ci * CHUNK_SIZE < end;
        ci++
      ) {
        const chunkStart = ci * CHUNK_SIZE;
        const chunk =
          chunkCache.current.get(ci) ??
          new Uint8Array(
            (
              (await unwrap(
                commands.readFileRange(vfsPath, chunkStart, CHUNK_SIZE),
              )) as FileChunk
            ).data,
          );
        const from = Math.max(start, chunkStart);
        const to = Math.min(end, chunkStart + chunk.length);
        if (to <= from) break;
        out.set(
          chunk.subarray(from - chunkStart, to - chunkStart),
          from - start,
        );
      }
      return out;
    },
    [chunkCache, vfsPath],
  );

  const copy = useCallback(
    async (separator: "\t" | ",") => {
      const sel = selectionRef.current;
      if (!sel || !dialect) return;
      const r = selectionRect(sel);
      if (r.r1 === Infinity) await ensureEof();
      else await ensureFileRows(r.r1 + headerRows + 1);
      const starts = rowStartsRef.current;
      const lastRow = fileRowsKnown() - headerRows - 1;
      const r1 = Math.min(r.r1, lastRow);
      if (r1 < r.r0) return;
      const start = starts[r.r0 + headerRows];
      const end = rowEnd(r1 + headerRows);
      if (end - start > MAX_COPY_BYTES) {
        setNotice(
          `Selection too large to copy (${formatSize(end - start)}, max ${formatSize(MAX_COPY_BYTES)})`,
        );
        return;
      }
      const c1 = Math.min(r.c1, columnCount - 1);
      const pick = (fields: string[]) => {
        const out: string[] = [];
        for (let c = r.c0; c <= c1; c++) out.push(fields[c] ?? "");
        return out;
      };
      try {
        const text = makeDecoder(encoding).decode(await readRange(start, end));
        const rows = parseRows(text, dialect)
          .slice(0, r1 - r.r0 + 1)
          .map(pick);
        const wholeColumns = sel.kind === "columns" || sel.kind === "all";
        if (wholeColumns && headerFields) rows.unshift(pick(headerFields));
        await navigator.clipboard.writeText(formatRows(rows, separator));
        setNotice(null);
      } catch (e) {
        setNotice(`Copy failed: ${e}`);
      }
    },
    [
      dialect,
      ensureEof,
      ensureFileRows,
      fileRowsKnown,
      headerRows,
      rowEnd,
      formatSize,
      columnCount,
      encoding,
      readRange,
      headerFields,
    ],
  );

  const selectAll = useCallback(() => {
    setSelection({
      kind: "all",
      anchor: { row: 0, col: 0 },
      head: { row: 0, col: 0 },
    });
  }, []);

  const [searchOpen, setSearchOpen] = useState(false);
  const [goToOpen, setGoToOpen] = useState(false);

  const handleSearchMatch = useCallback(
    async (match: SearchMatch) => {
      if (!dialect) return;
      await ensureByte(match.offset);
      const fileRow = floorIndex(rowStartsRef.current, match.offset);
      const start = rowStartsRef.current[fileRow];
      const end = rowEnd(fileRow);
      for (
        let ci = Math.floor(start / CHUNK_SIZE);
        ci <= Math.floor(Math.max(start, end - 1) / CHUNK_SIZE);
        ci++
      ) {
        if (!chunkCache.current.has(ci)) await loadChunk(ci);
      }
      const col = fieldAtOffset(
        collectBytes(chunkCache.current, start, end),
        match.offset - start,
        dialect,
        scan,
      );
      const row = fileRow - headerRows;
      if (row < 0) {
        setSelection({
          kind: "columns",
          anchor: { row: 0, col },
          head: { row: 0, col },
        });
        reveal({ row: clampedTopRow, col });
      } else {
        moveTo({ row, col }, false);
      }
    },
    [
      dialect,
      ensureByte,
      rowEnd,
      chunkCache,
      loadChunk,
      scan,
      headerRows,
      reveal,
      clampedTopRow,
      moveTo,
    ],
  );

  const handleGoTo = useCallback(
    async (value: string) => {
      const n = parseInt(value, 10);
      if (isNaN(n) || n < 1) return;
      await ensureFileRows(n - 1 + headerRows + 1);
      const lastRow = Math.max(0, fileRowsKnown() - headerRows - 1);
      moveTo(
        {
          row: Math.min(n - 1, lastRow),
          col: selectionRef.current?.head.col ?? 0,
        },
        false,
      );
    },
    [ensureFileRows, fileRowsKnown, headerRows, moveTo],
  );

  /// Copy in select mode: the selected text, or the whole value.
  const copyEditText = useCallback(() => {
    const box = editRef.current;
    if (!box) return;
    const text =
      box.selectionStart !== box.selectionEnd
        ? box.value.slice(box.selectionStart, box.selectionEnd)
        : box.value;
    navigator.clipboard
      .writeText(text)
      .catch((e) => setNotice(`Copy failed: ${e}`));
  }, []);
  const copyCommand = useCallback(() => {
    if (editingRef.current) copyEditText();
    else void copy("\t");
  }, [copy, copyEditText]);
  const selectAllCommand = useCallback(() => {
    if (editingRef.current) editRef.current?.select();
    else selectAll();
  }, [selectAll]);
  // The cursor alone is always a one-cell selection; it takes more than
  // that (or the cell-text box) to count.
  useEmbeddedCopy(() => {
    const sel = selectionRef.current;
    const beyondCursor =
      !!sel &&
      (sel.kind !== "cells" ||
        sel.anchor.row !== sel.head.row ||
        sel.anchor.col !== sel.head.col);
    if (!editingRef.current && !beyondCursor) return false;
    copyCommand();
    return true;
  });
  const copyCommandRef = useRef(copyCommand);
  copyCommandRef.current = copyCommand;
  const selectAllCommandRef = useRef(selectAllCommand);
  selectAllCommandRef.current = selectAllCommand;

  useEffect(() => {
    const unlisten = getCurrentWebviewWindow().listen<string>(
      "viewer-menu",
      (event) => {
        switch (event.payload) {
          case "copy":
            copyCommandRef.current();
            break;
          case "select_all":
            selectAllCommandRef.current();
            break;
          case "goto":
            setGoToOpen(true);
            break;
        }
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  useScopedBindings(viewerHost.embedded ? null : "viewer", {
    viewer_copy: copyCommand,
    viewer_select_all: selectAllCommand,
    viewer_goto: () => setGoToOpen(true),
    viewer_find: () => setSearchOpen(true),
  });

  useEffect(() => {
    if (!viewerHost.embedded) viewerRef.current?.focus();
  }, [viewerHost.embedded]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (
        e.target instanceof HTMLInputElement ||
        e.target instanceof HTMLTextAreaElement
      )
        return;
      const sel = selectionRef.current;
      const head = sel?.head ?? { row: clampedTopRow, col: 0 };
      const mod = e.metaKey || e.ctrlKey;
      const lastRow = Math.max(0, knownRows - 1);
      const lastColumn = columnCount - 1;
      const go = (cell: Cell) => {
        e.preventDefault();
        moveTo(cell, e.shiftKey);
      };
      const goToEnd = (col: number) => {
        e.preventDefault();
        void ensureEof().then(() => {
          const rows = fileRowsKnown() - headerRows;
          moveTo({ row: Math.max(0, rows - 1), col }, e.shiftKey);
        });
      };
      switch (e.key) {
        case "ArrowUp":
          return go({ row: mod ? 0 : head.row - 1, col: head.col });
        case "ArrowDown":
          return mod
            ? goToEnd(head.col)
            : go({ row: Math.min(lastRow, head.row + 1), col: head.col });
        case "ArrowLeft":
          return go({ row: head.row, col: mod ? 0 : head.col - 1 });
        case "ArrowRight":
          return go({ row: head.row, col: mod ? lastColumn : head.col + 1 });
        case "PageUp":
          return go({ row: head.row - visibleRows, col: head.col });
        case "PageDown":
          return go({
            row: Math.min(lastRow, head.row + visibleRows),
            col: head.col,
          });
        case "Home":
          return mod ? go({ row: 0, col: 0 }) : go({ row: head.row, col: 0 });
        case "End":
          return mod
            ? goToEnd(lastColumn)
            : go({ row: head.row, col: lastColumn });
        case "Tab":
          // Shift reverses the direction here rather than extending.
          e.preventDefault();
          moveTo(
            { row: head.row, col: head.col + (e.shiftKey ? -1 : 1) },
            false,
          );
          return;
        case "Enter":
        case "F2":
          if (sel && !mod && !e.altKey && sel.head.row < knownRows) {
            e.preventDefault();
            reveal(sel.head);
            setEditing(sel.head);
          }
          return;
        case "Escape":
          if (
            sel &&
            (sel.kind !== "cells" ||
              sel.anchor.row !== sel.head.row ||
              sel.anchor.col !== sel.head.col)
          ) {
            setSelection({ kind: "cells", anchor: head, head });
            e.preventDefault();
            e.stopPropagation();
          }
          return;
      }
    },
    [
      clampedTopRow,
      knownRows,
      columnCount,
      moveTo,
      ensureEof,
      fileRowsKnown,
      headerRows,
      visibleRows,
      reveal,
    ],
  );

  /// The box grows over its neighbours to fit the value, up to a limit,
  /// then scrolls.
  /// Width to fit the longest line, within bounds; the height follows
  /// the wrapped text (see the layout effect below).
  const selectBoxStyle = (value: string, cellWidth: number) => {
    // `measure` pads for a cell; the box's own chrome replaces that.
    const textWidth = measure(value.split(/\r?\n/)) - CELL_PADDING + 1;
    const chromeX = 2 * (BOX_BORDER + BOX_PADDING_X);
    return {
      borderWidth: BOX_BORDER,
      padding: `0 ${BOX_PADDING_X}px`,
      lineHeight: `${BOX_LINE_HEIGHT}px`,
      width: Math.min(
        Math.max(BOX_MAX_WIDTH, cellWidth),
        Math.max(cellWidth, textWidth + chromeX),
      ),
    };
  };

  const editingValue =
    editing && editing.row >= startRow && editing.row < endRow
      ? (visibleData[editing.row - startRow]?.[editing.col] ?? null)
      : null;

  useEffect(() => {
    if (editing && editingValue === null) leaveSelectMode();
  }, [editing, editingValue, leaveSelectMode]);

  // The box is an editable field that refuses every edit, not a readOnly
  // one: WebKit gives a read-only field no caret, and its arrow keys would
  // scroll the grid instead. React also puts the value back after any
  // input it couldn't stop (IME composition).
  useEffect(() => {
    const box = editRef.current;
    if (!editing || !box) return;
    const refuse = (e: Event) => e.preventDefault();
    box.addEventListener("beforeinput", refuse);
    box.focus();
    box.select();
    return () => box.removeEventListener("beforeinput", refuse);
  }, [editing]);

  // Grow the box to its wrapped text before it paints, up to the line cap.
  useLayoutEffect(() => {
    const box = editRef.current;
    if (!box) return;
    box.style.height = "0";
    const content = Math.min(box.scrollHeight, BOX_MAX_LINES * BOX_LINE_HEIGHT);
    box.style.height = `${Math.max(content, BOX_LINE_HEIGHT) + 2 * BOX_BORDER}px`;
  }, [editing, editingValue, widths]);

  // --- Render ---

  const cols: number[] = [];
  for (let c = firstCol; c <= lastCol; c++) cols.push(c);
  const leftSpacer = offsets[firstCol] ?? 0;
  const rowWidth = gutterWidth + totalWidth;
  const topSpacer = (startRow * ROW_HEIGHT) / scale;
  const head = selection?.head;

  const selectedColumns =
    rect && rect.r0 === 0 && rect.r1 === Infinity ? rect : null;
  const selectedRows = rect && rect.c1 === Infinity ? rect : null;

  // Focus stays on the viewer, not the cell; aria-activedescendant tells
  // assistive tech which cell the cursor is on.
  const cellId = (row: number, col: number) => `${gridId}-${row}-${col}`;
  const activeCell =
    head &&
    !editing &&
    head.row >= startRow &&
    head.row - startRow < visibleData.length &&
    head.col >= firstCol &&
    head.col <= lastCol
      ? cellId(head.row, head.col)
      : undefined;

  const status = (() => {
    const cursor =
      head && knownRows > 0
        ? `Row ${head.row + 1} / ${knownRows}${eofRef.current ? "" : "+"} | Col ${columnName(head.col)}`
        : `${knownRows}${eofRef.current ? "" : "+"} rows × ${columnCount} cols`;
    if (editing) {
      return `${cursor} | Selecting text in ${columnName(editing.col)}${editing.row + 1} (Esc to leave)`;
    }
    if (!rect) return cursor;
    const rows = rect.r1 === Infinity ? "all" : String(rect.r1 - rect.r0 + 1);
    const colsCount =
      rect.c1 === Infinity ? "all" : String(rect.c1 - rect.c0 + 1);
    return rows === "1" && colsCount === "1"
      ? cursor
      : `${cursor} | Sel: ${rows} × ${colsCount}`;
  })();

  return (
    <div
      className={styles.viewer}
      ref={viewerRef}
      tabIndex={-1}
      role="group"
      aria-activedescendant={activeCell}
      onKeyDown={handleKeyDown}
    >
      <CM.Root>
        <CM.Trigger asChild>
          <div
            className={`${styles.viewerContent} ${tableStyles.grid}`}
            ref={scrollRef}
            onScroll={handleScroll}
          >
            <div
              role="grid"
              aria-label={filePath}
              aria-multiselectable
              aria-rowcount={eofRef.current ? knownRows + 1 : -1}
              aria-colcount={columnCount + 1}
              aria-busy={!detected}
              style={{
                position: "relative",
                height: ROW_HEIGHT + scrollableHeight,
                width: rowWidth,
              }}
            >
              <div
                className={tableStyles.headerRow}
                ref={headerRef}
                role="row"
                aria-rowindex={1}
                style={{ width: rowWidth, height: ROW_HEIGHT }}
              >
                <div
                  className={tableStyles.corner}
                  role="columnheader"
                  aria-colindex={1}
                  title={shortcuts.label("Select all", "viewer_select_all")}
                  style={{ width: gutterWidth }}
                  onMouseDown={(e) => {
                    if (e.button !== 0) return;
                    e.preventDefault();
                    setEditing(null);
                    viewerRef.current?.focus();
                    selectAll();
                  }}
                />
                <div aria-hidden style={{ width: leftSpacer, flexShrink: 0 }} />
                {cols.map((c) => (
                  <div
                    key={c}
                    data-col={c}
                    role="columnheader"
                    aria-colindex={c + 2}
                    title={labelOf(c)}
                    className={`${tableStyles.headerCell} ${
                      c >= (rect?.c0 ?? Infinity) && c <= (rect?.c1 ?? -1)
                        ? selectedColumns
                          ? tableStyles.headerSelected
                          : tableStyles.headerInRange
                        : ""
                    }`}
                    style={{ width: widthOf(c) }}
                    onMouseDown={(e) =>
                      startDrag(e, "columns", { row: 0, col: c })
                    }
                  >
                    <span className={tableStyles.cellText}>{labelOf(c)}</span>
                    <div
                      className={tableStyles.grip}
                      aria-hidden
                      onMouseDown={(e) => {
                        if (e.button !== 0) return;
                        e.preventDefault();
                        e.stopPropagation();
                        resizeRef.current = {
                          col: c,
                          x: e.clientX,
                          width: widthOf(c),
                        };
                      }}
                      onDoubleClick={(e) => {
                        e.stopPropagation();
                        autoSizeColumn(c);
                      }}
                    />
                  </div>
                ))}
              </div>
              <div aria-hidden style={{ height: topSpacer }} />
              {visibleData.map((fields, i) => {
                const row = startRow + i;
                return (
                  <div
                    key={row}
                    className={tableStyles.row}
                    role="row"
                    aria-rowindex={row + 2}
                    style={{ width: rowWidth, height: ROW_HEIGHT }}
                  >
                    <div
                      data-row={row}
                      role="rowheader"
                      aria-colindex={1}
                      className={`${tableStyles.gutterCell} ${
                        rect && row >= rect.r0 && row <= rect.r1
                          ? selectedRows
                            ? tableStyles.headerSelected
                            : tableStyles.headerInRange
                          : ""
                      }`}
                      style={{ width: gutterWidth }}
                      onMouseDown={(e) => startDrag(e, "rows", { row, col: 0 })}
                    >
                      {row + 1}
                    </div>
                    <div
                      aria-hidden
                      style={{ width: leftSpacer, flexShrink: 0 }}
                    />
                    {cols.map((c) => {
                      const value = fields[c] ?? "";
                      const cursor = head?.row === row && head?.col === c;
                      return (
                        <div
                          key={c}
                          id={cellId(row, c)}
                          data-row={row}
                          data-col={c}
                          role="gridcell"
                          aria-colindex={c + 2}
                          aria-selected={inRect(rect, row, c)}
                          className={`${tableStyles.cell} ${
                            isNumeric(value) ? tableStyles.numeric : ""
                          } ${inRect(rect, row, c) ? tableStyles.selected : ""} ${
                            cursor ? tableStyles.cursor : ""
                          }`}
                          style={{ width: widthOf(c) }}
                          onMouseDown={(e) =>
                            startDrag(e, "cells", { row, col: c })
                          }
                          onDoubleClick={() => setEditing({ row, col: c })}
                        >
                          <span className={tableStyles.cellText}>
                            {displayText(value)}
                          </span>
                        </div>
                      );
                    })}
                  </div>
                );
              })}
              {editing && editingValue !== null && (
                <textarea
                  ref={editRef}
                  className={tableStyles.selectBox}
                  aria-readonly
                  aria-label={`Cell ${columnName(editing.col)}${editing.row + 1} text`}
                  wrap="soft"
                  spellCheck={false}
                  autoCorrect="off"
                  autoCapitalize="off"
                  value={editingValue}
                  onChange={() => {}}
                  style={{
                    left: gutterWidth + (offsets[editing.col] ?? 0),
                    top:
                      ROW_HEIGHT +
                      topSpacer +
                      (editing.row - startRow) * ROW_HEIGHT,
                    ...selectBoxStyle(editingValue, widthOf(editing.col)),
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Escape" || e.key === "Enter") {
                      e.preventDefault();
                      e.stopPropagation();
                      leaveSelectMode();
                    } else if (e.key === "Home" || e.key === "End") {
                      // Start and end of the value, where macOS would
                      // otherwise scroll — the grid, as the box rarely can.
                      e.preventDefault();
                      e.stopPropagation();
                      moveCaret(e.currentTarget, e.key === "Home", e.shiftKey);
                    } else if (e.key === "Tab") {
                      e.preventDefault();
                      e.stopPropagation();
                      leaveSelectMode();
                      moveTo(
                        {
                          row: editing.row,
                          col: editing.col + (e.shiftKey ? -1 : 1),
                        },
                        false,
                      );
                    }
                  }}
                />
              )}
            </div>
          </div>
        </CM.Trigger>
        <CM.Portal>
          <CM.Content
            className={menuStyles.content}
            loop
            onCloseAutoFocus={(e) => {
              e.preventDefault();
              (editRef.current ?? viewerRef.current)?.focus();
            }}
          >
            <CM.Item
              className={menuStyles.item}
              disabled={!selection && !editing}
              onSelect={copyCommand}
            >
              Copy
            </CM.Item>
            <CM.Item
              className={menuStyles.item}
              disabled={!selection}
              onSelect={() => void copy(",")}
            >
              Copy as CSV
            </CM.Item>
            <CM.Item className={menuStyles.item} onSelect={selectAll}>
              Select All
            </CM.Item>
            <CM.Separator className={menuStyles.separator} />
            <CM.Item
              className={menuStyles.item}
              onSelect={() => setGoToOpen(true)}
            >
              Go to Row...
            </CM.Item>
          </CM.Content>
        </CM.Portal>
      </CM.Root>
      <SearchBar
        open={searchOpen}
        onClose={() => {
          setSearchOpen(false);
          viewerRef.current?.focus();
        }}
        vfsPath={vfsPath}
        fileSize={fileSize}
        mode="text"
        encoding={encoding}
        onMatch={(m) => void handleSearchMatch(m)}
        onNoMatch={() => {}}
      />
      <GoToBar
        open={goToOpen}
        onClose={() => {
          setGoToOpen(false);
          viewerRef.current?.focus();
        }}
        label="Go to row"
        placeholder="1"
        onSubmit={(v) => void handleGoTo(v)}
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
          <span>Table</span>
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
          <span>{status}</span>
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
        <ModeToggle currentMode="table" autoMode={autoMode} />
      </div>
    </div>
  );
}
