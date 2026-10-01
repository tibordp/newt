import { useId } from "react";
import { ContextMenu as CM } from "../lib/menus";
import { commands } from "../lib/bindings";
import { safeSilent } from "../lib/ipc";
import { useCommandShortcuts } from "../lib/scopedBindings";
import { IconClose, IconMaximize, IconRestore } from "./modals/primitives";
import Terminal from "./Terminal";
import { TabBarMenuContent, TabMenuContent } from "./TerminalMenus";
import styles from "./TerminalPanel.module.scss";
import type { Terminal as TerminalType } from "./types";

type Props = {
  terminals: TerminalType[];
  activeTerminal: number | null;
  panesFocused: boolean;
  modalOpen: boolean;
  /// The panel is shown alone rather than below the panes.
  maximized: boolean;
};

export default function TerminalPanel({
  terminals,
  activeTerminal,
  panesFocused,
  modalOpen,
  maximized,
}: Props) {
  const shortcuts = useCommandShortcuts();
  const idBase = useId();
  const tabId = (handle: number) => `${idBase}-tab-${handle}`;
  const panelId = (handle: number) => `${idBase}-panel-${handle}`;
  return (
    <div className={styles.panel} role="region" aria-label="Terminal panel">
      <CM.Root>
        <CM.Trigger asChild>
          <div
            className={`${styles.tabBar} ${panesFocused ? "" : styles.tabBarFocused}`}
          >
            <div className={styles.tabs} role="tablist" aria-label="Terminals">
              {terminals.map((term, i) => (
                <CM.Root key={term.handle}>
                  <CM.Trigger asChild>
                    <button
                      id={tabId(term.handle)}
                      className={`${styles.tab} ${term.handle === activeTerminal ? styles.active : ""} ${term.defunct ? styles.defunct : ""}`}
                      role="tab"
                      aria-selected={term.handle === activeTerminal}
                      aria-controls={panelId(term.handle)}
                      tabIndex={-1}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() =>
                        safeSilent(commands.activateTerminal(term.handle))
                      }
                    >
                      <span>
                        Terminal {i + 1}
                        {term.defunct ? " (exited)" : ""}
                      </span>
                      <span
                        className={styles.tabClose}
                        title="Close terminal"
                        aria-hidden
                        onClick={(e) => {
                          e.stopPropagation();
                          safeSilent(commands.closeTerminal(term.handle));
                        }}
                      >
                        <IconClose />
                      </span>
                    </button>
                  </CM.Trigger>
                  <TabMenuContent
                    handle={term.handle}
                    handles={terminals.map((t) => t.handle)}
                    maximized={maximized}
                  />
                </CM.Root>
              ))}
            </div>
            <button
              className={styles.addButton}
              tabIndex={-1}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => safeSilent(commands.cmdCreateTerminal(0))}
              aria-label="New terminal"
              title={shortcuts.label("New terminal", "create_terminal")}
            >
              +
            </button>
            <button
              className={styles.maximizeButton}
              tabIndex={-1}
              onMouseDown={(e) => e.preventDefault()}
              onClick={async () => {
                // What maximizes is whatever has focus, so claim it first.
                if (activeTerminal != null) {
                  await safeSilent(commands.activateTerminal(activeTerminal));
                }
                safeSilent(commands.cmdToggleMaximized(0));
              }}
              aria-label={
                maximized ? "Restore split layout" : "Maximize terminal"
              }
              title={shortcuts.label(
                maximized ? "Restore split layout" : "Maximize terminal",
                "toggle_maximized",
              )}
            >
              {maximized ? <IconRestore /> : <IconMaximize />}
            </button>
          </div>
        </CM.Trigger>
        <TabBarMenuContent
          activeTerminal={activeTerminal}
          maximized={maximized}
        />
      </CM.Root>
      <div className={styles.content}>
        {terminals.map((term) => (
          <div
            key={term.handle}
            id={panelId(term.handle)}
            role="tabpanel"
            aria-labelledby={tabId(term.handle)}
            className={`${styles.terminalWrapper} ${term.handle !== activeTerminal ? styles.hidden : ""}`}
          >
            <Terminal
              handle={term.handle}
              active={!panesFocused && term.handle === activeTerminal}
              visible={term.handle === activeTerminal}
              modalOpen={modalOpen}
              defunct={term.defunct}
              maximized={maximized}
            />
          </div>
        ))}
      </div>
    </div>
  );
}
