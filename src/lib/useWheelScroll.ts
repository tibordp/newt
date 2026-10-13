import { useCallback, useEffect, useRef } from "react";
import { flushSync } from "react-dom";

/** Pixels per line for wheels that report `DOM_DELTA_LINE`. */
const LINE_PX = 40;

/**
 * What of `delta` to carry into the next frame: only a sub-pixel rest the
 * position rounded away. Distance the bounds or an edge held back is
 * dropped, so the view stops when the wheel does.
 */
function rest(delta: number, moved: number): number {
  const left = delta - moved;
  return Math.abs(left) < 1 ? left : 0;
}

/** A wheel event's deltas in pixels, whatever unit it reports them in. */
function wheelDelta(e: WheelEvent, el: HTMLElement): [number, number] {
  const unit =
    e.deltaMode === WheelEvent.DOM_DELTA_PAGE
      ? el.clientHeight
      : e.deltaMode === WheelEvent.DOM_DELTA_LINE
        ? LINE_PX
        : 1;
  return [e.deltaX * unit, e.deltaY * unit];
}

export interface WheelScrollOptions {
  /**
   * Re-derive what the view renders from the container's scroll position —
   * the work its `onScroll` handler does. Called with the position already
   * moved, and rendered synchronously, so rows and position reach the
   * screen in the same frame.
   */
  sync: (el: HTMLElement) => void;
  /**
   * The `scrollTop` range the view has rows for. Scrolling stops at its
   * edge; the wheel goes on from there once more rows arrive.
   */
  bounds?: (el: HTMLElement) => [number, number];
  /** Takes a wheel event before it scrolls; `true` consumes it. */
  claim?: (e: WheelEvent) => boolean;
}

/**
 * Wheel scrolling driven from script instead of the compositor. Native
 * wheel scrolling moves the content on its own thread, ahead of the
 * render that fills the newly exposed rows, so a virtualized view flashes
 * blank; here the wheel's delta becomes a position the view renders before
 * the frame is painted, as keyboard scrolling already does. The native
 * scrollbar stays the container's own.
 */
export function useWheelScroll(
  ref: React.RefObject<HTMLElement | null>,
  options: WheelScrollOptions,
): void {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const state = useRef({ dx: 0, dy: 0, frame: 0 });

  const apply = useCallback(() => {
    const s = state.current;
    s.frame = 0;
    const el = ref.current;
    if (!el) {
      s.dx = s.dy = 0;
      return;
    }
    const { sync, bounds } = optionsRef.current;
    const before = el.scrollTop;
    let top = before + s.dy;
    if (bounds) {
      // Only stop motion, never reverse it: the scrollbar can have left the
      // position outside the bounds.
      const [min, max] = bounds(el);
      if (s.dy > 0) top = Math.min(top, Math.max(before, max));
      if (s.dy < 0) top = Math.max(top, Math.min(before, min));
    }
    const left = el.scrollLeft;
    el.scrollLeft = left + s.dx;
    el.scrollTop = top;
    s.dx = rest(s.dx, el.scrollLeft - left);
    s.dy = rest(s.dy, el.scrollTop - before);
    flushSync(() => sync(el));
  }, [ref]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const s = state.current;
    const onWheel = (e: WheelEvent) => {
      // Ctrl+wheel and pinches zoom.
      if (e.ctrlKey || e.defaultPrevented) return;
      e.preventDefault();
      if (optionsRef.current.claim?.(e)) return;
      let [dx, dy] = wheelDelta(e, el);
      if (e.shiftKey && dx === 0) [dx, dy] = [dy, 0];
      s.dx += dx;
      s.dy += dy;
      if (!s.frame) s.frame = requestAnimationFrame(apply);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      el.removeEventListener("wheel", onWheel);
      cancelAnimationFrame(s.frame);
      s.frame = 0;
      s.dx = s.dy = 0;
    };
  }, [ref, apply]);
}
