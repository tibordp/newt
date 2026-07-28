import { useEffect, useMemo, useRef } from "react";

import type { CommandScope } from "./bindings";
import { normalizeKeyEvent } from "./commands";
import { usePreferences } from "./preferences";

/// A handler may return false to decline the key (e.g. image copy yielding
/// to a native text-selection copy); the event then proceeds unprevented.
type ScopedHandler = () => void | false;

/// Match keydowns against the central keybinding registry for a viewer or
/// editor window. Commands fire only in windows/modes that register a
/// handler for them; fundamental keys (Escape, arrows, page keys) are
/// deliberately not commands and stay hardcoded in the components, which
/// consume them before this hook sees the event.
export function useScopedBindings(
  scope: Exclude<CommandScope, "main">,
  handlers: Record<string, ScopedHandler>,
) {
  const preferences = usePreferences();
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  const bindingMap = useMemo(() => {
    const map = new Map<string, { command: string; when: string | null }[]>();
    if (!preferences) return map;
    const scoped = new Set(
      preferences.commands.filter((c) => c.scope === scope).map((c) => c.id),
    );
    for (const b of preferences.bindings) {
      if (!scoped.has(b.command)) continue;
      const entry = { command: b.command, when: b.when ?? null };
      const list = map.get(b.key);
      if (list) list.push(entry);
      else map.set(b.key, [entry]);
    }
    return map;
  }, [preferences, scope]);
  const mapRef = useRef(bindingMap);
  mapRef.current = bindingMap;

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      const key = normalizeKeyEvent(e);
      if (!key) return;
      const candidates = mapRef.current.get(key);
      if (!candidates) return;

      // Keys that would type into a focused input stay with the input
      const t = e.target;
      const editable =
        t instanceof HTMLInputElement ||
        t instanceof HTMLTextAreaElement ||
        (t instanceof HTMLElement && t.isContentEditable);
      if (editable && !e.metaKey && !e.ctrlKey && e.key.length === 1) return;

      // Prefer a scope-context binding over a when-less (global) one —
      // same resolution as the main window's dispatcher
      let match: { command: string; when: string | null } | null = null;
      for (const b of candidates) {
        if (!handlersRef.current[b.command]) continue;
        if (b.when) {
          if (b.when === scope) match = b;
        } else if (!match || !match.when) {
          match = b;
        }
      }
      if (!match) return;

      if (handlersRef.current[match.command]() !== false) e.preventDefault();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [scope]);
}

/// Resolved shortcut labels for tooltips and menu hints, by command id.
/// `get` returns null for unbound commands so callers can omit the hint;
/// `label` appends the shortcut to a tooltip ("Zoom in (=)").
export function useCommandShortcuts(): {
  get: (id: string) => string | null;
  label: (text: string, id: string) => string;
} {
  const preferences = usePreferences();
  return useMemo(() => {
    const map = new Map<string, string>();
    for (const c of preferences?.commands ?? []) {
      if (c.shortcut_display.length > 0) {
        map.set(c.id, c.shortcut_display.join("+"));
      }
    }
    const get = (id: string) => map.get(id) ?? null;
    return {
      get,
      label: (text: string, id: string) => {
        const k = get(id);
        return k ? `${text} (${k})` : text;
      },
    };
  }, [preferences]);
}
