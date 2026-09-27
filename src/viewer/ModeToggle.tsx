import styles from "./Viewer.module.scss";
import { safe } from "../lib/ipc";
import type { ViewerMode } from "./helpers";
import { commands } from "../lib/bindings";

interface ModeToggleProps {
  currentMode: ViewerMode;
  autoMode: ViewerMode;
}

function capitalize(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** The mode the auto-detected one pairs with: the source text for the
 * rendered modes, hex for everything else, text for hex itself. */
function counterpart(autoMode: ViewerMode): ViewerMode {
  switch (autoMode) {
    case "hex":
    case "table":
    case "markdown":
      return "text";
    default:
      return "hex";
  }
}

/** The mode F3 toggles to from the current one. */
export function getAlternateMode(
  currentMode: ViewerMode,
  autoMode: ViewerMode,
): ViewerMode {
  const other = counterpart(autoMode);
  return currentMode === other ? autoMode : other;
}

export function ModeToggle({ currentMode, autoMode }: ModeToggleProps) {
  const other = counterpart(autoMode);
  const modes: [ViewerMode, string][] = [
    [autoMode, capitalize(autoMode)],
    [other, capitalize(other)],
  ];

  return (
    <span className={styles.modeToggle}>
      {modes.map(([mode, label]) => (
        <button
          key={mode}
          tabIndex={-1}
          className={`${styles.modeToggleBtn} ${mode === currentMode ? styles.modeToggleBtnActive : ""}`}
          onClick={() => safe(commands.setViewerMode(mode))}
        >
          {label}
        </button>
      ))}
    </span>
  );
}
