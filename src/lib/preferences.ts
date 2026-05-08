import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { Event } from "@tauri-apps/api/event";

export type ResolvedBinding = {
  key: string;
  command: string;
  when?: string;
};

export type CommandInfo = {
  id: string;
  name: string;
  short_name?: string;
  category: string;
  shortcut?: string;
  shortcut_display: string[];
  needs_pane: boolean;
  /// Keybinding dispatch context (pane_focused / terminal_focused / undefined = global).
  when?: string;
  /// User-command run filter (file / directory / selection / undefined = any).
  /// Only set on user commands.
  applies_to?: string;
  default_key?: string;
  default_when?: string;
  user_overridden: boolean;
};

export type AppPreferences = {
  appearance: {
    show_hidden: boolean;
    show_command_bar: boolean;
    theme: "system" | "light" | "dark";
    columns: string[];
  };
  behavior: {
    confirm_delete: boolean;
  };
  hot_paths: {
    standard_folders: boolean;
    system_bookmarks: boolean;
    mounts: boolean;
    recent_folders: boolean;
  };
};

export type BookmarkEntry = {
  path: string;
  name?: string;
};

export type UserCommandEntry = {
  title: string;
  run: string;
  key?: string;
  terminal: boolean;
  /// Run-context filter (file / directory / selection / undefined = any).
  applies_to?: string;
};

export type PreferencesState = {
  settings: AppPreferences;
  schema: any;
  modified_keys: string[];
  bindings: ResolvedBinding[];
  commands: CommandInfo[];
  bookmarks: BookmarkEntry[];
  user_commands: UserCommandEntry[];
};

export const usePreferences = (): PreferencesState | null => {
  const [state, setState] = useState<PreferencesState | null>(null);

  useEffect(() => {
    // Fetch initial state
    invoke<PreferencesState>("get_preferences")
      .then(setState)
      .catch(console.error);

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
