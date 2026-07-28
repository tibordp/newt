import * as monaco from "monaco-editor";

// Registry key strings are canonical "meta+ctrl+shift+alt+key", lowercase,
// with `mod` already expanded per platform (see `normalizeKeyEvent`).
const NAMED_KEYS: Record<string, keyof typeof monaco.KeyCode> = {
  enter: "Enter",
  escape: "Escape",
  space: "Space",
  tab: "Tab",
  backspace: "Backspace",
  delete: "Delete",
  insert: "Insert",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
  up: "UpArrow",
  down: "DownArrow",
  left: "LeftArrow",
  right: "RightArrow",
  "=": "Equal",
  "-": "Minus",
  "[": "BracketLeft",
  "]": "BracketRight",
  ";": "Semicolon",
  "'": "Quote",
  ",": "Comma",
  ".": "Period",
  "/": "Slash",
  "`": "Backquote",
  "\\": "Backslash",
};

/// Translate a resolved registry key string into Monaco's keybinding
/// encoding, or null when the key has no Monaco equivalent (the window-level
/// dispatcher still handles it; only the in-Monaco binding and its palette
/// hint are lost).
export function monacoKeybinding(key: string): number | null {
  const isMac = navigator.platform.startsWith("Mac");
  const parts = key.split("+");
  const base = parts.pop();
  if (!base) return null;

  let mods = 0;
  for (const part of parts) {
    switch (part) {
      case "meta":
        mods |= isMac ? monaco.KeyMod.CtrlCmd : monaco.KeyMod.WinCtrl;
        break;
      case "ctrl":
        mods |= isMac ? monaco.KeyMod.WinCtrl : monaco.KeyMod.CtrlCmd;
        break;
      case "shift":
        mods |= monaco.KeyMod.Shift;
        break;
      case "alt":
        mods |= monaco.KeyMod.Alt;
        break;
      default:
        return null;
    }
  }

  let code: monaco.KeyCode | undefined;
  if (/^[a-z]$/.test(base)) {
    code = monaco.KeyCode[`Key${base.toUpperCase()}` as "KeyA"];
  } else if (/^[0-9]$/.test(base)) {
    code = monaco.KeyCode[`Digit${base}` as "Digit0"];
  } else if (/^f([1-9]|1[0-9])$/.test(base)) {
    code = monaco.KeyCode[`F${base.slice(1)}` as "F1"];
  } else {
    const named = NAMED_KEYS[base];
    if (named) code = monaco.KeyCode[named];
  }
  if (code === undefined) return null;

  return mods | code;
}
