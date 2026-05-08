import {
  Fragment,
  KeyboardEvent as ReactKeyboardEvent,
  useEffect,
  useRef,
  useState,
} from "react";

import { normalizeKeyEvent } from "../../../lib/commands";
import { CommandInfo, ResolvedBinding } from "../../../lib/preferences";
import styles from "../SettingsEditor.module.scss";

const IS_MAC =
  typeof navigator !== "undefined" && navigator.platform.startsWith("Mac");

/// Render a normalized key string ("ctrl+shift+f5") into display segments.
/// Mirrors the Rust `render_shortcut` so captured-but-unsaved keys can be
/// previewed before the round-trip through the backend.
export function renderShortcut(key: string): string[] {
  return key.split("+").map((part) => {
    switch (part.toLowerCase()) {
      case "ctrl":
        return "Ctrl";
      case "meta":
        return IS_MAC ? "⌘" : "Super";
      case "shift":
        return "Shift";
      case "alt":
        return IS_MAC ? "⌥" : "Alt";
      default:
        return part.length > 0 ? part[0].toUpperCase() + part.slice(1) : "";
    }
  });
}

/// Render a normalized key string as a row of <kbd> chips separated by " + ".
export function shortcutChips(key: string) {
  return (
    <span className={styles.shortcutKbd}>
      {renderShortcut(key).map((part, i) => (
        <Fragment key={i}>
          {i !== 0 ? " + " : ""}
          <kbd>{part}</kbd>
        </Fragment>
      ))}
    </span>
  );
}

/// True if the key has at least one non-modifier component.
export function isCompleteKey(key: string): boolean {
  if (!key) return false;
  const parts = key.split("+");
  const NON_MODIFIERS = parts.filter(
    (p) => !["ctrl", "meta", "shift", "alt"].includes(p.toLowerCase()),
  );
  return NON_MODIFIERS.length === 1 && NON_MODIFIERS[0].length > 0;
}

export function whenLabel(when: string | undefined | null): string {
  if (!when) return "Global";
  switch (when) {
    case "pane_focused":
      return "Pane focused";
    case "terminal_focused":
      return "Terminal focused";
    default:
      return when.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  }
}

/// Are two `when` values considered the same dispatch context?
function whenEq(a: string | null | undefined, b: string | null | undefined) {
  return (a ?? "") === (b ?? "");
}

export type Conflict = {
  kind: "hard" | "soft";
  binding: ResolvedBinding;
  commandName: string;
  commandId: string;
};

/// Detect conflicts for a candidate (key, when) being assigned to `commandId`.
/// - hard: another binding has the exact same (key, when) — they would collide
///   on dispatch.
/// - soft: same key in a different/overlapping when — one shadows the other in
///   that context but both exist.
export function detectConflicts(
  candidateKey: string,
  candidateWhen: string,
  ownCommandId: string,
  bindings: ResolvedBinding[],
  commandsById: Map<string, CommandInfo>,
): Conflict[] {
  const conflicts: Conflict[] = [];
  for (const b of bindings) {
    if (b.command === ownCommandId) continue;
    if (b.key !== candidateKey) continue;
    const sameWhen = whenEq(b.when, candidateWhen || null);
    const candidateGlobal = !candidateWhen;
    const otherGlobal = !b.when;
    const overlaps = sameWhen || candidateGlobal || otherGlobal;
    if (!overlaps) continue;
    const cmd = commandsById.get(b.command);
    conflicts.push({
      kind: sameWhen ? "hard" : "soft",
      binding: b,
      commandName: cmd?.name ?? b.command,
      commandId: b.command,
    });
  }
  return conflicts;
}

export function KeyCaptureInput({
  value,
  onChange,
  autoFocus,
  size = "compact",
}: {
  value: string;
  onChange: (key: string) => void;
  autoFocus?: boolean;
  /// "compact" matches the keybindings table row height. "regular" matches
  /// the surrounding text inputs in standard forms (CommandsEditor).
  size?: "compact" | "regular";
}) {
  const [recording, setRecording] = useState(!!autoFocus);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (autoFocus && ref.current) ref.current.focus();
  }, [autoFocus]);

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    // Tab and Escape have higher-priority semantics: Escape exits recording,
    // Tab lets the user move on to the action buttons without trapping focus.
    if (e.key === "Escape") {
      setRecording(false);
      ref.current?.blur();
      e.preventDefault();
      return;
    }
    if (e.key === "Tab") {
      // allow default focus traversal
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    const k = normalizeKeyEvent(e.nativeEvent);
    if (!k) return;
    if (isCompleteKey(k)) {
      onChange(k);
    }
  };

  const segments = value ? renderShortcut(value) : [];

  return (
    <div
      ref={ref}
      tabIndex={0}
      role="textbox"
      aria-label="Press key combination"
      className={[
        recording ? styles.keyCaptureActive : styles.keyCapture,
        size === "regular" ? styles.keyCaptureRegular : "",
      ]
        .filter(Boolean)
        .join(" ")}
      onFocus={() => setRecording(true)}
      onBlur={() => setRecording(false)}
      onKeyDown={onKeyDown}
      onClick={() => ref.current?.focus()}
    >
      {segments.length > 0 ? (
        <span className={styles.shortcutKbd}>
          {segments.map((part, i) => (
            <Fragment key={i}>
              {i !== 0 ? " + " : ""}
              <kbd>{part}</kbd>
            </Fragment>
          ))}
        </span>
      ) : (
        <span className={styles.keyCapturePlaceholder}>
          {recording ? "Press keys…" : "Click and press keys"}
        </span>
      )}
      {value && (
        <button
          type="button"
          className={styles.keyCaptureClear}
          onMouseDown={(e) => {
            // mousedown so the parent doesn't lose focus first
            e.preventDefault();
            onChange("");
            ref.current?.focus();
          }}
          title="Clear"
        >
          ×
        </button>
      )}
    </div>
  );
}
