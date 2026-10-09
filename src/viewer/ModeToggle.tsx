import styles from "./Viewer.module.scss";
import { safe } from "../lib/ipc";
import type { ViewerMode } from "./helpers";
import { usePreferences } from "../lib/preferences";
import { useCommandShortcuts } from "../lib/scopedBindings";
import { useViewerHost } from "./host";

interface ModeToggleProps {
  currentMode: ViewerMode;
  autoMode: ViewerMode;
}

const LABELS: Partial<Record<ViewerMode, string>> = {
  dissect: "Structure",
  pdf: "PDF",
};

function modeLabel(mode: ViewerMode): string {
  return LABELS[mode] ?? mode.charAt(0).toUpperCase() + mode.slice(1);
}

/** The mode the auto-detected one pairs with: the source text for the
 * rendered modes and for hex, the `viewer.alternate_mode` preference for
 * everything else (hex when that's the detected mode itself). */
export function useCounterpart(autoMode: ViewerMode): ViewerMode {
  const preferred = usePreferences()?.settings.viewer?.alternate_mode;
  switch (autoMode) {
    case "hex":
    case "table":
    case "markdown":
      return "text";
    default: {
      const other = preferred ?? "dissect";
      return autoMode === other ? "hex" : other;
    }
  }
}

/** The mode F3 toggles to from the current one. */
export function getAlternateMode(
  currentMode: ViewerMode,
  autoMode: ViewerMode,
  other: ViewerMode,
): ViewerMode {
  return currentMode === other ? autoMode : other;
}

export function ModeToggle({ currentMode, autoMode }: ModeToggleProps) {
  const shortcuts = useCommandShortcuts();
  const viewerHost = useViewerHost();
  const other = useCounterpart(autoMode);
  // Quick View picks the mode from its bar instead.
  if (viewerHost.embedded) return null;
  const alternate = getAlternateMode(currentMode, autoMode, other);
  const modes: [ViewerMode, string][] = [
    [autoMode, modeLabel(autoMode)],
    [other, modeLabel(other)],
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
                      `Switch to ${modeLabel(mode).toLowerCase()} view`,
                      "viewer_toggle_hex",
                    )
                  : `Switch to ${modeLabel(mode).toLowerCase()} view`
            }
          >
            {label}
          </button>
        );
      })}
    </span>
  );
}
