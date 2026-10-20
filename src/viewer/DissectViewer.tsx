import React, {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { message, save as saveDialog } from "@tauri-apps/plugin-dialog";

import {
  commands,
  type CopyFormat,
  type DissectChildrenAdded,
  type DissectChoice,
  type DissectContent,
  type DissectDiagnostic,
  type DissectNode,
  type DissectPage,
  type DissectProgress,
  type DissectSecret,
  type DissectSource,
  type DissectSpan,
  type DissectTask,
  type DissectTaskProgress,
} from "../lib/bindings";
import { safe, safeSilent, unwrap, unwrapBytes } from "../lib/ipc";
import { ContextMenu as CM, keyboardMenuHandlers } from "../lib/menus";
import { useCommandShortcuts, useScopedBindings } from "../lib/scopedBindings";
import { useFormatBytes } from "../lib/size";
import menuStyles from "../main_window/Menu.module.scss";
import { GoToBar } from "./GoToDialog";
import {
  CHUNK_SIZE,
  HEX_BYTES_PER_ROW,
  LruChunkCache,
  MAX_SCROLL_HEIGHT,
  formatHexOffset,
  hexByte,
  printableAscii,
  type VfsPath,
  type ViewerMode,
} from "./helpers";
import { DissectAs, type DissectAsTarget } from "./DissectAs";
import { ValueInline, ValueRows, copyableValue } from "./DissectValue";
import { useEmbeddedCopy, useViewerHost } from "./host";
import { IconChevronRight } from "./icons";
import { useAfterGrace } from "../lib/useAfterGrace";
import { useWheelScroll } from "../lib/useWheelScroll";
import { ModeToggle } from "./ModeToggle";
import styles from "./DissectViewer.module.scss";
import viewerStyles from "./Viewer.module.scss";

/// Children asked for at a time.
const PAGE = 200;
const TREE_ROW = 22;
const HEX_ROW = 18;
const OVERSCAN = 6;

export interface DissectViewerProps {
  filePath: string;
  vfsPath: VfsPath;
  /// Formats chosen for the file or for files within it, applied when it
  /// is dissected.
  choices: DissectChoice[];
  fileSize: number;
  autoMode: ViewerMode;
}

/// A line of the tree: a node, or the "more children" line under one.
type Row =
  | { kind: "node"; key: number; depth: number }
  | { kind: "more"; parent: number; depth: number };

/// Tree selection: a node's key, or `-(parent + 1)` for a "more" line.
const moreId = (parent: number) => -(parent + 1);

type Zone = "tree" | "hex";

type Range = { anchor: number; head: number };

const hex = (n: number) => `0x${n.toString(16).toUpperCase()}`;

/// Whether a request failed only because it was stopped.
const wasStopped = (e: unknown) =>
  e instanceof Error && e.message === "operation cancelled";

function spanText(span: DissectSpan): string {
  return `${hex(span.offset)} + ${hex(span.len)}`;
}

/// The field as one line: `Name: value — summary`.
function nodeLine(node: DissectNode): string {
  let line = node.name;
  if (node.value) line += `: ${node.value.text}`;
  if (node.summary) line += ` — ${node.summary}`;
  return line;
}

/// Whether byte `offset` of `source` lies in `span`.
function inSpan(span: DissectSpan | null, source: number, offset: number) {
  return (
    !!span &&
    span.source === source &&
    offset >= span.offset &&
    offset < span.offset + span.len
  );
}

const DIAG_LABELS: Record<DissectDiagnostic["kind"], string> = {
  truncated: "Truncated",
  malformed: "Malformed",
  unsupported: "Unsupported",
  limit: "Limit",
  warning: "Warning",
  note: "Note",
};

export function DissectViewer({
  filePath,
  vfsPath,
  choices,
  fileSize,
  autoMode,
}: DissectViewerProps) {
  const host = useViewerHost();
  const shortcuts = useCommandShortcuts();
  const formatSize = useFormatBytes();
  const fileKey = JSON.stringify(vfsPath);
  // Read when the file is dissected; a choice made since is in effect
  // already.
  const choicesRef = useRef(choices);
  choicesRef.current = choices;

  // --- The tree ---

  const [session, setSession] = useState<number | null>(null);
  const [rootKey, setRootKey] = useState<number | null>(null);
  const [nodes, setNodes] = useState<Map<number, DissectNode>>(new Map());
  const [children, setChildren] = useState<Map<number, number[]>>(new Map());
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const [selected, setSelected] = useState<number | null>(null);
  // Nodes whose children are being produced, with how many requests are
  // producing them, and how far each has got.
  const [loading, setLoading] = useState<Map<number, number>>(new Map());
  const loadCounts = useRef(new Map<number, number>());
  const [progress, setProgress] = useState<Map<number, DissectProgress>>(
    new Map(),
  );
  // Requests not about one node: finding a field, Dissect As's target.
  const [working, setWorking] = useState(0);
  // Byte reads of decoded streams in flight.
  const [reading, setReading] = useState(0);
  // Saves of a span to a file in flight.
  const [saving, setSaving] = useState(0);
  // How far the latest decoding read and save have got.
  const [tasks, setTasks] = useState<
    Partial<Record<DissectTask, DissectTaskProgress>>
  >({});
  const [error, setError] = useState<string | null>(null);
  const [zone, setZone] = useState<Zone>("tree");
  const [bytesOpen, setBytesOpen] = useState(true);
  // The node Dissect As is choosing the format of.
  const [asTarget, setAsTarget] = useState<
    (DissectAsTarget & { key: number }) | null
  >(null);
  // Why a command did nothing, until the selection moves.
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => setNotice(null), [selected]);
  // A password the dissection waits for, and what to ask again once it's
  // answered.
  const [secret, setSecret] = useState<{
    request: DissectSecret;
    retry: () => void;
  } | null>(null);
  const loadChildrenRef = useRef<(key: number, atLeast: number) => void>(
    () => {},
  );

  const merge = useCallback((page: DissectPage) => {
    setNodes((prev) => {
      const next = new Map(prev);
      next.set(page.parent.key, page.parent);
      for (const n of page.nodes) next.set(n.key, n);
      return next;
    });
    setChildren((prev) =>
      new Map(prev).set(
        page.parent.key,
        page.nodes.map((n) => n.key),
      ),
    );
  }, []);

  useEffect(() => {
    setSession(null);
    setRootKey(null);
    setNodes(new Map());
    setChildren(new Map());
    setExpanded(new Set());
    setSelected(null);
    setError(null);
    setSecret(null);
    setAsTarget(null);
    loadCounts.current = new Map();
    setLoading(new Map());
    setProgress(new Map());
    setTasks({});
    let cancelled = false;
    let opened: number | null = null;
    (async () => {
      try {
        const r = await unwrap(
          commands.dissectOpen(vfsPath, choicesRef.current),
        );
        if (cancelled) {
          safeSilent(commands.dissectClose(r.id));
          return;
        }
        opened = r.id;
        const root = r.page.parent;
        setNodes(new Map([[root.key, root]]));
        setSession(r.id);
        setRootKey(root.key);
        setExpanded(new Set([root.key]));
        setSelected(root.key);
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
      if (opened !== null) safeSilent(commands.dissectClose(opened));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fileKey]);

  const parentOf = useMemo(() => {
    const map = new Map<number, number>();
    for (const [parent, kids] of children) {
      for (const k of kids) map.set(k, parent);
    }
    return map;
  }, [children]);

  // The filter's text while its box is open; an invalid pattern filters
  // nothing, as in a pane.
  const [filter, setFilter] = useState<string | null>(null);
  const filterRef = useRef<HTMLInputElement>(null);
  const treeId = useId();
  const pattern = useMemo(() => {
    if (!filter) return null;
    try {
      return new RegExp(filter, "i");
    } catch {
      return null;
    }
  }, [filter]);

  const { rows, matches } = useMemo(() => {
    let matches = 0;
    if (rootKey === null) return { rows: [], matches };
    // The rows of `key`'s subtree, as far as it is expanded: under a
    // filter, the nodes that match and the way down to them. Collapsed
    // nodes are matched by their own line only.
    const walk = (key: number, depth: number): Row[] => {
      const node = nodes.get(key);
      const own = !pattern || (!!node && pattern.test(nodeLine(node)));
      if (pattern && own) matches++;
      const below: Row[] = [];
      if (expanded.has(key)) {
        for (const k of children.get(key) ?? []) {
          below.push(...walk(k, depth + 1));
        }
        const state = node?.children.state;
        if (
          (state === "more" || state === "stopped") &&
          (own || below.length > 0)
        ) {
          below.push({ kind: "more", parent: key, depth: depth + 1 });
        }
      }
      if (!own && below.length === 0 && depth > 0) return [];
      return [{ kind: "node", key, depth }, ...below];
    };
    return { rows: walk(rootKey, 0), matches };
  }, [rootKey, expanded, children, nodes, pattern]);

  const rowId = (row: Row) =>
    row.kind === "node" ? row.key : moreId(row.parent);
  const selectedIndex = rows.findIndex((r) => rowId(r) === selected);

  // A pattern the selected line doesn't match (shown only on the way down
  // to one that does, or hidden) selects the first match.
  useEffect(() => {
    if (!pattern) return;
    const matching = (r: Row | undefined) => {
      const n = r?.kind === "node" ? nodes.get(r.key) : undefined;
      return !!n && pattern.test(nodeLine(n));
    };
    if (matching(rows[selectedIndex])) return;
    const first = rows.find(matching);
    if (first) setSelected(rowId(first));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pattern]);
  const node = selected !== null ? (nodes.get(selected) ?? null) : null;

  // Progress also arrives for nodes driven on the way to another (finding
  // a field), so a load starts from none.
  const forgetProgress = useCallback(
    (key: number) =>
      setProgress((prev) => {
        const next = new Map(prev);
        next.delete(key);
        return next;
      }),
    [],
  );
  const startLoading = useCallback(
    (key: number) => {
      const counts = loadCounts.current;
      const running = counts.get(key) ?? 0;
      if (running === 0) forgetProgress(key);
      counts.set(key, running + 1);
      setLoading(new Map(counts));
    },
    [forgetProgress],
  );
  const endLoading = useCallback(
    (key: number) => {
      const counts = loadCounts.current;
      const left = (counts.get(key) ?? 1) - 1;
      if (left > 0) {
        counts.set(key, left);
      } else {
        counts.delete(key);
        forgetProgress(key);
      }
      setLoading(new Map(counts));
    },
    [forgetProgress],
  );

  const loadChildren = useCallback(
    async (key: number, atLeast: number) => {
      if (session === null) return;
      startLoading(key);
      try {
        const page = await unwrap(
          commands.dissectChildren(session, key, atLeast),
        );
        merge(page);
        if (page.secret) {
          setSecret({
            request: page.secret,
            retry: () => loadChildrenRef.current(key, atLeast),
          });
        }
      } catch (e) {
        setError(String(e));
      } finally {
        endLoading(key);
      }
    },
    [session, merge, startLoading, endLoading],
  );
  loadChildrenRef.current = loadChildren;

  // The root's children, once the dissection is open.
  useEffect(() => {
    if (session !== null && rootKey !== null) void loadChildren(rootKey, PAGE);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session]);

  useEffect(() => {
    if (session === null) return;
    const unlisten = getCurrentWebviewWindow().listen<DissectProgress>(
      "dissect-progress",
      ({ payload }) => {
        if (payload.id !== session) return;
        setProgress((prev) => new Map(prev).set(payload.node, payload));
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, [session]);

  // Children of a loading node, as its expansion produces them. The page
  // its request returns supersedes them, so one that arrives once the load
  // is over is dropped, and the parent itself waits for that page. Nodes
  // already known are kept: the viewer may have expanded one meanwhile.
  useEffect(() => {
    if (session === null) return;
    const unlisten = getCurrentWebviewWindow().listen<DissectChildrenAdded>(
      "dissect-children",
      ({ payload }) => {
        if (payload.id !== session || !loadCounts.current.has(payload.node))
          return;
        setNodes((prev) => {
          const next = new Map(prev);
          for (const n of payload.nodes)
            if (!next.has(n.key)) next.set(n.key, n);
          return next;
        });
        setChildren((prev) => {
          const keys = [...(prev.get(payload.node) ?? [])];
          keys.splice(
            payload.first,
            payload.nodes.length,
            ...payload.nodes.map((n) => n.key),
          );
          return new Map(prev).set(payload.node, keys);
        });
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, [session]);

  useEffect(() => {
    if (session === null) return;
    const unlisten = getCurrentWebviewWindow().listen<DissectTaskProgress>(
      "dissect-task-progress",
      ({ payload }) => {
        if (payload.id !== session) return;
        setTasks((prev) => ({ ...prev, [payload.task]: payload }));
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, [session]);

  // A finished task's last report would otherwise greet the next one.
  const forgetTask = useCallback(
    (task: DissectTask) =>
      setTasks((prev) => {
        const next = { ...prev };
        delete next[task];
        return next;
      }),
    [],
  );
  useEffect(() => {
    if (reading === 0) forgetTask("decode");
  }, [reading, forgetTask]);
  useEffect(() => {
    if (saving === 0) forgetTask("save");
  }, [saving, forgetTask]);

  const busy = loading.size > 0 || working > 0 || reading > 0 || saving > 0;
  const showBusy = useAfterGrace(busy);
  // What the status bar says is going on: the field loading (the one
  // furthest along, if several are), else what else is.
  const busyText = (() => {
    if (loading.size > 0) {
      const keys = [...loading.keys()];
      const measured = keys.filter((k) => progress.has(k));
      const key = measured.length
        ? measured.reduce((a, b) =>
            percentOf(progress.get(a)!) >= percentOf(progress.get(b)!) ? a : b,
          )
        : keys[0];
      const name = nodes.get(key)?.name ?? "";
      const done = progress.get(key);
      const codec = done?.codec ? ` · decoding ${done.codec}` : "";
      const more = keys.length > 1 ? ` and ${keys.length - 1} more` : "";
      return `Loading “${name}”${codec}${measure(done)}${more}`;
    }
    if (saving > 0) return `Saving${measure(tasks.save)}`;
    if (reading > 0) {
      const codec = tasks.decode?.codec ? ` ${tasks.decode.codec}` : "";
      return `Decoding${codec}${measure(tasks.decode)}`;
    }
    if (working > 0) return "Working…";
    return null;
  })();
  /// Stop what the dissection is doing: one node's work, or everything.
  const stop = useCallback(
    (key: number | null) => {
      if (session !== null) safeSilent(commands.dissectStop(session, key));
    },
    [session],
  );

  // Escape stops whatever is in flight, before it clears the filter or
  // closes the viewer.
  useEffect(() => {
    // An open palette takes Escape for itself.
    if (!busy || host.embedded || asTarget) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      e.stopPropagation();
      stop(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [busy, host.embedded, asTarget, stop]);

  const expand = useCallback(
    async (key: number) => {
      const n = nodes.get(key);
      if (!n || n.children.state === "leaf") return;
      setExpanded((prev) => new Set(prev).add(key));
      if (!children.has(key) || n.children.state === "locked") {
        await loadChildren(key, Math.max(PAGE, n.children.loaded));
      }
    },
    [nodes, children, loadChildren],
  );

  const collapse = useCallback(
    (key: number) => {
      if (loading.has(key)) stop(key);
      setExpanded((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    },
    [loading, stop],
  );

  const loadMore = useCallback(
    (parent: number) =>
      loadChildren(parent, (children.get(parent)?.length ?? 0) + PAGE),
    [children, loadChildren],
  );

  const activate = useCallback(
    (row: Row) => {
      if (row.kind === "more") {
        void loadMore(row.parent);
      } else if (nodes.get(row.key)?.children.state === "locked") {
        void expand(row.key);
      } else if (expanded.has(row.key)) {
        collapse(row.key);
      } else {
        void expand(row.key);
      }
    },
    [nodes, expanded, collapse, expand, loadMore],
  );

  // --- The hex pane: the byte space the selected field lives in ---

  const rootNode = rootKey !== null ? nodes.get(rootKey) : undefined;
  const fileSource = rootNode?.span?.source ?? 0;
  const span = node?.span ?? null;
  const source = span?.source ?? fileSource;
  const [sources, setSources] = useState<Map<number, DissectSource>>(new Map());
  const sourceInfo = sources.get(source);
  const sourceLen =
    source === fileSource ? fileSize : (sourceInfo?.len ?? span?.len ?? 0);
  const lenKnown = source === fileSource || (sourceInfo?.len_known ?? true);

  /// Ask again for the sources whose length was only a bound: decoding
  /// may have reached their end since.
  const refreshUnknownLengths = useCallback(
    () =>
      setSources((prev) => {
        const unknown = [...prev].filter(([, info]) => !info.len_known);
        if (unknown.length === 0) return prev;
        const next = new Map(prev);
        for (const [s] of unknown) next.delete(s);
        return next;
      }),
    [],
  );
  useEffect(() => {
    if (loading.size === 0) refreshUnknownLengths();
  }, [loading.size, refreshUnknownLengths]);

  /// The size of `s`, unless it runs to the end of a stream whose length
  /// isn't known yet.
  const sizeOf = (s: DissectSpan) => {
    const info = s.source === fileSource ? undefined : sources.get(s.source);
    return info && !info.len_known && s.offset + s.len >= info.len
      ? "size unknown"
      : formatSize(s.len);
  };

  useEffect(() => {
    if (session === null || sources.has(source)) return;
    let cancelled = false;
    (async () => {
      const info = await safeSilent(commands.dissectSource(session, source));
      if (info && !cancelled) {
        setSources((prev) => new Map(prev).set(source, info));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [session, source, sources]);

  const caches = useRef(new Map<number, LruChunkCache>());
  useEffect(() => {
    caches.current = new Map();
  }, [fileKey]);
  const cacheFor = (s: number) => {
    let cache = caches.current.get(s);
    if (!cache) {
      cache = new LruChunkCache(32);
      caches.current.set(s, cache);
    }
    return cache;
  };
  const [, bump] = useState(0);

  const [cursor, setCursor] = useState(0);
  const [range, setRange] = useState<Range | null>(null);
  // Whether a new selection moves the hex cursor to it: not when the
  // selection came from the hex pane itself.
  const followSelection = useRef(true);

  const hexRef = useRef<HTMLDivElement>(null);
  const [hexTop, setHexTop] = useState(0);
  const [hexHeight, setHexHeight] = useState(0);
  const hexRows = Math.ceil(sourceLen / HEX_BYTES_PER_ROW);
  const hexNatural = hexRows * HEX_ROW;
  const hexScale =
    hexNatural > MAX_SCROLL_HEIGHT ? hexNatural / MAX_SCROLL_HEIGHT : 1;
  const hexVisible = Math.max(1, Math.floor(hexHeight / HEX_ROW));
  const hexFirst = Math.max(0, hexTop - OVERSCAN);
  const hexLast = Math.min(hexRows, hexTop + hexVisible + OVERSCAN);
  const syncHex = useCallback(
    (el: HTMLElement) =>
      setHexTop(
        Math.min(
          Math.max(0, hexRows - hexVisible),
          Math.floor((el.scrollTop * hexScale) / HEX_ROW),
        ),
      ),
    [hexRows, hexVisible, hexScale],
  );
  useWheelScroll(hexRef, { sync: syncHex });

  const scrollHexTo = useCallback(
    (row: number, center: boolean) => {
      const el = hexRef.current;
      if (!el) return;
      const top = center ? row - Math.floor(hexVisible / 3) : row;
      el.scrollTop = (Math.max(0, top) * HEX_ROW) / hexScale;
      setHexTop(Math.max(0, top));
    },
    [hexVisible, hexScale],
  );

  /// Keep byte `offset` in view.
  const revealByte = useCallback(
    (offset: number) => {
      const row = Math.floor(offset / HEX_BYTES_PER_ROW);
      if (row < hexTop || row >= hexTop + hexVisible) {
        scrollHexTo(row, true);
      }
    },
    [hexTop, hexVisible, scrollHexTo],
  );

  // Before paint, like the tree's: the bytes scrolled to are drawn with
  // the scroll.
  useLayoutEffect(() => {
    if (!followSelection.current) {
      followSelection.current = true;
      return;
    }
    if (!span) return;
    setCursor(span.offset);
    setRange(null);
    const row = Math.floor(span.offset / HEX_BYTES_PER_ROW);
    if (row < hexTop || row >= hexTop + hexVisible) scrollHexTo(row, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected]);

  // Load the chunks on screen.
  const firstChunk = Math.floor((hexFirst * HEX_BYTES_PER_ROW) / CHUNK_SIZE);
  const lastChunk = Math.floor((hexLast * HEX_BYTES_PER_ROW) / CHUNK_SIZE);
  useEffect(() => {
    if (session === null || sourceLen === 0) return;
    let cancelled = false;
    (async () => {
      const cache = cacheFor(source);
      let loaded = false;
      for (let ci = firstChunk; ci <= lastChunk && !cancelled; ci++) {
        if (cache.has(ci)) continue;
        const offset = ci * CHUNK_SIZE;
        if (offset >= sourceLen) break;
        const decoded = source !== fileSource;
        if (decoded) setReading((n) => n + 1);
        try {
          const bytes = decoded
            ? await unwrapBytes(
                commands.dissectRead(session, source, offset, CHUNK_SIZE),
              )
            : await unwrapBytes(
                commands.readFileRange(vfsPath, offset, CHUNK_SIZE),
              );
          cache.set(ci, bytes);
          loaded = true;
          // A short read is the end of the stream.
          if (decoded && bytes.length < CHUNK_SIZE) refreshUnknownLengths();
        } catch (e) {
          if (!wasStopped(e)) console.error("dissect: failed to load bytes", e);
          break;
        } finally {
          if (decoded) setReading((n) => n - 1);
        }
      }
      if (loaded && !cancelled) bump((n) => n + 1);
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session, source, firstChunk, lastChunk, sourceLen]);

  const byteAt = (offset: number): number | undefined => {
    const ci = Math.floor(offset / CHUNK_SIZE);
    const chunk = caches.current.get(source)?.get(ci);
    if (!chunk) return undefined;
    return chunk[offset - ci * CHUNK_SIZE];
  };

  useLayoutEffect(() => {
    const el = hexRef.current;
    if (!el) return;
    setHexHeight(el.clientHeight);
    const observer = new ResizeObserver(() => setHexHeight(el.clientHeight));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // --- Tree scrolling ---

  const treeRef = useRef<HTMLDivElement>(null);
  const [treeTop, setTreeTop] = useState(0);
  const syncTree = useCallback(
    (el: HTMLElement) => setTreeTop(el.scrollTop),
    [],
  );
  useWheelScroll(treeRef, { sync: syncTree });
  const [treeHeight, setTreeHeight] = useState(0);
  useLayoutEffect(() => {
    const el = treeRef.current;
    if (!el) return;
    setTreeHeight(el.clientHeight);
    const observer = new ResizeObserver(() => setTreeHeight(el.clientHeight));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const treeVisible = Math.max(1, Math.floor(treeHeight / TREE_ROW));
  const treeFirst = Math.max(0, Math.floor(treeTop / TREE_ROW) - OVERSCAN);
  const treeLast = Math.min(
    rows.length,
    Math.ceil((treeTop + treeHeight) / TREE_ROW) + OVERSCAN,
  );

  // Keep the selected line in view. The lines drawn follow in the same
  // pass: left to the scroll event, a jump of a page would be painted
  // before them, empty.
  useLayoutEffect(() => {
    const el = treeRef.current;
    if (!el || selectedIndex < 0) return;
    const top = selectedIndex * TREE_ROW;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + TREE_ROW > el.scrollTop + el.clientHeight) {
      el.scrollTop = top + TREE_ROW - el.clientHeight;
    }
    setTreeTop(el.scrollTop);
  }, [selectedIndex]);

  // --- Focus ---

  const focusZone = useCallback(
    (z: Zone) => {
      setZone(z);
      if (z === "hex") setBytesOpen(true);
      if (host.embedded) return;
      (z === "tree" ? treeRef : hexRef).current?.focus();
    },
    [host.embedded],
  );

  const toggleBytes = () => {
    if (bytesOpen && zone === "hex") focusZone("tree");
    setBytesOpen(!bytesOpen);
  };

  // A hidden pane loses its scroll position (and can't take the focus), so
  // both are restored when it shows again.
  useLayoutEffect(() => {
    const el = hexRef.current;
    if (!bytesOpen || !el) return;
    el.scrollTop = (hexTop * HEX_ROW) / hexScale;
    if (zone === "hex" && !host.embedded) el.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [bytesOpen]);

  // A password asked for on opening keeps the focus in its bar.
  useEffect(() => {
    if (!host.embedded && session !== null && !secret) {
      treeRef.current?.focus();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [host.embedded, session]);

  // --- Locating the field at a byte ---

  const locate = useCallback(
    async (src: number, offset: number, fromHex: boolean) => {
      if (session === null) return;
      setWorking((n) => n + 1);
      try {
        const r = await unwrap(commands.dissectLocate(session, src, offset));
        const target = r.path[r.path.length - 1];
        const found = r.pages
          .flatMap((p) => [p.parent, ...p.nodes])
          .find((n) => n.key === target);
        // A byte clicked that no field of its stream holds leaves the
        // selection, and with it the stream, where it is.
        if (fromHex && found?.span?.source !== src) return;
        // The field found may be one the filter hides.
        setFilter(null);
        for (const page of r.pages) merge(page);
        setExpanded((prev) => {
          const next = new Set(prev);
          for (const k of r.path.slice(0, -1)) next.add(k);
          return next;
        });
        if (fromHex && target !== selected) followSelection.current = false;
        setSelected(target);
        if (r.secret) {
          setSecret({
            request: r.secret,
            retry: () => locateRef.current(src, offset, fromHex),
          });
        }
      } catch (e) {
        setError(String(e));
      } finally {
        setWorking((n) => n - 1);
      }
    },
    [session, merge, selected],
  );
  const locateRef = useRef(locate);
  locateRef.current = locate;

  const answerSecret = async (password: string | null) => {
    const pending = secret;
    if (!pending || session === null) return;
    setSecret(null);
    focusZone(zone);
    await safe(
      commands.dissectAnswerSecret(session, pending.request.index, password),
    );
    pending.retry();
  };

  const goToOffset = useCallback(
    (value: string) => {
      const offset = parseInt(value.replace(/^0x/i, ""), 16);
      if (isNaN(offset) || offset < 0 || offset >= sourceLen) return;
      setCursor(offset);
      setRange(null);
      scrollHexTo(Math.floor(offset / HEX_BYTES_PER_ROW), true);
      void locate(source, offset, true);
    },
    [sourceLen, source, locate, scrollHexTo],
  );

  const followTarget = useCallback(() => {
    const target = node?.target;
    if (!target) return false;
    void locate(target.source, target.offset, false);
  }, [node, locate]);

  // --- Copying ---

  const copyText = (text: string) => {
    void navigator.clipboard.writeText(text);
  };

  const copyBytes = useCallback(
    (s: DissectSpan | null, format: CopyFormat) => {
      if (!s || session === null || s.len === 0) return false;
      safe(commands.dissectCopy(session, s.source, s.offset, s.len, format));
    },
    [session],
  );

  const hexSelection = (): DissectSpan | null => {
    if (!range) return null;
    const start = Math.min(range.anchor, range.head);
    const end = Math.max(range.anchor, range.head);
    return { source, offset: start, len: end - start + 1 };
  };

  const copy = () => {
    if (zone === "hex" && range) return copyBytes(hexSelection(), "hex");
    if (!node) return false;
    copyText(nodeLine(node));
  };
  const copyValue = () => {
    if (!node) return false;
    copyText(
      node.value ? copyableValue(node.value) : (node.summary ?? node.name),
    );
  };

  /// The selected line and what's shown below it, as the tree shows them:
  /// collapsed nodes collapsed, each line's bytes after it, problems below.
  const treeText = (): string | null => {
    if (selectedIndex < 0) return null;
    const base = rows[selectedIndex].depth;
    const lines: string[] = [];
    for (let i = selectedIndex; i < rows.length; i++) {
      const row = rows[i];
      if (i > selectedIndex && row.depth <= base) break;
      const indent = "  ".repeat(row.depth - base);
      if (row.kind === "more") {
        const shown = children.get(row.parent)?.length ?? 0;
        const total = nodes.get(row.parent)?.children.count;
        const of = total
          ? ` of ${total.at_least ? "at least " : ""}${total.n}`
          : "";
        lines.push(`${indent}  … (${shown} shown${of})`);
        continue;
      }
      const n = nodes.get(row.key);
      if (!n) continue;
      const marker =
        n.children.state === "leaf"
          ? "  "
          : expanded.has(row.key)
            ? "▾ "
            : "▸ ";
      const bytes = n.span
        ? `  [0x${n.span.offset.toString(16)}+0x${n.span.len.toString(16)}]`
        : "";
      lines.push(`${indent}${marker}${nodeLine(n)}${bytes}`);
      for (const d of n.diagnostics) lines.push(`${indent}    ! ${d.message}`);
    }
    return lines.join("\n") + "\n";
  };
  const copyTree = () => {
    const text = treeText();
    if (text === null) return false;
    copyText(text);
  };

  /// A name to save the bytes of `n` under: the file it holds, else its
  /// own name.
  const saveName = (n: DissectNode): string => {
    let cursor: number | undefined = n.key;
    while (n.interpretation && cursor !== undefined) {
      const name = nodes.get(cursor)?.name ?? "";
      if (name.includes(".")) return name.split(/[/\\]/).pop() ?? name;
      cursor = parentOf.get(cursor);
    }
    return `${n.name.replace(/[/\\:*?"<>|]/g, "_")}.bin`;
  };

  /// Save the bytes of `s` to a file chosen in the save dialog.
  const saveBytes = async (s: DissectSpan | null, name: string) => {
    if (!s || session === null || s.len === 0) return false;
    const path = await saveDialog({ defaultPath: name });
    focusZone(zone);
    if (!path) return;
    setSaving((n) => n + 1);
    try {
      await unwrap(
        commands.dissectSave(session, s.source, s.offset, s.len, path),
      );
    } catch (e) {
      if (!wasStopped(e)) {
        await message(String(e), { kind: "error", title: "Error" });
      }
    } finally {
      setSaving((n) => n - 1);
    }
  };
  /// The hex pane's selection, else the selected field's bytes.
  const saveSelected = () => {
    if (zone === "hex" && range) {
      const base = node ? saveName(node).replace(/\.[^.]*$/, "") : "bytes";
      void saveBytes(hexSelection(), `${base}-${selStart.toString(16)}.bin`);
    } else if (node) {
      void saveBytes(span, saveName(node));
    } else {
      return false;
    }
  };

  useEmbeddedCopy(() => {
    if (!node) return false;
    copyText(nodeLine(node));
    return true;
  });

  const [goToOpen, setGoToOpen] = useState(false);

  /// What Dissect As would choose the format of for the selected line;
  /// null when it holds no file, undefined when that couldn't be found out.
  const selectedContent = async () => {
    if (session === null || selected === null) return undefined;
    const key = selected < 0 ? -selected - 1 : selected;
    setWorking((n) => n + 1);
    try {
      return await unwrap(commands.dissectContentOf(session, key));
    } catch (e) {
      if (!wasStopped(e)) setError(String(e));
      return undefined;
    } finally {
      setWorking((n) => n - 1);
    }
  };

  /// Choose the format of the file the selected field stands for.
  const openDissectAs = async () => {
    const content = await selectedContent();
    if (content === null) {
      setNotice(
        "Dissect As applies to a file: the file itself, an archive member, decompressed or embedded data",
      );
    } else if (content) {
      showDissectAs(content);
    }
  };

  // What the context menu's Dissect As applies to, found when it opens;
  // "pending" until then.
  const [menuContent, setMenuContent] = useState<
    DissectContent | null | "pending"
  >(null);

  const showDissectAs = (content: DissectContent) => {
    const { node: target, name } = content;
    setNodes((prev) => new Map(prev).set(target.key, target));
    const interpretation = target.interpretation;
    setAsTarget({
      key: target.key,
      name,
      current: interpretation?.forced
        ? (interpretation.format?.name ?? null)
        : null,
    });
  };

  /// The keys below `key`, as far as they're loaded.
  const descendants = (key: number): number[] =>
    (children.get(key) ?? []).flatMap((k) => [k, ...descendants(k)]);

  const pickFormat = async (picked: string | null) => {
    const target = asTarget;
    setAsTarget(null);
    if (!target || session === null || picked === target.current) return;
    const gone = descendants(target.key);
    startLoading(target.key);
    try {
      const { located: r, choices } = await unwrap(
        commands.dissectReinterpret(session, target.key, picked),
      );
      // Kept for when the file is dissected again, e.g. after F3 and back.
      if (!host.embedded) safe(commands.setViewerDissectFormats(choices));
      setChildren((prev) => {
        const next = new Map(prev);
        for (const k of gone) next.delete(k);
        return next;
      });
      setExpanded((prev) => {
        const next = new Set(prev);
        for (const k of gone) next.delete(k);
        for (const k of r.path) next.add(k);
        return next;
      });
      for (const page of r.pages) merge(page);
      setSelected(target.key);
      if (r.secret) {
        setSecret({
          request: r.secret,
          retry: () => loadChildrenRef.current(target.key, PAGE),
        });
      }
    } catch (e) {
      setError(String(e));
    } finally {
      endLoading(target.key);
    }
  };

  useScopedBindings(host.embedded || asTarget ? null : "viewer", {
    viewer_copy: copy,
    viewer_copy_value: copyValue,
    viewer_copy_bytes: () => copyBytes(span, "hex"),
    viewer_copy_tree: copyTree,
    viewer_save_bytes: saveSelected,
    viewer_follow_target: followTarget,
    viewer_goto: () => setGoToOpen(true),
    viewer_toggle_bytes: toggleBytes,
    viewer_dissect_as: () => void openDissectAs(),
    viewer_filter: () => {
      setZone("tree");
      if (filter === null) setFilter("");
      else filterRef.current?.focus();
    },
    viewer_select_all: () => {
      if (!span || span.len === 0) return false;
      setRange({ anchor: span.offset, head: span.offset + span.len - 1 });
      focusZone("hex");
    },
  });

  // The native Edit menu.
  const menuRef = useRef({ copy, selectAll: () => {}, goTo: () => {} });
  menuRef.current = {
    copy,
    selectAll: () => {
      if (span && span.len > 0) {
        setRange({ anchor: span.offset, head: span.offset + span.len - 1 });
      }
    },
    goTo: () => setGoToOpen(true),
  };
  useEffect(() => {
    if (host.embedded) return;
    const unlisten = getCurrentWebviewWindow().listen<string>(
      "viewer-menu",
      (event) => {
        if (event.payload === "copy") menuRef.current.copy();
        else if (event.payload === "select_all") menuRef.current.selectAll();
        else if (event.payload === "goto") menuRef.current.goTo();
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, [host.embedded]);

  // --- Keys ---

  const selectRow = (index: number) => {
    const row = rows[Math.max(0, Math.min(index, rows.length - 1))];
    if (row) setSelected(rowId(row));
  };

  const onTreeKey = (e: React.KeyboardEvent) => {
    const row = rows[selectedIndex];
    switch (e.key) {
      case "ArrowDown":
        selectRow(selectedIndex + 1);
        break;
      case "ArrowUp":
        selectRow(selectedIndex - 1);
        break;
      case "PageDown":
        selectRow(selectedIndex + treeVisible - 1);
        break;
      case "PageUp":
        selectRow(selectedIndex - treeVisible + 1);
        break;
      case "Home":
        selectRow(0);
        break;
      case "End":
        selectRow(rows.length - 1);
        break;
      case "ArrowRight":
        if (!row || row.kind !== "node") return;
        if (!expanded.has(row.key)) void expand(row.key);
        else if (rows[selectedIndex + 1]?.depth > row.depth) {
          selectRow(selectedIndex + 1);
        }
        break;
      case "ArrowLeft":
        if (!row) return;
        if (row.kind === "node" && expanded.has(row.key)) collapse(row.key);
        else {
          const parent =
            row.kind === "more" ? row.parent : parentOf.get(row.key);
          if (parent !== undefined) setSelected(parent);
        }
        break;
      case "Enter":
      case " ":
        if (!row || e.metaKey || e.ctrlKey) return;
        activate(row);
        break;
      case "Tab":
        if (bytesOpen) focusZone("hex");
        break;
      case "Escape":
        if (filter === null) return;
        setFilter(null);
        e.stopPropagation();
        break;
      default:
        return;
    }
    e.preventDefault();
  };

  const moveCursor = (to: number, extend: boolean) => {
    const offset = Math.max(0, Math.min(to, sourceLen - 1));
    setRange((prev) =>
      extend ? { anchor: prev?.anchor ?? cursor, head: offset } : null,
    );
    setCursor(offset);
    revealByte(offset);
  };

  const onHexKey = (e: React.KeyboardEvent) => {
    const extend = e.shiftKey;
    switch (e.key) {
      case "ArrowRight":
        moveCursor(cursor + 1, extend);
        break;
      case "ArrowLeft":
        moveCursor(cursor - 1, extend);
        break;
      case "ArrowDown":
        moveCursor(cursor + HEX_BYTES_PER_ROW, extend);
        break;
      case "ArrowUp":
        moveCursor(cursor - HEX_BYTES_PER_ROW, extend);
        break;
      case "PageDown":
        moveCursor(cursor + HEX_BYTES_PER_ROW * (hexVisible - 1), extend);
        break;
      case "PageUp":
        moveCursor(cursor - HEX_BYTES_PER_ROW * (hexVisible - 1), extend);
        break;
      case "Home":
        moveCursor(
          e.metaKey || e.ctrlKey ? 0 : cursor - (cursor % HEX_BYTES_PER_ROW),
          extend,
        );
        break;
      case "End":
        moveCursor(
          e.metaKey || e.ctrlKey
            ? sourceLen - 1
            : cursor - (cursor % HEX_BYTES_PER_ROW) + HEX_BYTES_PER_ROW - 1,
          extend,
        );
        break;
      case "Enter":
        if (e.metaKey || e.ctrlKey) return;
        void locate(source, cursor, true);
        break;
      case "Escape":
        if (!range) return;
        setRange(null);
        e.stopPropagation();
        break;
      case "Tab":
        focusZone("tree");
        break;
      default:
        return;
    }
    e.preventDefault();
  };

  // --- Mouse in the hex pane ---

  const dragging = useRef(false);
  /// The byte under the pointer, or between lines, columns or past a
  /// line's end (and outside the pane, while dragging) the nearest one:
  /// on the nearest line drawn, the nearest byte along it.
  const byteFromEvent = (e: React.MouseEvent | MouseEvent) => {
    const el = document.elementFromPoint(e.clientX, e.clientY);
    if (el instanceof HTMLElement && el.dataset.offset !== undefined) {
      return parseInt(el.dataset.offset, 10);
    }
    const lines = hexRef.current?.querySelectorAll<HTMLElement>(
      `.${styles.hexLine}`,
    );
    const nearest = (
      els: Iterable<HTMLElement>,
      gap: (r: DOMRect) => number,
    ) => {
      let best: HTMLElement | undefined;
      let bestGap = Infinity;
      for (const candidate of els) {
        const d = gap(candidate.getBoundingClientRect());
        if (d < bestGap) [best, bestGap] = [candidate, d];
      }
      return best;
    };
    const outside = (at: number, from: number, to: number) =>
      at < from ? from - at : at > to ? at - to : 0;
    const line = nearest(lines ?? [], (r) =>
      outside(e.clientY, r.top, r.bottom),
    );
    const byte = line
      ? nearest(line.querySelectorAll<HTMLElement>("[data-offset]"), (r) =>
          outside(e.clientX, r.left, r.right),
        )
      : undefined;
    return byte ? parseInt(byte.dataset.offset!, 10) : undefined;
  };
  const onHexMouseDown = (e: React.MouseEvent<HTMLElement>) => {
    if (e.button !== 0) return;
    // A press on the scrollbar is the scrollbar's.
    const pane = e.currentTarget;
    const box = pane.getBoundingClientRect();
    if (
      e.clientX >= box.left + pane.clientLeft + pane.clientWidth ||
      e.clientY >= box.top + pane.clientTop + pane.clientHeight
    ) {
      return;
    }
    const offset = byteFromEvent(e);
    if (offset === undefined) return;
    e.preventDefault();
    focusZone("hex");
    if (e.shiftKey) {
      setRange((prev) => ({ anchor: prev?.anchor ?? cursor, head: offset }));
    } else {
      setRange(null);
      void locate(source, offset, true);
    }
    setCursor(offset);
    dragging.current = true;
  };
  useEffect(() => {
    const move = (e: MouseEvent) => {
      if (!dragging.current) return;
      const offset = byteFromEvent(e);
      if (offset === undefined) return;
      setRange((prev) => ({ anchor: prev?.anchor ?? offset, head: offset }));
      setCursor(offset);
    };
    const up = () => {
      dragging.current = false;
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
    return () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
  }, []);

  // --- Rendering ---

  const selStart = range ? Math.min(range.anchor, range.head) : -1;
  const selEnd = range ? Math.max(range.anchor, range.head) : -2;
  const target = node?.target ?? null;

  const byteClass = (offset: number, ascii: boolean) => {
    const classes = [];
    if (offset >= selStart && offset <= selEnd) {
      classes.push(
        zone === "hex" ? styles.byteSelected : styles.byteSelectedInactive,
      );
    } else if (inSpan(span, source, offset)) {
      classes.push(styles.byteSpan);
    } else if (inSpan(target, source, offset)) {
      classes.push(styles.byteTarget);
    }
    if (offset === cursor && zone === "hex" && !ascii)
      classes.push(styles.byteCursor);
    return classes.join(" ") || undefined;
  };

  const shortcut = (id: string) => {
    const k = shortcuts.get(id);
    return k ? <span className={menuStyles.shortcut}>{k}</span> : null;
  };

  const treeRows = rows.slice(treeFirst, treeLast).map((row, i) => {
    const index = treeFirst + i;
    const id = rowId(row);
    const isSelected = id === selected;
    const indent = {
      paddingLeft: `calc(${row.depth} * 16px + var(--space-3))`,
    };
    const rowClass = `${styles.treeRow} ${
      isSelected
        ? zone === "tree"
          ? styles.treeRowSelected
          : styles.treeRowSelectedInactive
        : ""
    }`;
    if (row.kind === "more") {
      const parent = nodes.get(row.parent);
      const shown = children.get(row.parent)?.length ?? 0;
      const total = parent?.children.count;
      const busy = loading.has(row.parent);
      const moreButton = (
        <button
          type="button"
          tabIndex={-1}
          className={styles.moreButton}
          onClick={() => void loadMore(row.parent)}
        >
          {parent?.children.state === "stopped"
            ? "Stopped — continue"
            : `Show ${PAGE} more`}
        </button>
      );
      return (
        <div
          key={`m${row.parent}`}
          className={`${rowClass} ${styles.moreRow}`}
          style={{ top: index * TREE_ROW, ...indent }}
          onMouseDown={() => setSelected(id)}
          onDoubleClick={() => activate(row)}
        >
          <span className={styles.chevron}>
            {busy && <AfterGrace>{<Spinner />}</AfterGrace>}
          </span>
          {busy ? (
            <AfterGrace before={moreButton}>
              <span className={styles.moreLoading}>Loading…</span>
            </AfterGrace>
          ) : (
            moreButton
          )}
          <span className={styles.summary}>
            {shown.toLocaleString()} of{" "}
            {total
              ? `${total.at_least ? "at least " : ""}${total.n.toLocaleString()}`
              : "?"}{" "}
            shown
          </span>
        </div>
      );
    }
    const n = nodes.get(row.key);
    if (!n) return null;
    const expandable = n.children.state !== "leaf";
    const isOpen = expanded.has(row.key);
    const worst = n.diagnostics.concat(
      n.children.error ? [n.children.error] : [],
    );
    return (
      <div
        key={row.key}
        className={rowClass}
        style={{ top: index * TREE_ROW, ...indent }}
        onMouseDown={(e) => {
          if (e.button === 2) setSelected(id);
        }}
        onClick={() => {
          setSelected(id);
          focusZone("tree");
        }}
        onDoubleClick={() => expandable && activate(row)}
        role="treeitem"
        aria-level={row.depth + 1}
        aria-expanded={expandable ? isOpen : undefined}
        aria-selected={isSelected}
      >
        <span
          className={styles.chevron}
          data-open={isOpen || undefined}
          onClick={(e) => {
            e.stopPropagation();
            setSelected(id);
            if (expandable) activate(row);
          }}
        >
          {loading.has(row.key) ? (
            <AfterGrace before={expandable && <IconChevronRight />}>
              <Spinner />
            </AfterGrace>
          ) : (
            expandable && <IconChevronRight />
          )}
        </span>
        <span className={styles.name}>{n.name}</span>
        {n.interpretation &&
          (n.interpretation.forced ? (
            <span
              className={styles.forced}
              title={`Dissected as ${n.interpretation.format?.title}, chosen with Dissect As`}
            >
              as {n.interpretation.format?.title}
            </span>
          ) : (
            <span
              className={styles.format}
              title={n.interpretation.format?.title ?? "Not recognised"}
            >
              {n.interpretation.format?.name ?? "?"}
            </span>
          ))}
        {n.value && (
          <>
            <span className={styles.colon}>:</span>
            <ValueInline value={n.value} />
          </>
        )}
        {n.summary && <span className={styles.summary}>— {n.summary}</span>}
        {n.children.state === "locked" && (
          <span
            className={styles.lock}
            title="Needs a password: press Enter to unlock"
          >
            {"\u{f033e}"}
          </span>
        )}
        {worst.length > 0 && (
          <span
            className={styles.diagBadge}
            data-kind={worst[0].kind}
            title={worst
              .map((d) => `${DIAG_LABELS[d.kind]}: ${d.message}`)
              .join("\n")}
          >
            !
          </span>
        )}
        {n.span && index > 0 && (
          <span className={styles.offset}>{hex(n.span.offset)}</span>
        )}
      </div>
    );
  });

  const hexLines = [];
  for (let r = hexFirst; r < hexLast; r++) {
    const base = r * HEX_BYTES_PER_ROW;
    const bytes = [];
    const chars = [];
    for (let j = 0; j < HEX_BYTES_PER_ROW; j++) {
      const offset = base + j;
      if (offset >= sourceLen) break;
      const b = byteAt(offset);
      bytes.push(
        <span
          key={j}
          data-offset={offset}
          className={byteClass(offset, false)}
          data-gap={j === 7 ? "wide" : undefined}
        >
          {b === undefined ? "··" : hexByte(b)}
        </span>,
      );
      chars.push(
        <span key={j} data-offset={offset} className={byteClass(offset, true)}>
          {b === undefined ? " " : printableAscii(b)}
        </span>,
      );
    }
    hexLines.push(
      <div
        key={r}
        className={styles.hexLine}
        style={{ top: (r - hexTop) * HEX_ROW + (hexTop * HEX_ROW) / hexScale }}
      >
        <span className={styles.hexOffset}>{formatHexOffset(base)}</span>
        <span className={styles.hexBytes}>{bytes}</span>
        <span className={styles.hexChars}>{chars}</span>
      </div>,
    );
  }

  const origin = sourceInfo?.origin ?? null;
  const sourceLabel =
    source === fileSource
      ? "File"
      : origin
        ? `${origin.transform} of ${spanText(origin.parent)}`
        : `Stream #${source}`;

  return (
    <div className={`${viewerStyles.viewer} ${styles.root}`}>
      <div className={styles.body}>
        <div
          className={styles.treePane}
          {...keyboardMenuHandlers(
            () =>
              treeRef.current?.querySelector('[aria-selected="true"]') ??
              treeRef.current?.querySelector('[role="treeitem"]'),
          )}
        >
          <CM.Root
            onOpenChange={(open) => {
              if (!open || host.embedded) return;
              setMenuContent("pending");
              void selectedContent().then((c) => setMenuContent(c ?? null));
            }}
          >
            <CM.Trigger asChild>
              <div
                className={`${styles.tree} ${pattern ? styles.treeFiltered : ""}`}
                ref={treeRef}
                tabIndex={-1}
                role="tree"
                id={treeId}
                aria-label={`Structure of ${filePath}`}
                onKeyDown={onTreeKey}
                onFocus={() => setZone("tree")}
                onScroll={(e) => syncTree(e.currentTarget)}
              >
                <div
                  style={{
                    height: rows.length * TREE_ROW,
                    position: "relative",
                  }}
                >
                  {treeRows}
                </div>
                {error && <div className={styles.error}>{error}</div>}
                {session === null && !error && (
                  <div className={styles.placeholder}>
                    <Spinner />
                    Dissecting…
                  </div>
                )}
              </div>
            </CM.Trigger>
            <CM.Portal>
              <CM.Content
                className={menuStyles.content}
                loop
                onCloseAutoFocus={(e) => {
                  e.preventDefault();
                  treeRef.current?.focus();
                }}
              >
                <CM.Item
                  className={menuStyles.item}
                  disabled={!node}
                  onSelect={copy}
                >
                  Copy {shortcut("viewer_copy")}
                </CM.Item>
                <CM.Item
                  className={menuStyles.item}
                  disabled={!node}
                  onSelect={copyValue}
                >
                  Copy Value {shortcut("viewer_copy_value")}
                </CM.Item>
                <CM.Item
                  className={menuStyles.item}
                  disabled={!node}
                  onSelect={copyTree}
                >
                  Copy Tree {shortcut("viewer_copy_tree")}
                </CM.Item>
                <CM.Separator className={menuStyles.separator} />
                <CM.Sub>
                  <CM.SubTrigger
                    className={menuStyles.item}
                    disabled={!span || span.len === 0}
                  >
                    Copy Bytes
                    <span className={menuStyles.shortcut}>›</span>
                  </CM.SubTrigger>
                  <CM.Portal>
                    <CM.SubContent className={menuStyles.content} loop>
                      <CM.Item
                        className={menuStyles.item}
                        onSelect={() => copyBytes(span, "hex")}
                      >
                        As Hex {shortcut("viewer_copy_bytes")}
                      </CM.Item>
                      <CM.Item
                        className={menuStyles.item}
                        onSelect={() => copyBytes(span, "ascii")}
                      >
                        As Text
                      </CM.Item>
                      <CM.Item
                        className={menuStyles.item}
                        onSelect={() => copyBytes(span, "base64")}
                      >
                        As Base64
                      </CM.Item>
                    </CM.SubContent>
                  </CM.Portal>
                </CM.Sub>
                {!host.embedded && (
                  <CM.Item
                    className={menuStyles.item}
                    disabled={!span || span.len === 0}
                    onSelect={() =>
                      node && void saveBytes(span, saveName(node))
                    }
                  >
                    Save Bytes As… {shortcut("viewer_save_bytes")}
                  </CM.Item>
                )}
                <CM.Separator className={menuStyles.separator} />
                <CM.Item
                  className={menuStyles.item}
                  disabled={!target}
                  onSelect={followTarget}
                >
                  Go to Target {shortcut("viewer_follow_target")}
                </CM.Item>
                <CM.Item
                  className={menuStyles.item}
                  onSelect={() => setGoToOpen(true)}
                >
                  Go to Offset… {shortcut("viewer_goto")}
                </CM.Item>
                <CM.Separator className={menuStyles.separator} />
                {!host.embedded && (
                  <CM.Item
                    className={menuStyles.item}
                    disabled={!menuContent || menuContent === "pending"}
                    onSelect={() => {
                      if (menuContent && menuContent !== "pending") {
                        showDissectAs(menuContent);
                      }
                    }}
                  >
                    {menuContent && menuContent !== "pending"
                      ? `Dissect “${menuContent.name}” As…`
                      : "Dissect As…"}{" "}
                    {shortcut("viewer_dissect_as")}
                  </CM.Item>
                )}
                <CM.Separator className={menuStyles.separator} />
                <CM.CheckboxItem
                  className={menuStyles.item}
                  checked={bytesOpen}
                  onCheckedChange={toggleBytes}
                >
                  <span className={menuStyles.checkColumn}>
                    <CM.ItemIndicator>✓</CM.ItemIndicator>
                  </span>
                  Bytes Panel {shortcut("viewer_toggle_bytes")}
                </CM.CheckboxItem>
              </CM.Content>
            </CM.Portal>
          </CM.Root>
          {filter !== null && (
            <div className={styles.filterBar}>
              <input
                ref={filterRef}
                type="text"
                className={styles.filterInput}
                value={filter}
                placeholder="Filter (regex)"
                aria-label="Filter"
                aria-controls={treeId}
                autoFocus
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                onChange={(e) => setFilter(e.target.value)}
                onFocus={() => setZone("tree")}
                onKeyDown={(e) => {
                  switch (e.key) {
                    case "Escape":
                      e.preventDefault();
                      e.stopPropagation();
                      focusZone("tree");
                      setFilter(null);
                      break;
                    case "Enter":
                    case "Tab":
                      e.preventDefault();
                      focusZone("tree");
                      break;
                    case "ArrowUp":
                    case "ArrowDown":
                    case "PageUp":
                    case "PageDown":
                      onTreeKey(e);
                      break;
                  }
                }}
              />
            </div>
          )}
        </div>
        <div className={styles.side} hidden={!bytesOpen}>
          <div className={styles.sourceBar}>
            <span>{sourceLabel}</span>
            <span
              className={styles.sourceLen}
              title={
                lenKnown
                  ? undefined
                  : "Not known until the stream has been decoded to its end"
              }
            >
              {lenKnown ? formatSize(sourceLen) : "Size unknown"}
            </span>
          </div>
          <CM.Root>
            <CM.Trigger asChild>
              <div
                className={styles.hex}
                ref={hexRef}
                tabIndex={-1}
                role="grid"
                aria-label="Bytes"
                onKeyDown={onHexKey}
                onFocus={() => setZone("hex")}
                onMouseDown={onHexMouseDown}
                onContextMenuCapture={
                  keyboardMenuHandlers(
                    () =>
                      hexRef.current?.querySelector(
                        `[data-offset="${cursor}"]`,
                      ) ?? hexRef.current?.querySelector("[data-offset]"),
                  ).onContextMenuCapture
                }
                onScroll={(e) => syncHex(e.currentTarget)}
              >
                <div
                  className={styles.hexContent}
                  style={{ height: Math.min(hexNatural, MAX_SCROLL_HEIGHT) }}
                >
                  {hexLines}
                </div>
              </div>
            </CM.Trigger>
            <CM.Portal>
              <CM.Content
                className={menuStyles.content}
                loop
                onCloseAutoFocus={(e) => {
                  e.preventDefault();
                  hexRef.current?.focus();
                }}
              >
                <CM.Item
                  className={menuStyles.item}
                  disabled={!(hexSelection() ?? span)?.len}
                  onSelect={() => copyBytes(hexSelection() ?? span, "hex")}
                >
                  Copy as Hex
                </CM.Item>
                <CM.Item
                  className={menuStyles.item}
                  disabled={!(hexSelection() ?? span)?.len}
                  onSelect={() => copyBytes(hexSelection() ?? span, "ascii")}
                >
                  Copy as Text
                </CM.Item>
                <CM.Separator className={menuStyles.separator} />
                <CM.Item
                  className={menuStyles.item}
                  onSelect={() => setGoToOpen(true)}
                >
                  Go to Offset… {shortcut("viewer_goto")}
                </CM.Item>
              </CM.Content>
            </CM.Portal>
          </CM.Root>
          <Details
            node={node}
            source={sourceLabel}
            sizeOf={sizeOf}
            onFollow={followTarget}
          />
        </div>
      </div>
      {!host.embedded && (
        <DissectAs
          target={asTarget}
          onPick={(picked) => void pickFormat(picked)}
          onClose={() => setAsTarget(null)}
          restoreFocus={() => focusZone(zone)}
        />
      )}
      {secret && !host.embedded && (
        <PasswordBar request={secret.request} onAnswer={answerSecret} />
      )}
      <GoToBar
        open={goToOpen}
        onClose={() => {
          setGoToOpen(false);
          focusZone(zone);
        }}
        label="Go to offset (hex)"
        placeholder="0"
        onSubmit={goToOffset}
      />
      <div
        className={viewerStyles.viewerStatus}
        onContextMenu={(e) => e.preventDefault()}
      >
        <span className={viewerStyles.statusText}>
          <span title={filePath}>{filePath}</span>
          <span className={viewerStyles.statusSeparator} aria-hidden>
            |
          </span>
          <span>
            Structure
            {rootNode?.interpretation?.forced && (
              <> as {rootNode.interpretation.format?.title}</>
            )}
          </span>
          {rootNode?.summary && (
            <>
              <span className={viewerStyles.statusSeparator} aria-hidden>
                |
              </span>
              <span>{rootNode.summary}</span>
            </>
          )}
          {pattern && (
            <>
              <span className={viewerStyles.statusSeparator} aria-hidden>
                |
              </span>
              <span>{matches.toLocaleString()} matching</span>
            </>
          )}
          {showBusy && busyText && (
            <>
              <span className={viewerStyles.statusSeparator} aria-hidden>
                |
              </span>
              <span className={styles.busy}>
                {busyText}
                {!host.embedded && (
                  <span className={styles.muted}> · Esc to stop</span>
                )}
              </span>
            </>
          )}
          <span className={viewerStyles.statusSeparator} aria-hidden>
            |
          </span>
          <span role="status">
            {notice ??
              (zone === "hex" || range
                ? range
                  ? `Sel ${formatHexOffset(selStart)}–${formatHexOffset(selEnd)} (${formatSize(selEnd - selStart + 1)})`
                  : `Offset ${formatHexOffset(cursor)}`
                : span
                  ? `${formatHexOffset(span.offset)} + ${sizeOf(span)}`
                  : "")}
          </span>
        </span>
        <span className={styles.statusEnd}>
          {showBusy && (
            <>
              <button
                tabIndex={-1}
                className={viewerStyles.modeToggleBtn}
                onClick={() => stop(null)}
                title="Stop loading (Escape)"
              >
                Stop
              </button>
              <span className={viewerStyles.statusSeparator} aria-hidden>
                |
              </span>
            </>
          )}
          <button
            tabIndex={-1}
            aria-pressed={bytesOpen}
            className={`${viewerStyles.modeToggleBtn} ${bytesOpen ? viewerStyles.modeToggleBtnActive : ""}`}
            onClick={toggleBytes}
            title={shortcuts.label(
              `${bytesOpen ? "Hide" : "Show"} the bytes panel`,
              "viewer_toggle_bytes",
            )}
          >
            Bytes
          </button>
          {!host.embedded && (
            <span className={viewerStyles.statusSeparator} aria-hidden>
              |
            </span>
          )}
          <ModeToggle currentMode="dissect" autoMode={autoMode} />
        </span>
      </div>
    </div>
  );
}

function Spinner() {
  return <span className={styles.spinner} aria-hidden />;
}

type Fraction = { done: number; total: number };

function percentOf({ done, total }: Fraction): number {
  return Math.min(100, Math.floor((done * 100) / Math.max(total, 1)));
}

/// How far a task in the status bar has got, or an ellipsis when nothing
/// measures it.
function measure(progress: Fraction | undefined): string {
  return progress ? ` ${percentOf(progress)}%` : "…";
}

/// `children` once whatever mounted it has been going on for a moment,
/// `before` until then: a quick load never shows as one.
function AfterGrace({
  children,
  before = null,
}: {
  children: React.ReactNode;
  before?: React.ReactNode;
}) {
  return <>{useAfterGrace(true) ? children : before}</>;
}

function Details({
  node,
  source,
  sizeOf,
  onFollow,
}: {
  node: DissectNode | null;
  source: string;
  sizeOf: (span: DissectSpan) => string;
  onFollow: () => void;
}) {
  if (!node) return <div className={styles.details} />;
  const v = node.value;
  const diagnostics = node.diagnostics.concat(
    node.children.error ? [node.children.error] : [],
  );
  const count = node.children.count;
  return (
    <div className={styles.details} aria-label="Field details">
      <div className={styles.detailsTitle}>{node.name}</div>
      <dl className={styles.detailsList}>
        {v && <ValueRows value={v} />}
        {node.summary && (
          <>
            <dt>Summary</dt>
            <dd>{node.summary}</dd>
          </>
        )}
        {node.interpretation && (
          <>
            <dt>Format</dt>
            <dd>
              {node.interpretation.format?.title ?? "Not recognised"}
              {node.interpretation.forced && (
                <span className={styles.muted}> (chosen)</span>
              )}
            </dd>
          </>
        )}
        {node.description && (
          <>
            <dt>About</dt>
            <dd className={styles.description}>{node.description}</dd>
          </>
        )}
        {node.span && (
          <>
            <dt>Bytes</dt>
            <dd className={styles.mono}>
              {hex(node.span.offset)}–{hex(node.span.offset + node.span.len)}{" "}
              <span className={styles.muted}>
                ({sizeOf(node.span)}, {source})
              </span>
            </dd>
          </>
        )}
        {node.target && (
          <>
            <dt>Points to</dt>
            <dd className={styles.mono}>
              <button
                type="button"
                tabIndex={-1}
                className={styles.link}
                onClick={onFollow}
              >
                {spanText(node.target)}
              </button>
            </dd>
          </>
        )}
        {node.children.state === "locked" && (
          <>
            <dt>Children</dt>
            <dd>Locked: needs a password</dd>
          </>
        )}
        {count && node.children.state !== "locked" && (
          <>
            <dt>Children</dt>
            <dd>
              {count.at_least ? "at least " : ""}
              {count.n.toLocaleString()}
            </dd>
          </>
        )}
      </dl>
      {diagnostics.length > 0 && (
        <ul className={styles.diagnostics}>
          {diagnostics.map((d, i) => (
            <li key={i} data-kind={d.kind}>
              <span className={styles.diagKind}>{DIAG_LABELS[d.kind]}</span>
              {d.message}
              {d.span && (
                <span className={styles.muted}> at {spanText(d.span)}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/// Asks for the password a dissection waits for. Enter unlocks (an empty
/// password too: some formats default to one), Escape skips the encrypted
/// content.
function PasswordBar({
  request,
  onAnswer,
}: {
  request: DissectSecret;
  onAnswer: (password: string | null) => void;
}) {
  const [value, setValue] = useState("");
  useEffect(() => setValue(""), [request]);
  const label = `${request.attempt > 0 ? "Wrong password. " : ""}${request.prompt}`;
  return (
    <div
      className={viewerStyles.searchBar}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          onAnswer(null);
        } else if (e.key === "Enter") {
          e.preventDefault();
          onAnswer(value);
        }
      }}
    >
      <span className={viewerStyles.searchStatus}>{label}:</span>
      <input
        className={viewerStyles.searchInput}
        type="password"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        aria-label={request.prompt}
        autoComplete="off"
        autoFocus
      />
      <button
        className={viewerStyles.searchBtn}
        onClick={() => onAnswer(value)}
        title="Unlock (Enter)"
      >
        Unlock
      </button>
      <button
        className={viewerStyles.searchBtn}
        onClick={() => onAnswer(null)}
        title="Skip (Escape)"
      >
        Skip
      </button>
    </div>
  );
}
