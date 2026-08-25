import { MainWindowState } from "../main_window/types";
import { commands } from "./bindings";
import { safe, safeCommand } from "./ipc";
import { PreferencesState, ResolvedBinding } from "./preferences";

/// Execute a command by its ID. Dispatches to the corresponding cmd_* Tauri
/// command, which is intercepted by the middleware that closes the current
/// modal before execution.
export const executeCommandById = (
  commandId: string,
  state: MainWindowState,
  prefs: PreferencesState,
): Promise<unknown> | null => {
  const commandInfo = prefs.commands.find((c) => c.id === commandId);
  if (!commandInfo) return null;

  const paneHandle = state.display_options.active_pane;
  if (commandInfo.needs_pane && !paneHandle && paneHandle !== 0) {
    return null;
  }

  // User commands dispatch to run_user_command instead of cmd_<id>
  if (commandId.startsWith("user_command_")) {
    const index = parseInt(commandId.replace("user_command_", ""), 10);
    return safe(commands.runUserCommand(paneHandle, index));
  }

  return safeCommand("cmd_" + commandId, { paneHandle });
};

/// Physical-code fallback for Alt combos. macOS composes Option+key into a
/// new character (Opt+2 arrives as key "™", Opt+Z as "Ω"), which would make
/// captured bindings layout-dependent gibberish and stop plainly-spelled
/// "alt+…" bindings from ever matching. The mapping is keyboard-position
/// based, which is the accepted tradeoff for Alt combos.
const CODE_TO_KEY: Record<string, string> = {
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backquote: "`",
};

/// The numpad operator keys get their own names so they can be bound
/// separately from the main-row `+`/`-`/`*`/`/`, which `e.key` reports
/// identically. The main-row keys stay printable characters (quick search).
const NUMPAD_OPERATORS: Record<string, string> = {
  NumpadAdd: "numpad_add",
  NumpadSubtract: "numpad_subtract",
  NumpadMultiply: "numpad_multiply",
  NumpadDivide: "numpad_divide",
};

export function numpadOperator(code: string | undefined): string | null {
  return (code && NUMPAD_OPERATORS[code]) || null;
}

function keyFromCode(code: string | undefined): string | null {
  if (!code) return null;
  if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
  if (/^(Digit|Numpad)[0-9]$/.test(code)) return code.slice(-1);
  return CODE_TO_KEY[code] ?? null;
}

/// Normalize a keyboard event into a canonical key string matching the Rust format.
/// Format: modifier+modifier+key, all lowercase.
/// Modifier order: meta, ctrl, shift, alt.
export function normalizeKeyEvent(e: KeyboardEvent): string {
  const parts: string[] = [];

  if (e.metaKey) parts.push("meta");
  if (e.ctrlKey) parts.push("ctrl");
  if (e.shiftKey) parts.push("shift");
  if (e.altKey) parts.push("alt");

  let key = e.key;

  // A standalone modifier press is not a binding.
  if (key === "Control" || key === "Shift" || key === "Alt" || key === "Meta") {
    return "";
  }

  key = numpadOperator(e.code) ?? key.toLowerCase();

  if (e.altKey) {
    const physical = keyFromCode(e.code);
    if (physical) key = physical;
  }

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
