import { useMemo, useCallback } from "react";
import { executeCommandById } from "../lib/commands";
import { PreferencesState } from "../lib/preferences";
import { MainWindowState } from "./types";
import styles from "./CommandBar.module.scss";

/** Commands to show in the bar, in display order. */
const BAR_COMMANDS = [
  "command_palette",
  "rename",
  "view",
  "edit",
  "copy",
  "move",
  "create_directory",
  "delete_selected",
  "user_commands",
];

export default function CommandBar({
  state,
  preferences,
}: {
  state: MainWindowState;
  preferences: PreferencesState;
}) {
  const items = useMemo(() => {
    return BAR_COMMANDS.map((id) => {
      const cmd = preferences.commands.find((c) => c.id === id);
      return {
        id,
        label: cmd?.short_name ?? cmd?.name ?? id,
        shortcut: cmd?.shortcut_display ?? [],
      };
    });
  }, [preferences.commands]);

  const handleClick = useCallback(
    (commandId: string) => {
      executeCommandById(commandId, state, preferences);
    },
    [state, preferences],
  );

  return (
    <div className={styles.commandBar}>
      {items.map((item) => (
        <button
          key={item.id}
          className={styles.button}
          tabIndex={-1}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => handleClick(item.id)}
        >
          <span className={styles.shortcut}>
            {item.shortcut.length > 0 ? item.shortcut.join("+") : "\u00A0"}
          </span>
          <span className={styles.label}>{item.label}</span>
        </button>
      ))}
    </div>
  );
}
