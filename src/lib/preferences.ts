import { useEffect, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Event } from "@tauri-apps/api/event";

import { commands, type ResolvedPreferences } from "./bindings";
import { unwrap } from "./ipc";

export type {
  AppPreferences,
  BookmarkEntry,
  CommandInfo,
  ResolvedBinding,
  ResolvedPreferences,
  UserCommandEntry,
} from "./bindings";

export type PreferencesState = ResolvedPreferences;

export const usePreferences = (): PreferencesState | null => {
  const [state, setState] = useState<PreferencesState | null>(null);

  useEffect(() => {
    // Fetch initial state
    unwrap(commands.getPreferences()).then(setState).catch(console.error);

    // Listen for updates from file watcher
    const appWindow = getCurrentWebviewWindow();
    const listenPromise = appWindow.listen(
      "update:preferences",
      (event: Event<PreferencesState>) => {
        setState(event.payload);
      },
    );

    return () => {
      listenPromise.then((unlisten) => unlisten());
    };
  }, []);

  return state;
};
