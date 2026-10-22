import { useEffect, useState, useSyncExternalStore } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type { WindowButton, WindowButtons } from "./bindings";
import { commands } from "./bindings";
import { safeSilent } from "./ipc";
import { setFullscreen, useAppearance, useFullscreen } from "./appearance";
import { DropdownMenu as DM } from "./menus";
import { Shortcut, useCommands } from "../main_window/ContextMenu";
import menuStyles from "../main_window/Menu.module.scss";
import styles from "./TitleBar.module.scss";

/// The window's title as the header bar shows it. The webview sets the main
/// window's and the editor's through `setWindowTitle`; Rust titles a viewer
/// as it shows it, which gives the window focus, when the header bar reads
/// it back.
let title: string | null = null;
const titleListeners = new Set<() => void>();

function publishTitle(next: string) {
  if (next === title) return;
  title = next;
  titleListeners.forEach((l) => l());
}

export function setWindowTitle(next: string) {
  publishTitle(next);
  safeSilent(commands.setWindowTitle(next));
}

function useWindowTitle(): string | null {
  const value = useSyncExternalStore(
    (l) => {
      titleListeners.add(l);
      return () => titleListeners.delete(l);
    },
    () => title,
  );
  useEffect(() => {
    const win = getCurrentWindow();
    const read = () => win.title().then(publishTitle, () => {});
    // The window's first title, unless the webview has set one already.
    win.title().then(
      (t) => title === null && publishTitle(t),
      () => {},
    );
    window.addEventListener("focus", read);
    return () => window.removeEventListener("focus", read);
  }, []);
  return value;
}

/// Runs a command by id the way a key binding does; the main window
/// registers one, and its header bar then offers the primary menu.
type CommandRunner = (id: string) => void;
let runner: CommandRunner | null = null;
const runnerListeners = new Set<() => void>();

export function registerCommandRunner(next: CommandRunner | null) {
  runner = next;
  runnerListeners.forEach((l) => l());
}

function useCommandRunner(): CommandRunner | null {
  return useSyncExternalStore(
    (l) => {
      runnerListeners.add(l);
      return () => runnerListeners.delete(l);
    },
    () => runner,
  );
}

/// The window's own title bar, where Newt draws one. macOS draws the title
/// and traffic lights over the top of the page, on the strip this leaves
/// for them; on GNOME this is the whole header bar, GTK keeping only the
/// window's frame. Window chrome is the one place a component follows the
/// platform: Windows and KDE draw their own title bars.
export function TitleBar() {
  const { platform, window_buttons } = useAppearance();
  const fullscreen = useFullscreen();
  const drawn = platform === "macos" || platform === "gnome";

  useEffect(() => {
    if (!drawn) return;
    const win = getCurrentWindow();
    const sync = () => win.isFullscreen().then(setFullscreen, () => {});
    sync();
    const unlisten = win.onResized(sync);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [drawn]);

  if (!drawn || fullscreen) return null;
  if (platform === "gnome") {
    return <HeaderBar buttons={window_buttons ?? { start: [], end: [] }} />;
  }
  return <div className={styles.titleBar} data-tauri-drag-region />;
}

function HeaderBar({ buttons }: { buttons: WindowButtons }) {
  const title = useWindowTitle();
  const run = useCommandRunner();
  const maximized = useMaximized();
  // Square corners, with GTK's frame (see `_platform.scss`); GTK's own
  // report covers tiling, this a window that opens maximized.
  useEffect(() => {
    document.documentElement.toggleAttribute("data-maximized", maximized);
  }, [maximized]);

  return (
    <div className={styles.headerBar} data-tauri-drag-region>
      <div className={styles.start} data-tauri-drag-region>
        {buttons.start.map((b) => (
          <WindowControl key={b} kind={b} maximized={maximized} />
        ))}
      </div>
      <div className={styles.title} data-tauri-drag-region>
        {title}
      </div>
      <div className={styles.end} data-tauri-drag-region>
        {run && <PrimaryMenu run={run} />}
        {buttons.end.map((b) => (
          <WindowControl key={b} kind={b} maximized={maximized} />
        ))}
      </div>
    </div>
  );
}

function useMaximized(): boolean {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    const win = getCurrentWindow();
    const sync = () => win.isMaximized().then(setMaximized, () => {});
    sync();
    const unlisten = win.onResized(sync);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
  return maximized;
}

const CONTROL_LABEL: Record<WindowButton, string> = {
  minimize: "Minimize",
  maximize: "Maximize",
  close: "Close",
};

function WindowControl({
  kind,
  maximized,
}: {
  kind: WindowButton;
  maximized: boolean;
}) {
  const win = getCurrentWindow();
  const act = {
    minimize: () => win.minimize(),
    maximize: () => win.toggleMaximize(),
    close: () => win.close(),
  }[kind];
  return (
    <button
      className={styles.control}
      tabIndex={-1}
      aria-label={
        kind === "maximize" && maximized ? "Restore" : CONTROL_LABEL[kind]
      }
      onMouseDown={(e) => e.preventDefault()}
      onClick={() => void act()}
    >
      <svg viewBox="0 0 16 16" aria-hidden>
        {kind === "close" && <path d="M4.5 4.5l7 7m0-7l-7 7" />}
        {kind === "minimize" && <path d="M4.5 11.5h7" />}
        {kind === "maximize" &&
          (maximized ? (
            <path d="M6.5 4.5h5v5M4.5 6.5h5v5h-5z" />
          ) : (
            <rect x="4.5" y="4.5" width="7" height="7" rx="1" />
          ))}
      </svg>
    </button>
  );
}

/// GNOME's primary menu: the window- and app-level commands a menu bar
/// would hold.
const PRIMARY_MENU: ([string, string] | null)[] = [
  ["new_window", "New Window"],
  ["connect_remote", "Connect to Remote Host…"],
  null,
  ["command_palette", "Command Palette"],
  ["open_settings", "Settings"],
  null,
  ["documentation", "Newt Documentation"],
  ["about", "About Newt"],
];

function PrimaryMenu({ run }: { run: CommandRunner }) {
  const commands = useCommands();
  return (
    <DM.Root>
      <DM.Trigger asChild>
        <button
          className={styles.menuButton}
          tabIndex={-1}
          aria-label="Main Menu"
          onMouseDown={(e) => e.preventDefault()}
        >
          <svg viewBox="0 0 16 16" aria-hidden>
            <path d="M2.5 4h11M2.5 8h11M2.5 12h11" />
          </svg>
        </button>
      </DM.Trigger>
      <DM.Portal>
        <DM.Content className={menuStyles.content} align="end" loop>
          {PRIMARY_MENU.map((entry, i) =>
            entry ? (
              <DM.Item
                key={entry[0]}
                className={menuStyles.item}
                onSelect={() => run(entry[0])}
              >
                {entry[1]}
                <Shortcut commands={commands} id={entry[0]} />
              </DM.Item>
            ) : (
              <DM.Separator key={i} className={menuStyles.separator} />
            ),
          )}
        </DM.Content>
      </DM.Portal>
    </DM.Root>
  );
}
