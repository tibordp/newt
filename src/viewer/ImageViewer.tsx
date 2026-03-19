import React, { useCallback, useEffect, useRef, useState } from "react";

import styles from "./Viewer.module.scss";
import { formatSize, type ViewerMode } from "./helpers";
import { ModeToggle } from "./ModeToggle";

export interface ImageViewerProps {
  filePath: string;
  fileUrl: string;
  fileSize: number;
  autoMode: ViewerMode;
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

export function ImageViewer({
  filePath,
  fileUrl,
  fileSize,
  autoMode,
}: ImageViewerProps) {
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

  const initImage = useCallback((img: HTMLImageElement) => {
    const container = containerRef.current;
    if (!container || img.naturalWidth === 0) return;
    const ns = { w: img.naturalWidth, h: img.naturalHeight };
    setNaturalSize(ns);
    const cw = container.clientWidth;
    const ch = container.clientHeight;
    const z = Math.min(cw / ns.w, ch / ns.h, 1);
    setZoom(z);
    setPan({ x: (cw - ns.w * z) / 2, y: (ch - ns.h * z) / 2 });
  }, []);

  const handleLoad = useCallback(
    (e: React.SyntheticEvent<HTMLImageElement>) => initImage(e.currentTarget),
    [initImage],
  );

  // Handle cached images whose load event fired before React attached onLoad
  useEffect(() => {
    const img = imgRef.current;
    if (img && img.complete) initImage(img);
  }, [initImage]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      switch (e.key) {
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
      <div
        className={styles.viewerStatus}
        onContextMenu={(e) => e.preventDefault()}
      >
        <span className={styles.statusText}>
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
        </span>
        <ModeToggle currentMode="image" autoMode={autoMode} />
      </div>
    </div>
  );
}
