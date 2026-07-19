import { useEffect, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Event } from "@tauri-apps/api/event";

import { commands, type RuntimeState } from "./bindings";
import { unwrap } from "./ipc";

export type { RuntimeState } from "./bindings";

/// App-wide ephemeral UI state (state.json) — the machine-written sibling
/// of usePreferences. Updates arrive via the app-wide broadcast whenever
/// any window writes a key.
export const useRuntimeState = (): RuntimeState | null => {
  const [state, setState] = useState<RuntimeState | null>(null);

  useEffect(() => {
    unwrap(commands.getRuntimeState()).then(setState).catch(console.error);

    const appWindow = getCurrentWebviewWindow();
    const listenPromise = appWindow.listen(
      "update:runtime-state",
      (event: Event<RuntimeState>) => {
        setState(event.payload);
      },
    );

    return () => {
      listenPromise.then((unlisten) => unlisten());
    };
  }, []);

  return state;
};
