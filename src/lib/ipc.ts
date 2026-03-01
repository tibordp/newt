import { invoke } from "@tauri-apps/api/core";
import { message } from "@tauri-apps/plugin-dialog";
import { createContext, useEffect, useRef, useState } from "react";
import { Event } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { enablePatches, applyPatches, Patch } from "immer";

export const safeCommand = async (
  command: string,
  args: object = {},
): Promise<void> => {
  try {
    await invoke(command, { ...args });
  } catch (e) {
    await message(String(e), {
      kind: "error",
      title: "Error",
    });
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
              invoke("ping", {}).catch(() => {});
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
    listenPromise.then(() => invoke("ping", {}).catch(() => {}));
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
