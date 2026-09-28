import React, { useEffect, useRef, useState } from "react";
import { useFormatBytes } from "../lib/size";

import styles from "./Viewer.module.scss";
import { type ViewerMode } from "./helpers";
import { ModeToggle } from "./ModeToggle";
import { useViewerHost } from "./host";

export interface MediaViewerProps {
  tag: "audio" | "video";
  filePath: string;
  fileUrl: string;
  fileSize: number;
  autoMode: ViewerMode;
}

export function MediaViewer({
  tag,
  filePath,
  fileUrl,
  fileSize,
  autoMode,
}: MediaViewerProps) {
  const formatSize = useFormatBytes();
  const viewerHost = useViewerHost();
  const viewerRef = useRef<HTMLDivElement>(null);
  const [mediaError, setMediaError] = useState<string | null>(null);

  useEffect(() => {
    if (!viewerHost.embedded) viewerRef.current?.focus();
  }, [viewerHost.embedded]);

  const modeName = tag === "audio" ? "Audio" : "Video";

  return (
    <div className={styles.viewer} ref={viewerRef} tabIndex={-1}>
      <div className={styles.mediaContent}>
        {mediaError ? (
          <div className={styles.imageErrorMessage} role="alert">
            Unable to play {modeName.toLowerCase()} preview: {mediaError}
          </div>
        ) : tag === "audio" ? (
          <audio
            className={styles.audioPlayer}
            aria-label={filePath}
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
            aria-label={filePath}
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
      <div
        className={styles.viewerStatus}
        onContextMenu={(e) => e.preventDefault()}
      >
        <span className={styles.statusText}>
          <span title={filePath}>{filePath}</span>
          <span className={styles.statusSeparator} aria-hidden>
            |
          </span>
          <span>{modeName}</span>
          <span className={styles.statusSeparator} aria-hidden>
            |
          </span>
          <span>{formatSize(fileSize)}</span>
        </span>
        <ModeToggle currentMode={tag} autoMode={autoMode} />
      </div>
    </div>
  );
}
