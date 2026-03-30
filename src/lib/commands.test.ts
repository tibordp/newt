import { describe, it, expect } from "vitest";
import {
  normalizeKeyEvent,
  buildBindingMap,
  getCurrentContext,
} from "./commands";
import type { ResolvedBinding } from "./preferences";
import type { MainWindowState } from "../main_window/types";

function fakeKeyEvent(
  overrides: Partial<KeyboardEvent> & { key: string },
): KeyboardEvent {
  return {
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    ...overrides,
  } as KeyboardEvent;
}

describe("normalizeKeyEvent", () => {
  it("normalizes a plain letter key", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "a" }))).toBe("a");
  });

  it("adds ctrl modifier", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "a", ctrlKey: true }))).toBe(
      "ctrl+a",
    );
  });

  it("adds meta modifier", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "a", metaKey: true }))).toBe(
      "meta+a",
    );
  });

  it("orders modifiers: meta, ctrl, shift, alt", () => {
    expect(
      normalizeKeyEvent(
        fakeKeyEvent({
          key: "a",
          metaKey: true,
          ctrlKey: true,
          shiftKey: true,
          altKey: true,
        }),
      ),
    ).toBe("meta+ctrl+shift+alt+a");
  });

  it("returns empty string for standalone modifier", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Control" }))).toBe("");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Shift" }))).toBe("");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Alt" }))).toBe("");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Meta" }))).toBe("");
  });

  it("maps space", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: " " }))).toBe("space");
  });

  it("maps arrow keys", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "ArrowUp" }))).toBe("up");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "ArrowDown" }))).toBe("down");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "ArrowLeft" }))).toBe("left");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "ArrowRight" }))).toBe(
      "right",
    );
  });

  it("maps special keys", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Escape" }))).toBe("escape");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Enter" }))).toBe("enter");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Backspace" }))).toBe(
      "backspace",
    );
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Tab" }))).toBe("tab");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Delete" }))).toBe("delete");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Insert" }))).toBe("insert");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "Home" }))).toBe("home");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "End" }))).toBe("end");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "PageUp" }))).toBe("pageup");
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "PageDown" }))).toBe(
      "pagedown",
    );
  });

  it("lowercases function keys", () => {
    expect(normalizeKeyEvent(fakeKeyEvent({ key: "F5" }))).toBe("f5");
  });

  it("handles shift+delete", () => {
    expect(
      normalizeKeyEvent(fakeKeyEvent({ key: "Delete", shiftKey: true })),
    ).toBe("shift+delete");
  });
});

describe("buildBindingMap", () => {
  it("builds empty map from empty bindings", () => {
    const map = buildBindingMap([]);
    expect(map.size).toBe(0);
  });

  it("maps single binding", () => {
    const bindings: ResolvedBinding[] = [
      { key: "ctrl+a", command: "select_all" },
    ];
    const map = buildBindingMap(bindings);
    expect(map.get("ctrl+a")).toHaveLength(1);
    expect(map.get("ctrl+a")![0].command).toBe("select_all");
  });

  it("groups multiple bindings with same key", () => {
    const bindings: ResolvedBinding[] = [
      { key: "ctrl+a", command: "cmd1" },
      { key: "ctrl+a", command: "cmd2", when: "pane_focused" },
    ];
    const map = buildBindingMap(bindings);
    expect(map.get("ctrl+a")).toHaveLength(2);
  });

  it("separates different keys", () => {
    const bindings: ResolvedBinding[] = [
      { key: "ctrl+a", command: "cmd1" },
      { key: "ctrl+b", command: "cmd2" },
    ];
    const map = buildBindingMap(bindings);
    expect(map.get("ctrl+a")).toHaveLength(1);
    expect(map.get("ctrl+b")).toHaveLength(1);
  });
});

describe("getCurrentContext", () => {
  it("returns null for null state", () => {
    expect(getCurrentContext(null)).toBeNull();
  });

  it("returns pane_focused when panes are focused", () => {
    const state = {
      display_options: { panes_focused: true, active_terminal: null },
    } as unknown as MainWindowState;
    expect(getCurrentContext(state)).toBe("pane_focused");
  });

  it("returns terminal_focused when terminal is active", () => {
    const state = {
      display_options: { panes_focused: false, active_terminal: 0 },
    } as unknown as MainWindowState;
    expect(getCurrentContext(state)).toBe("terminal_focused");
  });

  it("returns null when nothing focused", () => {
    const state = {
      display_options: { panes_focused: false, active_terminal: null },
    } as unknown as MainWindowState;
    expect(getCurrentContext(state)).toBeNull();
  });
});
