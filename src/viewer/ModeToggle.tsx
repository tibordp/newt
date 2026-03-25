import styles from "./Viewer.module.scss";
import { safeCommand } from "../lib/ipc";
import type { ViewerMode } from "./helpers";

interface ModeToggleProps {
  currentMode: ViewerMode;
  autoMode: ViewerMode;
}

function capitalize(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** Get the alternate mode for F3 toggle */
export function getAlternateMode(
  currentMode: ViewerMode,
  autoMode: ViewerMode,
): ViewerMode {
  const other = autoMode === "hex" ? "text" : "hex";
  return currentMode === other
    ? autoMode === "hex"
      ? "hex"
      : autoMode
    : other;
}

export function ModeToggle({ currentMode, autoMode }: ModeToggleProps) {
  const modes: [ViewerMode, string][] =
    autoMode === "hex"
      ? [
          ["hex", "Hex"],
          ["text", "Text"],
        ]
      : [
          [autoMode, capitalize(autoMode)],
          ["hex", "Hex"],
        ];

  return (
    <span className={styles.modeToggle}>
      {modes.map(([mode, label]) => (
        <button
          key={mode}
          tabIndex={-1}
          className={`${styles.modeToggleBtn} ${mode === currentMode ? styles.modeToggleBtnActive : ""}`}
          onClick={() => safeCommand("set_viewer_mode", { mode })}
        >
          {label}
        </button>
      ))}
    </span>
  );
}
