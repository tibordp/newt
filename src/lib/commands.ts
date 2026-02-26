import { MainWindowState } from "../main_window/types";
import { confirm } from "@tauri-apps/plugin-dialog";
import { safeCommand } from "./ipc";

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

export class Shortcut {
  private _ctrl = false;
  private _shift = false;
  private _meta = false;
  private _alt = false;
  private _key = null;
  private _char = null;

  cmd() {
    if (navigator.platform.indexOf("Mac") === 0) {
      this._meta = true;
    } else {
      this._ctrl = true;
    }
    return this;
  }

  ctrl() {
    this._ctrl = true;
    return this;
  }

  meta() {
    this._meta = true;
    return this;
  }

  shift() {
    this._shift = true;
    return this;
  }

  alt() {
    this._alt = true;
    return this;
  }

  key(key: string) {
    this._char = null;
    this._key = key;
    return this;
  }

  char(char: string) {
    this._key = null;
    this._char = char;
    return this;
  }

  render() {
    let ret = [];
    if (this._ctrl) {
      ret.push("Ctrl");
    }
    if (this._meta) {
      ret.push("⌘");
    }
    if (this._shift) {
      ret.push("Shift");
    }
    if (this._alt) {
      ret.push("Alt");
    }
    if (this._key) {
      ret.push(this._key);
    }
    if (this._char) {
      ret.push(this._char.toUpperCase());
    }
    return ret;
  }

  matches(e: KeyboardEvent) {
    if (
      this._ctrl != e.ctrlKey ||
      this._shift != e.shiftKey ||
      this._meta != e.metaKey ||
      this._alt != e.altKey
    ) {
      return false;
    }

    if (this._key) {
      return this._key == e.key;
    } else if (this._char) {
      return this._char.toLowerCase() == e.key.toLowerCase();
    }

    return false;
  }
}

export type Command = {
  name: string;
  command?: string;
  noPane?: boolean;
  shortcut?: Shortcut;
  args?: object;
  callback?: (MainWindowState) => Promise<void>;
};

export const commands: Command[] = [
  {
    name: "New Window",
    command: "new_window",
    noPane: true,
    shortcut: new Shortcut().cmd().char("n"),
  },
  {
    name: "As Other Pane",
    command: "copy_pane",
    shortcut: new Shortcut().cmd().char("."),
  },
  {
    name: "Select All",
    command: "select_all",
    shortcut: new Shortcut().cmd().char("a"),
  },
  {
    name: "Clear Selection",
    command: "deselect_all",
    shortcut: new Shortcut().cmd().char("d"),
  },
  { name: "View", command: "view", shortcut: new Shortcut().key("F3") },
  {
    name: "Rename...",
    command: "dialog",
    args: { dialog: "rename" },
    shortcut: new Shortcut().key("F2"),
  },
  {
    name: "File properties...",
    command: "dialog",
    args: { dialog: "properties" },
    shortcut: new Shortcut().alt().key("Enter"),
  },
  {
    name: "Delete Selected",
    callback: async (state: MainWindowState) => {
      const paneHandle = state.display_options.active_pane;
      const pane = state.panes[paneHandle];

      const names = pane.selected.length > 0
        ? pane.selected
        : pane.focused ? [pane.focused] : [];

      if (names.length === 0) return;

      let message;
      if (names.length > 1) {
        message = `Delete ${names.length} selected files?`;
      } else {
        message = `Delete ${names[0]}?`;
      }
      confirm(message, { title: "Delete" }).then(async (confirmed) => {
        if (confirmed) {
          await safeCommand("delete_selected", { paneHandle });
        }
      });
    },
    shortcut: new Shortcut().shift().key("Delete"),
  },
  {
    name: "Create Directory...",
    command: "dialog",
    args: { dialog: "create_directory" },
    shortcut: new Shortcut().key("F7"),
  },
  {
    name: "Create File...",
    command: "dialog",
    args: { dialog: "create_file" },
    shortcut: new Shortcut().shift().key("F4"),
  },
  {
    name: "Go To...",
    command: "dialog",
    args: { dialog: "navigate" },
    shortcut: new Shortcut().cmd().char("l"),
  },
  { name: "Open in Default App", command: "open" },
  {
    name: "Open in Terminal",
    command: "send_to_terminal",
    shortcut: new Shortcut().cmd().key("Enter"),
  },
  {
    name: "Copy to Other Pane...",
    command: "dialog",
    args: { dialog: "copy" },
    shortcut: new Shortcut().key("F5"),
  },
  {
    name: "Move to Other Pane...",
    command: "dialog",
    args: { dialog: "move" },
    shortcut: new Shortcut().key("F6"),
  },
  {
    name: "Copy Path to Clipboard",
    command: "copy_to_clipboard",
    shortcut: new Shortcut().cmd().char("c"),
  },
  {
    name: "Paste Path from Clipboard",
    command: "paste_from_clipboard",
    shortcut: new Shortcut().cmd().char("v"),
  },
  {
    name: "Toggle Hidden Files",
    command: "toggle_hidden",
    noPane: true,
    shortcut: new Shortcut().cmd().char("h"),
  },
  {
    name: "Close Window",
    command: "close_window",
    noPane: true,
    shortcut: new Shortcut().cmd().char("w"),
  },
  {
    name: "Reload Window",
    callback: async () => window.location.reload(),
    noPane: true,
  },
  {
    name: "Connect to Remote Host...",
    command: "dialog",
    args: { dialog: "connect_remote" },
    noPane: true,
    shortcut: new Shortcut().cmd().shift().char("r"),
  },
  {
    name: "Open Elevated",
    command: "open_elevated",
    noPane: true,
  },
  {
    name: "Select Filesystem",
    command: "dialog",
    args: { dialog: "select_vfs" },
    shortcut: new Shortcut().cmd().shift().char("l"),
  },
  {
    name: "Mount S3",
    command: "mount_s3",
  },
  {
    name: "Open Folder in Default File Manager",
    command: "open_folder",
    shortcut: new Shortcut().shift().key("F3"),
  },
];

export const executeCommand = (
  command: Command,
  state: MainWindowState
): Promise<void> | null => {
  const paneHandle = state.display_options.active_pane;
  if (!command.noPane && (!paneHandle && paneHandle !== 0)) {
    return null;
  }

  if (command.command) {
    return safeCommand(command.command, {
      ...(command.args || {}),
      paneHandle,
    });
  } else if (command.callback) {
    return command.callback(state);
  }
};
