import { invoke } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { createContext, useEffect, useRef, useState } from "react";
import { Event } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { applyPatches, Patch } from "immer";

import type { Result } from "./bindings";

/// Settle a typed `commands.X(...)` promise into `{ ok: T } | { err: string }`.
/// tauri-specta's generated commands return `Result<T, string>` on user
/// errors but RE-THROW `Error` instances (transport failure, webview gone,
/// etc.). Both helpers here funnel through this so the wrappers below have
/// a single error-handling surface.
async function settle<T>(
  promise: Promise<Result<T, string>>,
): Promise<{ ok: T } | { err: string }> {
  try {
    const result = await promise;
    if (result.status === "error") return { err: result.error };
    return { ok: result.data };
  } catch (e) {
    return {
      err: e instanceof Error ? e.message : String(e),
    };
  }
}

/// Wrap a typed `commands.X(...)` call: on error, show a popup; return the
/// data on success or `null` on failure. Use for fire-and-handle: call sites
/// that don't otherwise inspect the return.
export const safe = async <T>(
  promise: Promise<Result<T, string>>,
): Promise<T | null> => {
  const r = await settle(promise);
  if ("err" in r) {
    await message(r.err, { kind: "error", title: "Error" });
    return null;
  }
  return r.ok;
};

/// Wrap a typed `commands.X(...)` call and return its error as a string
/// instead of popping up a toast. Use this when the caller wants to render
/// the error inline (e.g. inside a dialog form). Returns `null` on success.
export const tryRun = async <T>(
  promise: Promise<Result<T, string>>,
): Promise<string | null> => {
  const r = await settle(promise);
  return "err" in r ? r.err : null;
};

/// Wrap a typed `commands.X(...)` call: on error, log and return `null`. Use
/// for non-critical commands where the user shouldn't be interrupted.
export const safeSilent = async <T>(
  promise: Promise<Result<T, string>>,
): Promise<T | null> => {
  const r = await settle(promise);
  if ("err" in r) {
    console.error(r.err);
    return null;
  }
  return r.ok;
};

/// Unwrap a typed `commands.X(...)` call: throw on error, return data on
/// success. Use when the caller needs the data and is happy to let an
/// outer try/catch handle the error.
export const unwrap = async <T>(
  promise: Promise<Result<T, string>>,
): Promise<T> => {
  const r = await settle(promise);
  if ("err" in r) throw new Error(r.err);
  return r.ok;
};

// Dynamic-name escape hatches. Prefer the typed wrappers above; these
// stringly-typed shims exist for call sites that compute the command name at
// runtime (command-palette `cmd_<id>` dispatch, context menus) where TS can't
// statically narrow the args anyway.

export const safeCommand = async (
  command: string,
  args: object = {},
): Promise<void> => {
  try {
    await invoke(command, { ...args });
  } catch (e) {
    await message(String(e), { kind: "error", title: "Error" });
  }
};

export const tryCommand = async (
  command: string,
  args: object = {},
): Promise<string | null> => {
  try {
    await invoke(command, { ...args });
    return null;
  } catch (e) {
    return String(e);
  }
};

export const safeCommandSilent = async (
  command: string,
  args: object = {},
): Promise<void> => {
  try {
    await invoke(command, { ...args });
  } catch (e) {
    console.error(e);
  }
};

export type ChangePayload<T> = {
  state?: T;
  patch?: Patch[];
  version: number;
};

export type TerminalData = {
  handle: string;
  data: number[];
};

export const TerminalData = createContext({});

function deepUpdate(original: any, received: any): any {
  if (
    original === null ||
    received === null ||
    Array.isArray(original) !== Array.isArray(received) ||
    typeof original !== typeof received
  ) {
    return received;
  }

  let isChanged = false;
  let ret;
  if (Array.isArray(original)) {
    if (original.length !== received.length) {
      return received;
    }

    const result = Array(original.length);
    for (let i = 0; i < original.length; i++) {
      result[i] = deepUpdate(original[i], received[i]);
      isChanged = isChanged || result[i] !== original[i];
    }

    ret = isChanged ? result : original;
  } else if (typeof original === "object") {
    const keys = new Set([...Object.keys(original), ...Object.keys(received)]);

    const result: Record<string, any> = {};
    for (const key of keys) {
      result[key] = deepUpdate(original[key], received[key]);
      isChanged = isChanged || result[key] !== original[key];
    }
    ret = isChanged ? result : original;
  } else {
    ret = received;
  }

  return ret;
}

export const useRemoteState = <T>(
  event_name: string,
  deps: any[] = [],
): T | null => {
  const version = useRef<number | null>(null);
  const [state, setState] = useState<T | null>(null);

  useEffect(() => {
    const appWindow = getCurrentWebviewWindow();
    const listenPromise = appWindow.listen(
      `update:${event_name}`,
      (event: Event<ChangePayload<T>>) => {
        // State is serialized, so we perform a "deep" update (diff), updating
        // only the changed parts of the current state. This is to avoid losing
        // the reference to the state object, which would cause a re-render of
        // the entire component tree.
        setState((s) => {
          let ret: T | null = s;
          if (event.payload.patch) {
            if (
              version.current !== null &&
              event.payload.version === version.current + 1
            ) {
              version.current = event.payload.version;
              ret = applyPatches(s as any, event.payload.patch) as T;
            } else if (version.current !== null) {
              // this should never happen, but just in case
              console.warn("version mismatch, requesting full state...");
              invoke("ping", { name: event_name }).catch(() => {});
              ret = s;
            }
          } else {
            version.current = event.payload.version;
            ret = deepUpdate(s, event.payload.state!);
          }
          return ret;
        });
      },
    );
    listenPromise.then(() =>
      invoke("ping", { name: event_name }).catch(() => {}),
    );
    return () => {
      listenPromise.then((unlisten) => unlisten());
    };
  }, deps);

  // @ts-expect-error debug helper
  window.__NEWT_STATE = window.__NEWT_STATE ?? {};
  // @ts-expect-error debug helper
  window.__NEWT_STATE[event_name] = state;

  return state;
};

export type DataCallback = (data: number[]) => void;

type TerminalDataListener = {
  messages: number[][];
  listener?: DataCallback;
  disconnected?: boolean;
};

export type TerminalDataState = {
  [handle: string]: TerminalDataListener;
};

export const useTerminalData = (deps: any[] = []): any => {
  const state = useRef<TerminalDataState>({});

  useEffect(() => {
    const appWindow = getCurrentWebviewWindow();
    const listenPromise = appWindow.listen(
      "terminal_data",
      (event: Event<TerminalData>) => {
        if (!(event.payload.handle in state.current)) {
          state.current[event.payload.handle] = {
            messages: [],
            listener: undefined,
          };
        }
        const cur = state.current[event.payload.handle];
        if (!cur.disconnected) {
          if (cur.listener) {
            cur.listener(event.payload.data);
          } else {
            cur.messages.push(event.payload.data);
          }
        }
      },
    );
    return () => {
      listenPromise.then((unlisten) => unlisten());
    };
  }, deps);

  return state.current;
};

export const registerTerminalDataHandler = (
  context: TerminalDataState,
  handle: number,
  listener: DataCallback,
): (() => void) => {
  if (!(handle in context)) {
    context[handle] = { messages: [], listener: listener };
  } else {
    if (context[handle].listener) {
      throw new Error("cannot have more than one listener per terminal");
    }
    context[handle].listener = listener;
    while (context[handle].messages.length > 0) {
      listener(context[handle].messages.shift()!);
    }
  }
  return () => {
    context[handle].disconnected = true;
  };
};
