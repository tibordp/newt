import styles from "./Viewer.module.scss";
import { safe } from "../lib/ipc";
import type { ViewerMode } from "./helpers";
import { useCommandShortcuts } from "../lib/scopedBindings";
import { useViewerHost } from "./host";

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
  const shortcuts = useCommandShortcuts();
  const viewerHost = useViewerHost();
  // Quick View picks the mode from its bar instead.
  if (viewerHost.embedded) return null;
  const other = counterpart(autoMode);
  const alternate = getAlternateMode(currentMode, autoMode);
  const modes: [ViewerMode, string][] = [
    [autoMode, capitalize(autoMode)],
    [other, capitalize(other)],
  ];

  return (
    <span
      className={styles.modeToggle}
      role="radiogroup"
      aria-label="View mode"
    >
      {modes.map(([mode, label]) => {
        const checked = mode === currentMode;
        return (
          <button
            key={mode}
            tabIndex={-1}
            role="radio"
            aria-checked={checked}
            className={`${styles.modeToggleBtn} ${checked ? styles.modeToggleBtnActive : ""}`}
            onClick={() => safe(viewerHost.setMode(mode))}
            title={
              checked
                ? undefined
                : mode === alternate
                  ? shortcuts.label(
                      `Switch to ${mode} view`,
                      "viewer_toggle_hex",
                    )
                  : `Switch to ${mode} view`
            }
          >
            {label}
          </button>
        );
      })}
    </span>
  );
}
