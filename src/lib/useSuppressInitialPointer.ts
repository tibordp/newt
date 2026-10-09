import { useEffect, useState } from "react";

const MODIFIERS = new Set(["Shift", "Control", "Alt", "Meta"]);

/// Returns `false` until the user actually moves the mouse, and again after
/// every key press until they move it once more. Use the returned value to
/// gate pointer interactions on a menu / dialog driven by both keyboard and
/// mouse, so that whatever item sits under a resting cursor doesn't get
/// hover-focused and override the keyboard's selection: neither when the
/// menu opens under it, nor when the keyboard scrolls the list (or typing
/// re-renders it) beneath it.
///
/// Only a mousemove whose position changed counts: WebKit and Chromium
/// send one after a scroll or layout change under a resting cursor, to
/// update hover. The wheel counts too. Modifier keys don't take the
/// pointer away, so Shift+click still lands.
///
/// Callers that stay mounted while closed (a menu whose Radix anchor lives
/// in the pane) MUST pass their `open` flag: the arming is per-open, and
/// without it the first mouse move of the session disarms the hook for
/// good. Callers mounted only while open can omit it.
///
/// Typical usage: spread `{ style: { pointerEvents: active ? undefined : "none" } }`
/// onto the list container. CSS-level suppression works for libraries
/// (cmdk, …) whose hover handlers we can't intercept directly.
export function useSuppressInitialPointer(open = true): boolean {
  const [active, setActive] = useState(false);
  useEffect(() => {
    setActive(false);
    if (!open) return;
    let last: { x: number; y: number } | null = null;
    const onMove = (e: MouseEvent) => {
      const moved =
        e.movementX !== 0 ||
        e.movementY !== 0 ||
        (last !== null && (e.clientX !== last.x || e.clientY !== last.y));
      last = { x: e.clientX, y: e.clientY };
      if (moved) setActive(true);
    };
    const onWheel = () => setActive(true);
    const onKey = (e: KeyboardEvent) => {
      if (!MODIFIERS.has(e.key)) setActive(false);
    };
    const capture = { capture: true };
    window.addEventListener("mousemove", onMove, capture);
    window.addEventListener("wheel", onWheel, { capture: true, passive: true });
    window.addEventListener("keydown", onKey, capture);
    return () => {
      window.removeEventListener("mousemove", onMove, capture);
      window.removeEventListener("wheel", onWheel, capture);
      window.removeEventListener("keydown", onKey, capture);
    };
  }, [open]);
  return open && active;
}
