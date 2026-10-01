import { message } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";

import styles from "./Viewer.module.scss";
import { commands, type ViewerState } from "../lib/bindings";
import { useRemoteState, safe, unwrap, unwrapBytes } from "../lib/ipc";
import { useScopedBindings } from "../lib/scopedBindings";
import { useViewerHost } from "./host";
import type { VfsPath } from "../lib/types";
import {
  CHUNK_SIZE,
  MAX_CACHED_CHUNKS,
  LruChunkCache,
  buildFileUrl,
  fileVersion,
  type FileInfo,
  type ViewerMode,
} from "./helpers";
import { getAlternateMode } from "./ModeToggle";
import { TextViewer } from "./TextViewer";
import { HexViewer } from "./HexViewer";
import { ImageViewer } from "./ImageViewer";
import { MediaViewer } from "./MediaViewer";
import { PdfViewer } from "./PdfViewer";
import { TableViewer } from "./TableViewer";
import { MarkdownViewer } from "./MarkdownViewer";

type ViewerBodyProps = {
  displayPath: string;
  filePath: VfsPath | null;
  fileServerBase: string;
  viewerState: ViewerState | null;
  /// Called once the file can be shown in its mode (or has failed to load).
  onReady?: () => void;
};

/// The file in the mode its state names, in a viewer window or in Quick
/// View (see `ViewerHost`).
export function ViewerBody({
  displayPath,
  filePath,
  fileServerBase,
  viewerState,
  onReady,
}: ViewerBodyProps) {
  const viewerHost = useViewerHost();
  const [info, setInfo] = useState<FileInfo | null>(null);
  const fileUrl =
    filePath && info
      ? buildFileUrl(
          fileServerBase,
          filePath.vfs_id,
          filePath.path,
          fileVersion(info),
        )
      : "";
  const [error, setError] = useState<string | null>(null);
  const [autoMode, setAutoMode] = useState<ViewerMode | null>(null);

  const chunkCache = useRef(new LruChunkCache(MAX_CACHED_CHUNKS));
  // Which file this is: display paths can repeat (a search VFS shows its
  // own label for every entry).
  const fileKey = filePath ? JSON.stringify(filePath) : "";

  // Fetch file info when file path becomes available and push its
  // associated mode to Rust
  useEffect(() => {
    if (!filePath) return;
    // Reset state for new file
    setInfo(null);
    setError(null);
    setAutoMode(null);
    chunkCache.current.clear();

    let cancelled = false;
    (async () => {
      try {
        const inspected = await unwrap(commands.inspectFile(filePath));
        if (cancelled) return;
        setInfo(inspected.details as FileInfo);
        setAutoMode(inspected.viewer_mode);
        safe(viewerHost.setMode(inspected.viewer_mode));
      } catch (e: any) {
        if (cancelled) return;
        setError(e.toString());
        if (!viewerHost.embedded) {
          await message(e.toString(), { kind: "error", title: "Error" });
        }
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fileKey]);

  const stateMode = filePath
    ? ((viewerState?.mode as ViewerMode) ?? null)
    : null;
  // A new file starts in text mode until its detected mode comes back from
  // Rust; until then, show the mode just detected rather than text.
  const [confirmedFor, setConfirmedFor] = useState<string | null>(null);
  useEffect(() => {
    if (autoMode && stateMode === autoMode) setConfirmedFor(fileKey);
  }, [autoMode, stateMode, fileKey]);
  const currentMode = confirmedFor === fileKey ? stateMode : autoMode;

  const ready = !!error || (!!info && !!currentMode);
  useEffect(() => {
    if (ready) onReady?.();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready]);

  // Text mode decodes as UTF-8 until the sniff result lands or the user
  // picks an encoding. The BOM is skipped only when the effective encoding
  // is the one it announces.
  const detectedEncoding = viewerState?.encoding.detected ?? null;
  const selectedEncoding = viewerState?.encoding.selected ?? null;
  const encoding = selectedEncoding ?? detectedEncoding?.encoding ?? "UTF-8";
  const bomLen =
    detectedEncoding?.encoding === encoding ? detectedEncoding.bom_len : 0;
  const encodingLabel =
    selectedEncoding || detectedEncoding
      ? `${encoding}${bomLen > 0 ? " (BOM)" : ""}`
      : null;

  // Preload first hex chunk when switching to hex mode (or when auto-detected as hex)
  useEffect(() => {
    if (!info || currentMode !== "hex") return;
    if (chunkCache.current.has(0)) return;

    if (!filePath) return;
    const fp = filePath;
    (async () => {
      try {
        chunkCache.current.set(
          0,
          await unwrapBytes(commands.readFileRange(fp, 0, CHUNK_SIZE)),
        );
      } catch (e: any) {
        console.error("Failed to preload first chunk", e);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentMode, info]);

  const loadChunk = useCallback(
    async (chunkIndex: number) => {
      if (!filePath) return;
      if (chunkCache.current.has(chunkIndex)) return;
      const offset = chunkIndex * CHUNK_SIZE;
      try {
        chunkCache.current.set(
          chunkIndex,
          await unwrapBytes(
            commands.readFileRange(filePath, offset, CHUNK_SIZE),
          ),
        );
      } catch (e: any) {
        console.error("Failed to load chunk", chunkIndex, e);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [fileKey],
  );

  useScopedBindings(viewerHost.embedded ? null : "viewer", {
    viewer_toggle_hex: () => {
      if (!currentMode) return false;
      const resolved = autoMode ?? currentMode;
      safe(viewerHost.setMode(getAlternateMode(currentMode, resolved)));
    },
  });

  let content: React.ReactNode;

  if (error) {
    content = (
      <>
        <div className={styles.viewerContent} />
        <div
          className={styles.viewerStatus}
          onContextMenu={(e) => e.preventDefault()}
        >
          <span className={styles.statusError}>{error}</span>
        </div>
      </>
    );
  } else if (!filePath || !info || !currentMode) {
    content = (
      <>
        <div className={styles.viewerContent} />
        <div
          className={styles.viewerStatus}
          onContextMenu={(e) => e.preventDefault()}
        >
          {filePath ? <span>Loading...</span> : null}
        </div>
      </>
    );
  } else if (currentMode === "text") {
    content = (
      <TextViewer
        filePath={displayPath}
        vfsPath={filePath}
        fileSize={info.size}
        chunkCache={chunkCache}
        loadChunk={loadChunk}
        autoMode={autoMode ?? currentMode}
        encoding={encoding}
        bomLen={bomLen}
        encodingLabel={encodingLabel}
        needsSniff={detectedEncoding === null}
      />
    );
  } else if (currentMode === "table" && viewerState) {
    content = (
      <TableViewer
        filePath={displayPath}
        vfsPath={filePath}
        fileSize={info.size}
        chunkCache={chunkCache}
        loadChunk={loadChunk}
        autoMode={autoMode ?? currentMode}
        encoding={encoding}
        bomLen={bomLen}
        encodingLabel={encodingLabel}
        needsSniff={detectedEncoding === null}
        options={viewerState.table}
      />
    );
  } else if (currentMode === "markdown") {
    content = (
      <MarkdownViewer
        filePath={displayPath}
        vfsPath={filePath}
        fileServerBase={fileServerBase}
        fileSize={info.size}
        autoMode={autoMode ?? currentMode}
        encoding={encoding}
        bomLen={bomLen}
        encodingLabel={encodingLabel}
        needsSniff={detectedEncoding === null}
      />
    );
  } else if (currentMode === "image") {
    content = (
      <ImageViewer
        filePath={displayPath}
        vfsPath={filePath}
        fileUrl={fileUrl}
        fileSize={info.size}
        mimeType={info.mime_type}
        autoMode={autoMode ?? currentMode}
      />
    );
  } else if (currentMode === "audio" || currentMode === "video") {
    content = (
      <MediaViewer
        tag={currentMode}
        filePath={displayPath}
        fileUrl={fileUrl}
        fileSize={info.size}
        autoMode={autoMode ?? currentMode}
      />
    );
  } else if (currentMode === "pdf") {
    content = (
      <PdfViewer
        filePath={displayPath}
        fileUrl={fileUrl}
        fileSize={info.size}
        autoMode={autoMode ?? currentMode}
      />
    );
  } else {
    content = (
      <HexViewer
        filePath={displayPath}
        vfsPath={filePath}
        fileSize={info.size}
        chunkCache={chunkCache}
        loadChunk={loadChunk}
        autoMode={autoMode ?? currentMode}
      />
    );
  }

  return <>{content}</>;
}

// --- The viewer window ---

function Viewer() {
  const [searchParams] = useSearchParams();
  const viewerState = useRemoteState<ViewerState>("viewer");

  // Read file info from remote state, fall back to search params
  const displayPath =
    viewerState?.display_path ?? searchParams.get("path") ?? "";
  const filePath: VfsPath | null =
    viewerState?.file_path ??
    (searchParams.has("vfs_path")
      ? JSON.parse(searchParams.get("vfs_path")!)
      : null);
  const fileServerBase =
    viewerState?.file_server_base ?? searchParams.get("file_server_base") ?? "";

  useEffect(() => {
    if (displayPath) document.title = displayPath;
  }, [displayPath]);

  // Window-level Escape handler — closing the viewer is fundamental and
  // deliberately not a rebindable command. Sub-viewers/SearchBar
  // stopPropagation or preventDefault when they consume Escape. Tab has no
  // job outside a text field; left alone it walks focus off the content,
  // where no key reaches the viewer any more.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      if (e.key === "Escape") {
        safe(commands.closeWindow());
        e.preventDefault();
      } else if (
        e.key === "Tab" &&
        !(e.target instanceof HTMLInputElement) &&
        !(e.target instanceof HTMLTextAreaElement)
      ) {
        e.preventDefault();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  return (
    <ViewerBody
      displayPath={displayPath}
      filePath={filePath}
      fileServerBase={fileServerBase}
      viewerState={viewerState}
    />
  );
}

export default Viewer;
