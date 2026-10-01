import { useMemo, useCallback } from "react";
import { ContextMenu as CM } from "../lib/menus";
import { commands } from "../lib/bindings";
import { safe } from "../lib/ipc";
import { executeCommandById } from "../lib/commands";
import { PreferencesState } from "../lib/preferences";
import { MainWindowState } from "./types";
import styles from "./CommandBar.module.scss";
import menuStyles from "./Menu.module.scss";

/** Commands to show in the bar, in display order. */
const BAR_COMMANDS = [
  "connect_remote",
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
        name: (cmd?.name ?? id).replace(/(\.\.\.|…)$/, ""),
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
    <CM.Root>
      <CM.Trigger asChild>
        <div className={styles.commandBar}>
          {items.map((item) => (
            <button
              key={item.id}
              className={styles.button}
              tabIndex={-1}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => handleClick(item.id)}
              title={item.name !== item.label ? item.name : undefined}
            >
              <span className={styles.shortcut}>
                {item.shortcut.length > 0 ? item.shortcut.join("+") : "\u00A0"}
              </span>
              <span className={styles.label}>{item.label}</span>
            </button>
          ))}
        </div>
      </CM.Trigger>
      <CM.Portal>
        <CM.Content className={menuStyles.content} loop>
          <CM.Item
            className={menuStyles.item}
            onSelect={() =>
              safe(
                commands.updatePreference("appearance.show_command_bar", false),
              )
            }
          >
            Hide Command Bar
          </CM.Item>
        </CM.Content>
      </CM.Portal>
    </CM.Root>
  );
}
