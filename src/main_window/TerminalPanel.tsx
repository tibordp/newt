import { commands } from "../lib/bindings";
import { safeSilent } from "../lib/ipc";
import { useCommandShortcuts } from "../lib/scopedBindings";
import { IconMaximize, IconRestore } from "./modals/primitives";
import Terminal from "./Terminal";
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
  return (
    <div className={styles.panel}>
      <div className={styles.tabBar}>
        {terminals.map((term, i) => (
          <button
            key={term.handle}
            className={`${styles.tab} ${term.handle === activeTerminal ? styles.active : ""} ${term.defunct ? styles.defunct : ""}`}
            tabIndex={-1}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => safeSilent(commands.activateTerminal(term.handle))}
          >
            <span>
              Terminal {i + 1}
              {term.defunct ? " (exited)" : ""}
            </span>
            <span
              className={styles.tabClose}
              onClick={(e) => {
                e.stopPropagation();
                safeSilent(commands.closeTerminal(term.handle));
              }}
            >
              ×
            </span>
          </button>
        ))}
        <button
          className={styles.addButton}
          tabIndex={-1}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => safeSilent(commands.cmdCreateTerminal(0))}
          title="New Terminal"
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
          title={shortcuts.label(
            maximized ? "Restore split layout" : "Maximize terminal",
            "toggle_maximized",
          )}
        >
          {maximized ? <IconRestore /> : <IconMaximize />}
        </button>
      </div>
      <div className={styles.content}>
        {terminals.map((term) => (
          <div
            key={term.handle}
            className={`${styles.terminalWrapper} ${term.handle !== activeTerminal ? styles.hidden : ""}`}
          >
            <Terminal
              handle={term.handle}
              active={!panesFocused && term.handle === activeTerminal}
              visible={term.handle === activeTerminal}
              modalOpen={modalOpen}
              defunct={term.defunct}
            />
          </div>
        ))}
      </div>
    </div>
  );
}
