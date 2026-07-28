import React, { useCallback, useEffect, useRef, useState } from "react";
import * as CM from "@radix-ui/react-context-menu";
import * as DM from "@radix-ui/react-dropdown-menu";
import { message } from "@tauri-apps/plugin-dialog";

import { useFormatBytes } from "../lib/size";
import { commands, type ImageBackground } from "../lib/bindings";
import { safe, unwrap } from "../lib/ipc";
import { usePreferences } from "../lib/preferences";
import { useCommandShortcuts, useScopedBindings } from "../lib/scopedBindings";
import type { VfsPath } from "../lib/types";
import menuStyles from "../main_window/Menu.module.scss";

import styles from "./Viewer.module.scss";
import { type ViewerMode, type ExifRow } from "./helpers";
import {
  IconChecker,
  IconFlipH,
  IconFlipV,
  IconInfo,
  IconMinus,
  IconPlus,
  IconRotateCcw,
  IconRotateCw,
} from "./icons";
import { ModeToggle } from "./ModeToggle";

export interface ImageViewerProps {
  filePath: string;
  vfsPath: VfsPath;
  fileUrl: string;
  fileSize: number;
  mimeType: string | null;
  autoMode: ViewerMode;
}

// Zoom is a CSS-pixel scale factor, but the user-facing notion of "100%" is
// one image pixel per physical display pixel, so every user-visible zoom
// quantity (indicator, limits, actual-size) is scaled by devicePixelRatio.
const MAX_DEVICE_ZOOM = 50;
const KEY_ZOOM_FACTOR = 1.25;
const WHEEL_ZOOM_FACTOR = 1 / 0.9;
const PAN_STEP = 75;
const ZOOM_PRESETS = [25, 50, 100, 200, 400, 800];

type Rotation = 0 | 90 | 180 | 270;

const BACKGROUNDS: { id: ImageBackground; label: string; cls: string }[] = [
  { id: "dark", label: "Dark", cls: "bgDark" },
  { id: "checkerboard", label: "Checkerboard", cls: "bgChecker" },
  { id: "light", label: "Light", cls: "bgLight" },
];

interface Size {
  w: number;
  h: number;
}

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

// Displayed bounding box of the (possibly rotated) image at zoom 1
function dispSize(ns: Size, rotation: Rotation): Size {
  return rotation % 180 === 0 ? ns : { w: ns.h, h: ns.w };
}

function fitZoom(size: Size, cw: number, ch: number) {
  return Math.min(cw / size.w, ch / size.h, 1 / window.devicePixelRatio);
}

function clampView(
  z: number,
  px: number,
  py: number,
  size: Size,
  cw: number,
  ch: number,
) {
  const zoom = Math.min(
    Math.max(z, fitZoom(size, cw, ch)),
    MAX_DEVICE_ZOOM / window.devicePixelRatio,
  );
  const imgW = size.w * zoom;
  const imgH = size.h * zoom;
  return {
    zoom,
    pan: {
      x: imgW <= cw ? (cw - imgW) / 2 : Math.min(0, Math.max(cw - imgW, px)),
      y: imgH <= ch ? (ch - imgH) / 2 : Math.min(0, Math.max(ch - imgH, py)),
    },
  };
}

// Maps the natural image box (at origin) onto the rotated bounding box (at origin)
function rotationTransform(ns: Size, rotation: Rotation): string {
  switch (rotation) {
    case 0:
      return "";
    case 90:
      return `translate(${ns.h}px, 0) rotate(90deg)`;
    case 180:
      return `translate(${ns.w}px, ${ns.h}px) rotate(180deg)`;
    case 270:
      return `translate(0, ${ns.w}px) rotate(270deg)`;
  }
}

// Same mapping as rotationTransform, as canvas context operations
function applyRotationToCtx(
  ctx: CanvasRenderingContext2D,
  ns: Size,
  rotation: Rotation,
) {
  switch (rotation) {
    case 90:
      ctx.translate(ns.h, 0);
      ctx.rotate(Math.PI / 2);
      break;
    case 180:
      ctx.translate(ns.w, ns.h);
      ctx.rotate(Math.PI);
      break;
    case 270:
      ctx.translate(0, ns.w);
      ctx.rotate((3 * Math.PI) / 2);
      break;
  }
}

type DragState =
  | { kind: "pan"; x: number; y: number; panX: number; panY: number }
  | { kind: "select"; anchorX: number; anchorY: number; moved: boolean };

export function ImageViewer({
  filePath,
  vfsPath,
  fileUrl,
  fileSize,
  mimeType,
  autoMode,
}: ImageViewerProps) {
  const formatSize = useFormatBytes();
  const preferences = usePreferences();
  const shortcuts = useCommandShortcuts();
  const viewerRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [naturalSize, setNaturalSize] = useState<Size | null>(null);
  const [rotation, setRotation] = useState<Rotation>(0);
  const [flipH, setFlipH] = useState(false);
  const [flipV, setFlipV] = useState(false);
  const [sel, setSel] = useState<Rect | null>(null);
  const [panning, setPanning] = useState(false);
  const [infoOpen, setInfoOpen] = useState(false);
  const [exifRows, setExifRows] = useState<ExifRow[] | null>(null);
  const [dpr, setDpr] = useState(window.devicePixelRatio);
  const [imageError, setImageError] = useState(false);
  const drag = useRef<DragState | null>(null);

  const background =
    preferences?.settings.viewer?.image_background ?? "checkerboard";
  const setBackground = useCallback((bg: ImageBackground) => {
    safe(commands.updatePreference("viewer.image_background", bg));
  }, []);

  // Keep a ref to latest state so native event listeners can read it
  const stateRef = useRef({
    zoom,
    pan,
    naturalSize,
    rotation,
    flipH,
    flipV,
    sel,
  });
  stateRef.current = { zoom, pan, naturalSize, rotation, flipH, flipV, sel };

  useEffect(() => {
    viewerRef.current?.focus();
  }, []);

  const applyView = useCallback((z: number, px: number, py: number) => {
    const container = containerRef.current;
    const { naturalSize: ns, rotation: rot } = stateRef.current;
    if (!ns || !container) return;
    const v = clampView(
      z,
      px,
      py,
      dispSize(ns, rot),
      container.clientWidth,
      container.clientHeight,
    );
    setZoom(v.zoom);
    setPan(v.pan);
  }, []);

  // Zoom to `z`, keeping the image point at container coords (cx, cy) fixed
  const zoomAt = useCallback(
    (cx: number, cy: number, z: number) => {
      const { zoom: cur, pan: curPan } = stateRef.current;
      const imgX = (cx - curPan.x) / cur;
      const imgY = (cy - curPan.y) / cur;
      applyView(z, cx - imgX * z, cy - imgY * z);
    },
    [applyView],
  );

  const zoomAtCenter = useCallback(
    (z: number) => {
      const container = containerRef.current;
      if (!container) return;
      zoomAt(container.clientWidth / 2, container.clientHeight / 2, z);
    },
    [zoomAt],
  );

  // Re-clamp on container resize (window resize, info panel toggle)
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver(() => {
      const { zoom: z, pan: p } = stateRef.current;
      applyView(z, p.x, p.y);
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, [applyView]);

  // Track devicePixelRatio (window moving between monitors) and re-clamp,
  // since the zoom limits are expressed in device pixels
  useEffect(() => {
    const mq = window.matchMedia(`(resolution: ${dpr}dppx)`);
    const handler = () => {
      setDpr(window.devicePixelRatio);
      const { zoom: z, pan: p } = stateRef.current;
      applyView(z, p.x, p.y);
    };
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, [dpr, applyView]);

  const resetView = useCallback(() => {
    const container = containerRef.current;
    const { naturalSize: ns, rotation: rot } = stateRef.current;
    if (!ns || !container) return;
    applyView(
      fitZoom(dispSize(ns, rot), container.clientWidth, container.clientHeight),
      0,
      0,
    );
  }, [applyView]);

  const rotate = useCallback((delta: 90 | -90) => {
    const container = containerRef.current;
    const { zoom: z, naturalSize: ns, rotation: rot } = stateRef.current;
    if (!ns || !container) return;
    const cw = container.clientWidth;
    const ch = container.clientHeight;
    const wasFit = z <= fitZoom(dispSize(ns, rot), cw, ch) + 1e-9;
    const next = ((((rot + delta) % 360) + 360) % 360) as Rotation;
    setRotation(next);
    setSel(null);
    const size = dispSize(ns, next);
    const nz = wasFit ? fitZoom(size, cw, ch) : z;
    const v = clampView(
      nz,
      (cw - size.w * nz) / 2,
      (ch - size.h * nz) / 2,
      size,
      cw,
      ch,
    );
    setZoom(v.zoom);
    setPan(v.pan);
  }, []);

  const toggleFlipH = useCallback(() => {
    setFlipH((f) => !f);
    setSel(null);
  }, []);
  const toggleFlipV = useCallback(() => {
    setFlipV((f) => !f);
    setSel(null);
  }, []);

  const initImage = useCallback((img: HTMLImageElement) => {
    const container = containerRef.current;
    if (!container || img.naturalWidth === 0) return;
    const ns = { w: img.naturalWidth, h: img.naturalHeight };
    setNaturalSize(ns);
    const cw = container.clientWidth;
    const ch = container.clientHeight;
    const z = fitZoom(ns, cw, ch);
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

  // Copy the selection (or the whole image) to the clipboard as PNG, with
  // rotation/flips baked in — what you see is what you copy. The ClipboardItem
  // is constructed with a pending promise: WebKit requires clipboard.write to
  // be called synchronously within the user gesture.
  const copyToClipboard = useCallback(async (region: Rect | null) => {
    const img = imgRef.current;
    const {
      naturalSize: ns,
      rotation: rot,
      flipH: fh,
      flipV: fv,
    } = stateRef.current;
    if (!img || !ns) return;
    const size = dispSize(ns, rot);
    const r = region ?? { x: 0, y: 0, w: size.w, h: size.h };

    const blobPromise = new Promise<Blob>((resolve, reject) => {
      try {
        const canvas = document.createElement("canvas");
        canvas.width = r.w;
        canvas.height = r.h;
        const ctx = canvas.getContext("2d");
        if (!ctx) throw new Error("canvas 2d context unavailable");
        ctx.translate(-r.x, -r.y);
        if (fh || fv) {
          ctx.translate(fh ? size.w : 0, fv ? size.h : 0);
          ctx.scale(fh ? -1 : 1, fv ? -1 : 1);
        }
        applyRotationToCtx(ctx, ns, rot);
        ctx.drawImage(img, 0, 0);
        canvas.toBlob(
          (b) => (b ? resolve(b) : reject(new Error("PNG encoding failed"))),
          "image/png",
        );
      } catch (e) {
        reject(e);
      }
    });

    try {
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": blobPromise }),
      ]);
    } catch (e) {
      await message(`Failed to copy image: ${e}`, {
        kind: "error",
        title: "Error",
      });
    }
  }, []);

  const selectAll = useCallback(() => {
    const { naturalSize: ns, rotation: rot } = stateRef.current;
    if (!ns) return;
    const size = dispSize(ns, rot);
    setSel({ x: 0, y: 0, w: size.w, h: size.h });
  }, []);

  const cycleBackground = useCallback(() => {
    const idx = BACKGROUNDS.findIndex((b) => b.id === background);
    setBackground(BACKGROUNDS[(idx + 1) % BACKGROUNDS.length].id);
  }, [background, setBackground]);

  // Fundamental keys stay intrinsic: Escape layers selection-clear over the
  // window-level close, arrows pan. Everything else dispatches through the
  // keybinding registry below.
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Escape") {
        if (stateRef.current.sel) {
          setSel(null);
          e.preventDefault();
          e.stopPropagation(); // keep the window-level handler from closing the viewer
        }
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const { zoom: z } = stateRef.current;
      switch (e.key) {
        case "ArrowLeft":
          applyView(
            z,
            stateRef.current.pan.x + PAN_STEP,
            stateRef.current.pan.y,
          );
          break;
        case "ArrowRight":
          applyView(
            z,
            stateRef.current.pan.x - PAN_STEP,
            stateRef.current.pan.y,
          );
          break;
        case "ArrowUp":
          applyView(
            z,
            stateRef.current.pan.x,
            stateRef.current.pan.y + PAN_STEP,
          );
          break;
        case "ArrowDown":
          applyView(
            z,
            stateRef.current.pan.x,
            stateRef.current.pan.y - PAN_STEP,
          );
          break;
        default:
          return;
      }
      e.preventDefault();
    },
    [applyView],
  );

  useScopedBindings("viewer", {
    viewer_zoom_in: () => zoomAtCenter(stateRef.current.zoom * KEY_ZOOM_FACTOR),
    viewer_zoom_out: () =>
      zoomAtCenter(stateRef.current.zoom / KEY_ZOOM_FACTOR),
    viewer_zoom_fit: resetView,
    viewer_zoom_actual: () => zoomAtCenter(1 / window.devicePixelRatio),
    viewer_rotate_cw: () => rotate(90),
    viewer_rotate_ccw: () => rotate(-90),
    viewer_flip_horizontal: toggleFlipH,
    viewer_flip_vertical: toggleFlipV,
    viewer_cycle_background: cycleBackground,
    viewer_toggle_info: () => setInfoOpen((v) => !v),
    viewer_select_all: selectAll,
    viewer_copy: () => {
      // A text selection in the info panel takes priority — yield to the
      // webview's native copy
      if (window.getSelection()?.toString()) return false;
      void copyToClipboard(stateRef.current.sel);
    },
  });

  // Non-passive wheel listener so we can preventDefault (React wheel events are passive)
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handler = (e: WheelEvent) => {
      const {
        zoom: curZoom,
        naturalSize: ns,
        rotation: rot,
      } = stateRef.current;
      if (!ns) return;

      const rect = container.getBoundingClientRect();
      const size = dispSize(ns, rot);
      const minZoom = fitZoom(size, rect.width, rect.height);
      const maxZoom = MAX_DEVICE_ZOOM / window.devicePixelRatio;

      // If already at the limit in the scroll direction, let the event pass through
      const zoomingOut = e.deltaY > 0;
      if (
        (zoomingOut && curZoom <= minZoom) ||
        (!zoomingOut && curZoom >= maxZoom)
      )
        return;

      e.preventDefault();
      const factor = zoomingOut ? 1 / WHEEL_ZOOM_FACTOR : WHEEL_ZOOM_FACTOR;
      zoomAt(e.clientX - rect.left, e.clientY - rect.top, curZoom * factor);
    };

    container.addEventListener("wheel", handler, { passive: false });
    return () => container.removeEventListener("wheel", handler);
  }, [zoomAt]);

  // Trackpad pinch (macOS WebKit delivers it as non-standard gesture events)
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let startZoom = 0;
    const onStart = (e: Event) => {
      startZoom = stateRef.current.zoom;
      e.preventDefault();
    };
    const onChange = (e: Event) => {
      if (!startZoom) return;
      e.preventDefault();
      const g = e as unknown as {
        scale: number;
        clientX: number;
        clientY: number;
      };
      const rect = container.getBoundingClientRect();
      zoomAt(g.clientX - rect.left, g.clientY - rect.top, startZoom * g.scale);
    };
    const onEnd = (e: Event) => {
      startZoom = 0;
      e.preventDefault();
    };
    container.addEventListener("gesturestart", onStart);
    container.addEventListener("gesturechange", onChange);
    container.addEventListener("gestureend", onEnd);
    return () => {
      container.removeEventListener("gesturestart", onStart);
      container.removeEventListener("gesturechange", onChange);
      container.removeEventListener("gestureend", onEnd);
    };
  }, [zoomAt]);

  // Container coords → image (display-space) coords
  const imagePoint = useCallback((clientX: number, clientY: number) => {
    const container = containerRef.current!;
    const rect = container.getBoundingClientRect();
    const { zoom: z, pan: p } = stateRef.current;
    return {
      x: (clientX - rect.left - p.x) / z,
      y: (clientY - rect.top - p.y) / z,
    };
  }, []);

  // Left drag draws a selection; middle drag or modifier+left drag pans
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    const container = containerRef.current;
    const {
      zoom: z,
      pan: curPan,
      naturalSize: ns,
      rotation: rot,
    } = stateRef.current;
    if (!ns || !container) return;
    const panDrag =
      e.button === 1 || (e.button === 0 && (e.shiftKey || e.altKey));
    if (panDrag) {
      const size = dispSize(ns, rot);
      // Nothing to pan if the image fits entirely within the container
      if (
        size.w * z <= container.clientWidth &&
        size.h * z <= container.clientHeight
      )
        return;
      e.preventDefault();
      drag.current = {
        kind: "pan",
        x: e.clientX,
        y: e.clientY,
        panX: curPan.x,
        panY: curPan.y,
      };
      setPanning(true);
    } else if (e.button === 0) {
      e.preventDefault();
      drag.current = {
        kind: "select",
        anchorX: e.clientX,
        anchorY: e.clientY,
        moved: false,
      };
    }
  }, []);

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      const d = drag.current;
      if (!d) return;
      if (d.kind === "pan") {
        applyView(
          stateRef.current.zoom,
          d.panX + (e.clientX - d.x),
          d.panY + (e.clientY - d.y),
        );
        return;
      }
      const { naturalSize: ns, rotation: rot } = stateRef.current;
      if (!ns) return;
      if (
        !d.moved &&
        Math.hypot(e.clientX - d.anchorX, e.clientY - d.anchorY) < 3
      )
        return;
      d.moved = true;
      const size = dispSize(ns, rot);
      const a = imagePoint(d.anchorX, d.anchorY);
      const b = imagePoint(e.clientX, e.clientY);
      const x0 = Math.max(0, Math.floor(Math.min(a.x, b.x)));
      const y0 = Math.max(0, Math.floor(Math.min(a.y, b.y)));
      const x1 = Math.min(size.w, Math.ceil(Math.max(a.x, b.x)));
      const y1 = Math.min(size.h, Math.ceil(Math.max(a.y, b.y)));
      setSel(
        x1 - x0 >= 1 && y1 - y0 >= 1
          ? { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
          : null,
      );
    };
    const handleMouseUp = () => {
      const d = drag.current;
      if (d?.kind === "select" && !d.moved) setSel(null); // click clears
      drag.current = null;
      setPanning(false);
    };
    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
    return () => {
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };
  }, [applyView, imagePoint]);

  const handleDoubleClick = useCallback(
    (e: React.MouseEvent) => {
      const container = containerRef.current;
      if (!container || !stateRef.current.naturalSize) return;
      const actualSize = 1 / window.devicePixelRatio;
      if (Math.abs(stateRef.current.zoom - actualSize) > 1e-3) {
        const rect = container.getBoundingClientRect();
        zoomAt(e.clientX - rect.left, e.clientY - rect.top, actualSize);
      } else {
        resetView();
      }
    },
    [zoomAt, resetView],
  );

  // Lazily fetch EXIF the first time the info panel opens
  useEffect(() => {
    if (!infoOpen || exifRows !== null) return;
    (async () => {
      try {
        setExifRows(await unwrap(commands.imageExif(vfsPath)));
      } catch (e) {
        console.error("Failed to read EXIF", e);
        setExifRows([]);
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [infoOpen]);

  const refocusViewer = useCallback((e: Event) => {
    e.preventDefault();
    viewerRef.current?.focus();
  }, []);

  const zoomPercent = Math.round(zoom * dpr * 100);
  const size = naturalSize && dispSize(naturalSize, rotation);
  const orientation = [
    rotation !== 0 ? `${rotation}°` : null,
    flipH ? "flip H" : null,
    flipV ? "flip V" : null,
  ]
    .filter(Boolean)
    .join(" ");

  const flipTransform =
    size && (flipH || flipV)
      ? `translate(${flipH ? size.w : 0}px, ${flipV ? size.h : 0}px) scale(${flipH ? -1 : 1}, ${flipV ? -1 : 1})`
      : "";

  const backgroundCls =
    BACKGROUNDS.find((b) => b.id === background)?.cls ?? "bgChecker";

  const toolbar = (
    <div
      className={styles.viewerToolbar}
      onMouseDown={(e) => e.preventDefault()}
    >
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn}`}
        onClick={() => zoomAtCenter(stateRef.current.zoom / KEY_ZOOM_FACTOR)}
        title={shortcuts.label("Zoom out", "viewer_zoom_out")}
      >
        <IconMinus />
      </button>
      <DM.Root>
        <DM.Trigger asChild>
          <button
            className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarZoom}`}
            title="Zoom presets"
          >
            {zoomPercent}% ▾
          </button>
        </DM.Trigger>
        <DM.Portal>
          <DM.Content
            className={menuStyles.content}
            loop
            onCloseAutoFocus={refocusViewer}
          >
            <DM.Item className={menuStyles.item} onSelect={resetView}>
              Fit
              {shortcuts.get("viewer_zoom_fit") && (
                <span className={menuStyles.shortcut}>
                  {shortcuts.get("viewer_zoom_fit")}
                </span>
              )}
            </DM.Item>
            {ZOOM_PRESETS.map((p) => (
              <DM.Item
                key={p}
                className={menuStyles.item}
                onSelect={() => zoomAtCenter(p / 100 / window.devicePixelRatio)}
              >
                {p}%
                {p === 100 && shortcuts.get("viewer_zoom_actual") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_zoom_actual")}
                  </span>
                )}
              </DM.Item>
            ))}
          </DM.Content>
        </DM.Portal>
      </DM.Root>
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn}`}
        onClick={() => zoomAtCenter(stateRef.current.zoom * KEY_ZOOM_FACTOR)}
        title={shortcuts.label("Zoom in", "viewer_zoom_in")}
      >
        <IconPlus />
      </button>
      <span className={styles.viewerToolbarSep} />
      <button
        className={styles.viewerToolbarBtn}
        onClick={resetView}
        title={shortcuts.label("Fit to window", "viewer_zoom_fit")}
      >
        Fit
      </button>
      <button
        className={styles.viewerToolbarBtn}
        onClick={() => zoomAtCenter(1 / window.devicePixelRatio)}
        title={shortcuts.label("Actual size", "viewer_zoom_actual")}
      >
        1:1
      </button>
      <span className={styles.viewerToolbarSep} />
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn}`}
        onClick={() => rotate(-90)}
        title={shortcuts.label("Rotate counter-clockwise", "viewer_rotate_ccw")}
      >
        <IconRotateCcw />
      </button>
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn}`}
        onClick={() => rotate(90)}
        title={shortcuts.label("Rotate clockwise", "viewer_rotate_cw")}
      >
        <IconRotateCw />
      </button>
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn} ${flipH ? styles.viewerToolbarBtnActive : ""}`}
        onClick={toggleFlipH}
        title={shortcuts.label("Flip horizontal", "viewer_flip_horizontal")}
      >
        <IconFlipH />
      </button>
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn} ${flipV ? styles.viewerToolbarBtnActive : ""}`}
        onClick={toggleFlipV}
        title={shortcuts.label("Flip vertical", "viewer_flip_vertical")}
      >
        <IconFlipV />
      </button>
      <span className={styles.viewerToolbarSep} />
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn}`}
        onClick={cycleBackground}
        title={shortcuts.label("Cycle background", "viewer_cycle_background")}
      >
        <IconChecker />
      </button>
      <button
        className={`${styles.viewerToolbarBtn} ${styles.viewerToolbarIconBtn} ${infoOpen ? styles.viewerToolbarBtnActive : ""}`}
        onClick={() => setInfoOpen((v) => !v)}
        title={shortcuts.label("Image info", "viewer_toggle_info")}
      >
        <IconInfo />
      </button>
    </div>
  );

  const infoPanel = infoOpen && (
    <div className={styles.imageInfoPanel}>
      <div className={styles.imageInfoHeader}>Image</div>
      <dl className={styles.imageInfoList}>
        {naturalSize && (
          <>
            <div className={styles.imageInfoRow}>
              <dt>Dimensions</dt>
              <dd>
                {naturalSize.w} × {naturalSize.h}
              </dd>
            </div>
            <div className={styles.imageInfoRow}>
              <dt>Megapixels</dt>
              <dd>{((naturalSize.w * naturalSize.h) / 1e6).toFixed(1)} MP</dd>
            </div>
          </>
        )}
        {mimeType && (
          <div className={styles.imageInfoRow}>
            <dt>Type</dt>
            <dd>{mimeType}</dd>
          </div>
        )}
        <div className={styles.imageInfoRow}>
          <dt>File size</dt>
          <dd>{formatSize(fileSize)}</dd>
        </div>
      </dl>
      <div className={styles.imageInfoHeader}>EXIF</div>
      {exifRows === null ? (
        <div className={styles.imageInfoStatus}>Loading…</div>
      ) : exifRows.length === 0 ? (
        <div className={styles.imageInfoStatus}>No EXIF metadata</div>
      ) : (
        <dl className={styles.imageInfoList}>
          {exifRows.map((row) => (
            <div className={styles.imageInfoRow} key={row.label}>
              <dt>{row.label}</dt>
              <dd>{row.value}</dd>
            </div>
          ))}
        </dl>
      )}
    </div>
  );

  return (
    <div
      className={styles.viewer}
      ref={viewerRef}
      tabIndex={-1}
      onKeyDown={handleKeyDown}
    >
      {toolbar}
      <div className={styles.imageMain}>
        <CM.Root>
          <CM.Trigger asChild>
            <div
              className={`${styles.imageContent} ${styles[backgroundCls]} ${panning ? styles.imageContentPanning : ""}`}
              ref={containerRef}
              onMouseDown={handleMouseDown}
              onDoubleClick={handleDoubleClick}
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
                  crossOrigin="anonymous"
                  onLoad={handleLoad}
                  onError={() => setImageError(true)}
                  draggable={false}
                  style={{
                    transform: [
                      `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
                      flipTransform,
                      naturalSize
                        ? rotationTransform(naturalSize, rotation)
                        : "",
                    ]
                      .filter(Boolean)
                      .join(" "),
                    transformOrigin: "0 0",
                    // Nearest-neighbor only when magnified past 1:1 device
                    // pixels; below that the image is downscaled and should
                    // be resampled
                    imageRendering: zoom * dpr > 1.001 ? "pixelated" : "auto",
                  }}
                />
              )}
              {sel && (
                <div
                  className={styles.marchingAnts}
                  style={{
                    left: pan.x + sel.x * zoom,
                    top: pan.y + sel.y * zoom,
                    width: sel.w * zoom,
                    height: sel.h * zoom,
                  }}
                />
              )}
            </div>
          </CM.Trigger>
          <CM.Portal>
            <CM.Content
              className={menuStyles.content}
              loop
              onCloseAutoFocus={refocusViewer}
            >
              <CM.Item
                className={menuStyles.item}
                disabled={!sel}
                onSelect={() => void copyToClipboard(stateRef.current.sel)}
              >
                Copy Selection
                {shortcuts.get("viewer_copy") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_copy")}
                  </span>
                )}
              </CM.Item>
              <CM.Item
                className={menuStyles.item}
                onSelect={() => void copyToClipboard(null)}
              >
                Copy Image
              </CM.Item>
              <CM.Separator className={menuStyles.separator} />
              <CM.Item className={menuStyles.item} onSelect={resetView}>
                Fit to Window
                {shortcuts.get("viewer_zoom_fit") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_zoom_fit")}
                  </span>
                )}
              </CM.Item>
              <CM.Item
                className={menuStyles.item}
                onSelect={() => zoomAtCenter(1 / window.devicePixelRatio)}
              >
                Actual Size
                {shortcuts.get("viewer_zoom_actual") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_zoom_actual")}
                  </span>
                )}
              </CM.Item>
              <CM.Separator className={menuStyles.separator} />
              <CM.Item className={menuStyles.item} onSelect={() => rotate(90)}>
                Rotate Clockwise
                {shortcuts.get("viewer_rotate_cw") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_rotate_cw")}
                  </span>
                )}
              </CM.Item>
              <CM.Item className={menuStyles.item} onSelect={() => rotate(-90)}>
                Rotate Counter-Clockwise
                {shortcuts.get("viewer_rotate_ccw") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_rotate_ccw")}
                  </span>
                )}
              </CM.Item>
              <CM.Item className={menuStyles.item} onSelect={toggleFlipH}>
                Flip Horizontal
                {shortcuts.get("viewer_flip_horizontal") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_flip_horizontal")}
                  </span>
                )}
              </CM.Item>
              <CM.Item className={menuStyles.item} onSelect={toggleFlipV}>
                Flip Vertical
                {shortcuts.get("viewer_flip_vertical") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_flip_vertical")}
                  </span>
                )}
              </CM.Item>
              <CM.Separator className={menuStyles.separator} />
              <CM.Sub>
                <CM.SubTrigger className={menuStyles.item}>
                  Background
                  <span className={menuStyles.shortcut}>›</span>
                </CM.SubTrigger>
                <CM.Portal>
                  <CM.SubContent className={menuStyles.content} loop>
                    <CM.RadioGroup
                      value={background}
                      onValueChange={(v) => setBackground(v as ImageBackground)}
                    >
                      {BACKGROUNDS.map((b) => (
                        <CM.RadioItem
                          key={b.id}
                          value={b.id}
                          className={menuStyles.item}
                          onSelect={(e) => e.preventDefault()}
                        >
                          <span className={menuStyles.checkColumn}>
                            <CM.ItemIndicator>•</CM.ItemIndicator>
                          </span>
                          {b.label}
                        </CM.RadioItem>
                      ))}
                    </CM.RadioGroup>
                  </CM.SubContent>
                </CM.Portal>
              </CM.Sub>
              <CM.CheckboxItem
                className={menuStyles.item}
                checked={infoOpen}
                onCheckedChange={setInfoOpen}
              >
                <span className={menuStyles.checkColumn}>
                  <CM.ItemIndicator>✓</CM.ItemIndicator>
                </span>
                Image Info
                {shortcuts.get("viewer_toggle_info") && (
                  <span className={menuStyles.shortcut}>
                    {shortcuts.get("viewer_toggle_info")}
                  </span>
                )}
              </CM.CheckboxItem>
            </CM.Content>
          </CM.Portal>
        </CM.Root>
        {infoPanel}
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
          {orientation && (
            <>
              <span className={styles.statusSeparator}>|</span>
              <span>{orientation}</span>
            </>
          )}
          {sel && (
            <>
              <span className={styles.statusSeparator}>|</span>
              <span>
                Sel: {sel.w} × {sel.h}
              </span>
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
