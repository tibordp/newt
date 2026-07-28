import { useEffect, useState } from "react";

/// Returns `false` until the user actually moves the mouse, then flips
/// to `true` (and stays until the next open). Use the returned value to
/// gate pointer interactions on a freshly-opened menu / dialog so that
/// whatever item happens to sit under the cursor at open time doesn't get
/// hover-focused — which would override the keyboard's intended initial
/// selection.
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
    if (!open) {
      setActive(false);
      return;
    }
    const onMove = () => setActive(true);
    window.addEventListener("mousemove", onMove, { once: true, capture: true });
    return () =>
      window.removeEventListener("mousemove", onMove, { capture: true });
  }, [open]);
  return open && active;
}
