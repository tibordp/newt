import {
  Fragment,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type { MenuEntry, WindowButton, WindowButtons } from "./bindings";
import { commands } from "./bindings";
import { safeSilent } from "./ipc";
import {
  drawsHeaderBar,
  setFullscreen,
  useAppearance,
  useFullscreen,
} from "./appearance";
import { DropdownMenu as DM, refocusOwner } from "./menus";
import { Shortcut, useCommands } from "../main_window/ContextMenu";
import menuStyles from "../main_window/Menu.module.scss";
import styles from "./TitleBar.module.scss";

/// The window's title as the title bar shows it. The webview sets the main
/// window's and the editor's through `setWindowTitle`, and then never reads
/// the window's back: a read can resolve after a newer title was set. Rust
/// titles a viewer as it shows it, which gives the window focus, when the
/// title bar reads it.
let title: string | null = null;
let titledByPage = false;
const titleListeners = new Set<() => void>();

function publishTitle(next: string) {
  if (next === title) return;
  title = next;
  titleListeners.forEach((l) => l());
}

export function setWindowTitle(next: string) {
  titledByPage = true;
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
    const read = () =>
      win.title().then(
        (t) => titledByPage || publishTitle(t),
        () => {},
      );
    read();
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

/// The header bar Newt draws on GNOME, GTK keeping only the window's
/// frame. Window chrome is the one place a component follows the platform:
/// macOS, Windows, KDE and the GNOME look on other desktops have the
/// system's own title bar.
export function TitleBar() {
  const appearance = useAppearance();
  const fullscreen = useFullscreen();
  const drawn = drawsHeaderBar(appearance);

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

  if (!drawn || fullscreen || !appearance.window_buttons) return null;
  return <HeaderBar buttons={appearance.window_buttons} />;
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
    <div
      className={styles.headerBar}
      data-tauri-drag-region
      onContextMenu={(e) => {
        e.preventDefault();
        void commands.showWindowMenu();
      }}
    >
      <div className={styles.start} data-tauri-drag-region>
        {buttons.start.map((b) => (
          <WindowControl key={b} kind={b} maximized={maximized} />
        ))}
      </div>
      <div className={styles.title} data-tauri-drag-region>
        {title}
      </div>
      <div className={styles.end} data-tauri-drag-region>
        {run ? <PrimaryMenu run={run} /> : <WindowMenu />}
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

/// The element the last press landed on. A press on the header bar starts
/// a window move, whose pointer grab swallows the release, so WebKit still
/// holds the button down: the next press makes no pointerdown, and only its
/// release arrives. A header bar button released while the header bar holds
/// the last press acts on the release instead.
let lastPress: Element | null = null;
window.addEventListener(
  "pointerdown",
  (e) => {
    lastPress = e.target instanceof Element ? e.target : null;
  },
  true,
);

function releasedAfterMove(e: React.PointerEvent): boolean {
  return (
    e.button === 0 &&
    !!lastPress?.closest("[data-tauri-drag-region]") &&
    !e.currentTarget.contains(lastPress)
  );
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
      onPointerUp={(e) => {
        if (releasedAfterMove(e)) void act();
      }}
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

/// The header bar's menu button and its menu.
function HeaderMenu({
  label,
  onOpen,
  onCloseAutoFocus,
  children,
}: {
  label: string;
  onOpen?: () => void;
  onCloseAutoFocus?: (e: Event) => void;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const change = (next: boolean) => {
    if (next) onOpen?.();
    setOpen(next);
  };
  return (
    <DM.Root open={open} onOpenChange={change}>
      <DM.Trigger asChild>
        <button
          className={styles.menuButton}
          tabIndex={-1}
          aria-label={label}
          onMouseDown={(e) => e.preventDefault()}
          onPointerUp={(e) => {
            if (!open && releasedAfterMove(e)) change(true);
          }}
        >
          <svg viewBox="0 0 16 16" aria-hidden>
            <path d="M2.5 4h11M2.5 8h11M2.5 12h11" />
          </svg>
        </button>
      </DM.Trigger>
      <DM.Portal>
        <DM.Content
          className={menuStyles.content}
          align="end"
          loop
          onCloseAutoFocus={onCloseAutoFocus}
        >
          {children}
        </DM.Content>
      </DM.Portal>
    </DM.Root>
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
    <HeaderMenu label="Main Menu">
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
    </HeaderMenu>
  );
}

/// A viewer's or editor's menu, which is its menu bar elsewhere
/// (`window_menu.rs`): each top-level menu a section, nested ones submenus.
/// Fetched afresh as it opens, so checks reflect the window's state.
function WindowMenu() {
  const [entries, setEntries] = useState<MenuEntry[] | null>(null);
  const returnFocus = useRef<Element | null>(null);

  useEffect(() => {
    // A viewer gets its menu as it is shown, which focuses it.
    const refresh = () => commands.getWindowMenu().then(setEntries, () => {});
    refresh();
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, []);

  if (!entries?.length) return null;
  return (
    <HeaderMenu
      label="Main Menu"
      onOpen={() => {
        returnFocus.current = document.activeElement;
        commands.getWindowMenu().then(setEntries, () => {});
      }}
      // Viewer and editor windows have no owner for `REFOCUS_EVENT`;
      // focus goes back where it was.
      onCloseAutoFocus={(e) => {
        const target = returnFocus.current;
        if (target instanceof HTMLElement && target.isConnected) {
          e.preventDefault();
          target.focus();
        } else {
          refocusOwner(e);
        }
      }}
    >
      {entries.map((entry, i) =>
        entry.kind === "submenu" ? (
          <Fragment key={i}>
            {i > 0 && <DM.Separator className={menuStyles.separator} />}
            <DM.Label className={menuStyles.sectionHeader}>
              {entry.label}
            </DM.Label>
            <MenuItems items={entry.items} />
          </Fragment>
        ) : (
          <MenuItems key={i} items={[entry]} />
        ),
      )}
    </HeaderMenu>
  );
}

function MenuItems({ items }: { items: MenuEntry[] }) {
  // Check items keep their labels in line with plain ones beside them.
  const checks = items.some((e) => e.kind === "item" && e.checked !== null);
  return items.map((entry, i) => {
    switch (entry.kind) {
      case "separator":
        return <DM.Separator key={i} className={menuStyles.separator} />;
      case "submenu":
        return (
          <DM.Sub key={i}>
            <DM.SubTrigger
              className={menuStyles.item}
              disabled={!entry.enabled}
            >
              {checks && <span className={menuStyles.checkColumn} />}
              <span className={menuStyles.itemLabel}>{entry.label}</span>
              <span className={menuStyles.shortcut} aria-hidden>
                ›
              </span>
            </DM.SubTrigger>
            <DM.Portal>
              <DM.SubContent className={menuStyles.content}>
                <MenuItems items={entry.items} />
              </DM.SubContent>
            </DM.Portal>
          </DM.Sub>
        );
      case "item":
        return (
          <DM.Item
            key={entry.id}
            className={menuStyles.item}
            disabled={!entry.enabled}
            onSelect={() => void commands.activateWindowMenuItem(entry.id)}
          >
            {checks && (
              <span className={menuStyles.checkColumn}>
                {entry.checked ? "✓" : ""}
              </span>
            )}
            <span className={menuStyles.itemLabel}>{entry.label}</span>
          </DM.Item>
        );
    }
  });
}
