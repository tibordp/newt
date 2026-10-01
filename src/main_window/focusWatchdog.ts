import { useEffect } from "react";
import { REFOCUS_EVENT } from "./types";

// Handing focus back this many times in a row without it sticking means an
// owner that won't take it; stop rather than spin.
const MAX_HANDS = 5;

/// `<body>` is never a resting place for focus in the main window: when
/// focus lands there (an overlay that closed without returning it, a
/// focused element that unmounted), it goes back to its owner — the active
/// pane or terminal. A safety net under the menus' and dialogs' own focus
/// handling, which development builds report so the gap gets fixed.
export function useFocusWatchdog() {
  useEffect(() => {
    let frame = 0;
    let hands = 0;
    const check = () => {
      frame = 0;
      if (!document.hasFocus()) return;
      const active = document.activeElement;
      if (active && active !== document.body) {
        hands = 0;
        return;
      }
      // A dialog traps focus itself, and its closing is followed by the
      // owner taking focus back (see CLAUDE.md, "Dialog close").
      if (document.querySelector('[role="dialog"], [role="alertdialog"]')) {
        return;
      }
      if (hands >= MAX_HANDS) return;
      hands += 1;
      if (import.meta.env.DEV) {
        console.warn("focus fell to <body>; returning it to its owner");
      }
      window.dispatchEvent(new Event(REFOCUS_EVENT));
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(check);
    };
    const onWindowBlur = () => {
      hands = 0;
    };
    document.addEventListener("focusout", schedule);
    window.addEventListener("focus", schedule);
    window.addEventListener("blur", onWindowBlur);
    return () => {
      if (frame) cancelAnimationFrame(frame);
      document.removeEventListener("focusout", schedule);
      window.removeEventListener("focus", schedule);
      window.removeEventListener("blur", onWindowBlur);
    };
  }, []);
}
