import { ContextMenu as CM } from "../lib/menus";

import { commands as ipc } from "../lib/bindings";
import { safeSilent } from "../lib/ipc";
import { Shortcut, useCommands } from "./ContextMenu";
import styles from "./Menu.module.scss";

/// The panel-wide items every terminal menu ends with.
function PanelItems({
  activeTerminal,
  maximized,
}: {
  activeTerminal: number | null;
  maximized: boolean;
}) {
  const commands = useCommands();
  return (
    <>
      <CM.Item
        className={styles.item}
        onSelect={() => safeSilent(ipc.cmdCreateTerminal(0))}
      >
        New Terminal
        <Shortcut commands={commands} id="create_terminal" />
      </CM.Item>
      <CM.Separator className={styles.separator} />
      <CM.Item
        className={styles.item}
        onSelect={async () => {
          // What maximizes is whatever has focus, so claim it first.
          if (activeTerminal != null) {
            await safeSilent(ipc.activateTerminal(activeTerminal));
          }
          safeSilent(ipc.cmdToggleMaximized(0));
        }}
      >
        {maximized ? "Restore Split Layout" : "Maximize Terminal"}
        <Shortcut commands={commands} id="toggle_maximized" />
      </CM.Item>
      <CM.Item
        className={styles.item}
        onSelect={() => safeSilent(ipc.cmdToggleTerminalPanel(0))}
      >
        Hide Terminal Panel
        <Shortcut commands={commands} id="toggle_terminal_panel" />
      </CM.Item>
    </>
  );
}

/// Right-click on a terminal tab.
export function TabMenuContent({
  handle,
  handles,
  maximized,
}: {
  handle: number;
  handles: number[];
  maximized: boolean;
}) {
  const others = handles.filter((h) => h !== handle);
  return (
    <CM.Portal>
      <CM.Content className={styles.content} loop>
        <CM.Item
          className={styles.item}
          onSelect={() => safeSilent(ipc.closeTerminal(handle))}
        >
          Close Terminal
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={others.length === 0}
          onSelect={() => {
            for (const h of others) safeSilent(ipc.closeTerminal(h));
          }}
        >
          Close Other Terminals
        </CM.Item>
        <CM.Separator className={styles.separator} />
        <PanelItems activeTerminal={handle} maximized={maximized} />
      </CM.Content>
    </CM.Portal>
  );
}

/// Right-click on the tab bar outside the tabs.
export function TabBarMenuContent({
  activeTerminal,
  maximized,
}: {
  activeTerminal: number | null;
  maximized: boolean;
}) {
  return (
    <CM.Portal>
      <CM.Content className={styles.content} loop>
        <PanelItems activeTerminal={activeTerminal} maximized={maximized} />
      </CM.Content>
    </CM.Portal>
  );
}

/// Right-click inside a terminal: its clipboard and screen, then the
/// panel's items.
export function TerminalMenuContent({
  handle,
  hasSelection,
  maximized,
  onCopy,
  onPaste,
  onSelectAll,
  onClear,
}: {
  handle: number;
  hasSelection: boolean;
  maximized: boolean;
  onCopy: () => void;
  onPaste: () => void;
  onSelectAll: () => void;
  onClear: () => void;
}) {
  return (
    <CM.Portal>
      <CM.Content className={styles.content} loop>
        <CM.Item
          className={styles.item}
          disabled={!hasSelection}
          onSelect={onCopy}
        >
          Copy
        </CM.Item>
        <CM.Item className={styles.item} onSelect={onPaste}>
          Paste
        </CM.Item>
        <CM.Item className={styles.item} onSelect={onSelectAll}>
          Select All
        </CM.Item>
        <CM.Item className={styles.item} onSelect={onClear}>
          Clear
        </CM.Item>
        <CM.Separator className={styles.separator} />
        <CM.Item
          className={styles.item}
          onSelect={() => safeSilent(ipc.closeTerminal(handle))}
        >
          Close Terminal
        </CM.Item>
        <PanelItems activeTerminal={handle} maximized={maximized} />
      </CM.Content>
    </CM.Portal>
  );
}
