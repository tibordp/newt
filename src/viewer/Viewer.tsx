import { invoke } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router-dom";

import styles from "./Viewer.module.scss";
import { safeCommand, useRemoteState } from "../lib/ipc";
import type { VfsPath } from "../lib/types";
import {
  CHUNK_SIZE,
  MAX_CACHED_CHUNKS,
  LruChunkCache,
  detectAutoMode,
  type FileChunk,
  type FileInfo,
  type ViewerMode,
} from "./helpers";
import { getAlternateMode } from "./ModeToggle";
import { TextViewer } from "./TextViewer";
import { HexViewer } from "./HexViewer";
import { ImageViewer } from "./ImageViewer";
import { MediaViewer } from "./MediaViewer";
import { PdfViewer } from "./PdfViewer";

// --- Main Viewer component ---

interface ViewerRemoteState {
  mode: string;
  file_path: VfsPath | null;
  display_path: string | null;
  file_server_base: string | null;
}

function Viewer() {
  const [searchParams] = useSearchParams();
  const viewerState = useRemoteState<ViewerRemoteState>("viewer");

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
  const fileUrl = filePath
    ? `${fileServerBase}/${filePath.vfs_id}${filePath.path}`
    : "";

  const [info, setInfo] = useState<FileInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [autoMode, setAutoMode] = useState<ViewerMode | null>(null);

  const chunkCache = useRef(new LruChunkCache(MAX_CACHED_CHUNKS));

  // Fetch file info when file path becomes available and push auto-detected mode to Rust
  useEffect(() => {
    if (!displayPath || !filePath) return;
    // Reset state for new file
    setInfo(null);
    setError(null);
    setAutoMode(null);
    chunkCache.current.clear();
    document.title = displayPath;

    (async () => {
      try {
        const fi: FileInfo = await invoke("file_details", { path: filePath });
        setInfo(fi);
        const mode = detectAutoMode(fi.mime_type);
        setAutoMode(mode);
        safeCommand("set_viewer_mode", { mode });
      } catch (e: any) {
        setError(e.toString());
        await message(e.toString(), { kind: "error", title: "Error" });
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [displayPath]);

  const currentMode = filePath
    ? ((viewerState?.mode as ViewerMode) ?? null)
    : null;

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
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [displayPath],
  );

  // Window-level key handler for actions that must work regardless of focus.
  // Sub-viewers/SearchBar stopPropagation when they consume Escape.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        safeCommand("close_window");
        e.preventDefault();
      } else if (e.key === "F3" && currentMode) {
        e.preventDefault();
        const resolved = autoMode ?? currentMode;
        safeCommand("set_viewer_mode", {
          mode: getAlternateMode(currentMode, resolved),
        });
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [currentMode, autoMode]);

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
      />
    );
  } else if (currentMode === "image") {
    content = (
      <ImageViewer
        filePath={displayPath}
        fileUrl={fileUrl}
        fileSize={info.size}
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

export default Viewer;
