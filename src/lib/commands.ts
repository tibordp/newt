import { MainWindowState } from "../main_window/types";
import { safeCommand } from "./ipc";
import { PreferencesState, ResolvedBinding } from "./preferences";

/// Execute a command by its ID. Dispatches to the corresponding cmd_* Tauri
/// command, which is intercepted by the middleware that closes the current
/// modal before execution.
export const executeCommandById = (
  commandId: string,
  state: MainWindowState,
  prefs: PreferencesState,
): Promise<void> | null => {
  const commandInfo = prefs.commands.find((c) => c.id === commandId);
  if (!commandInfo) return null;

  const paneHandle = state.display_options.active_pane;
  if (commandInfo.needs_pane && !paneHandle && paneHandle !== 0) {
    return null;
  }

  return safeCommand("cmd_" + commandId, { paneHandle });
};

/// Normalize a keyboard event into a canonical key string matching the Rust format.
/// Format: modifier+modifier+key, all lowercase.
/// Modifier order: meta, ctrl, shift, alt.
export function normalizeKeyEvent(e: KeyboardEvent): string {
  const parts: string[] = [];

  if (e.metaKey) parts.push("meta");
  if (e.ctrlKey) parts.push("ctrl");
  if (e.shiftKey) parts.push("shift");
  if (e.altKey) parts.push("alt");

  // Normalize the key name
  let key = e.key;

  // Don't include standalone modifier presses
  if (key === "Control" || key === "Shift" || key === "Alt" || key === "Meta") {
    return "";
  }

  // Normalize common key names
  key = key.toLowerCase();

  // Map key names to our canonical format
  const keyMap: Record<string, string> = {
    " ": "space",
    arrowup: "up",
    arrowdown: "down",
    arrowleft: "left",
    arrowright: "right",
    escape: "escape",
    enter: "enter",
    backspace: "backspace",
    tab: "tab",
    delete: "delete",
    insert: "insert",
    home: "home",
    end: "end",
    pageup: "pageup",
    pagedown: "pagedown",
  };

  key = keyMap[key] || key;

  parts.push(key);
  return parts.join("+");
}

/// Build a lookup map from normalized key string to bindings for O(1) lookup.
export function buildBindingMap(
  bindings: ResolvedBinding[],
): Map<string, ResolvedBinding[]> {
  const map = new Map<string, ResolvedBinding[]>();
  for (const binding of bindings) {
    const existing = map.get(binding.key);
    if (existing) {
      existing.push(binding);
    } else {
      map.set(binding.key, [binding]);
    }
  }
  return map;
}

/// Determine the current "when" context from state.
export function getCurrentContext(
  state: MainWindowState | null,
): string | null {
  if (!state) return null;
  if (state.display_options.panes_focused) return "pane_focused";
  if (state.display_options.active_terminal != null) return "terminal_focused";
  return null;
}

export const modifiers = (e: React.KeyboardEvent<Element>) => {
  const isMac = navigator.platform.indexOf("Mac") === 0;
  const noModifiers = !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;
  let ctrlOrMeta;
  let insertKey;
  if (isMac) {
    ctrlOrMeta = e.metaKey;
    insertKey = "Help";
  } else {
    ctrlOrMeta = e.ctrlKey;
    insertKey = "Insert";
  }

  return { isMac, noModifiers, ctrlOrMeta, insertKey };
};
