import { safeCommandSilent } from "../lib/ipc";
import Terminal from "./Terminal";
import styles from "./TerminalPanel.module.scss";
import type { Terminal as TerminalType } from "./types";

type Props = {
  terminals: TerminalType[];
  activeTerminal?: number;
  panesFocused: boolean;
  modalOpen: boolean;
};

export default function TerminalPanel({
  terminals,
  activeTerminal,
  panesFocused,
  modalOpen,
}: Props) {
  return (
    <div className={styles.panel}>
      <div className={styles.tabBar}>
        {terminals.map((term, i) => (
          <button
            key={term.handle}
            className={`${styles.tab} ${term.handle === activeTerminal ? styles.active : ""}`}
            onClick={() =>
              safeCommandSilent("activate_terminal", { handle: term.handle })
            }
          >
            <span>Terminal {i + 1}</span>
            <span
              className={styles.tabClose}
              onClick={(e) => {
                e.stopPropagation();
                safeCommandSilent("close_terminal", { handle: term.handle });
              }}
            >
              ×
            </span>
          </button>
        ))}
        <button
          className={styles.addButton}
          onClick={() => safeCommandSilent("create_terminal")}
          title="New Terminal"
        >
          +
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
            />
          </div>
        ))}
      </div>
    </div>
  );
}
