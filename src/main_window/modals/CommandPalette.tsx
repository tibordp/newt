import { Fragment, useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";
import { safe, safeCommand } from "../../lib/ipc";
import { PreferencesState } from "../../lib/preferences";
import { MainWindowState } from "../types";
import { Palette, Highlight, fuzzyMatch } from "./Palette";
import styles from "./CommandPalette.module.scss";
import { commands } from "../../lib/bindings";

const preventAutoFocus = (e: Event) => e.preventDefault();

function matchesAppliesToCondition(
  command: { applies_to?: string | null },
  state: MainWindowState | null,
): boolean {
  if (!command.applies_to || command.applies_to === "any") return true;
  if (!state || !state.panes) return true;

  const pane = state.panes[state.display_options.active_pane];
  if (!pane) return true;

  const focused = pane.focused
    ? pane.file_window.items.find((f) => f.name === pane.focused)
    : undefined;

  switch (command.applies_to) {
    case "file":
      return !!focused && !focused.is_dir;
    case "directory":
      return !!focused && focused.is_dir;
    case "selection":
      return (
        pane.stats.selected_file_count + pane.stats.selected_dir_count > 0 ||
        (!!focused && focused.name !== "..")
      );
    default:
      return true;
  }
}

export default function CommandPalette({
  preferences,
  state,
  categoryFilter,
}: {
  preferences: PreferencesState | null;
  state: MainWindowState | null;
  categoryFilter?: string | null;
}) {
  const [filter, setFilter] = useState("");

  const paneHandle =
    state?.display_options.panes_focused && state?.display_options.active_pane;

  const allCommands = preferences?.commands ?? [];

  const filteredCommands = useMemo(() => {
    let ret = allCommands.map((command) => ({
      ...fuzzyMatch(filter, command.name),
      command,
    }));

    ret = ret.filter(
      ({ matches, command }) =>
        matches &&
        // Hide internal commands from palette
        command.id !== "command_palette" &&
        command.id !== "hot_paths" &&
        command.id !== "user_commands" &&
        (!command.needs_pane || !!paneHandle || paneHandle === 0) &&
        // Category filter (e.g. F9 → "User" only)
        (!categoryFilter || command.category === categoryFilter) &&
        // When condition filtering for user commands
        matchesAppliesToCondition(command, state),
    );
    ret.sort((a, b) => a.score - b.score);
    return ret.map(({ command }) => command);
  }, [filter, paneHandle, allCommands, categoryFilter, state]);

  const onSelect = (value: string) => {
    const index = parseInt(value, 10);
    const command = filteredCommands[index];
    if (!command) return;

    if (command.id.startsWith("user_command_")) {
      const cmdIndex = parseInt(command.id.replace("user_command_", ""), 10);
      safe(commands.runUserCommand(paneHandle || 0, cmdIndex));
    } else {
      safeCommand("cmd_" + command.id, {
        paneHandle: paneHandle || 0,
      });
    }
  };

  return (
    <Dialog.Content
      className={styles.content}
      onCloseAutoFocus={preventAutoFocus}
    >
      <Dialog.Title className="sr-only">Command Palette</Dialog.Title>
      <Palette shouldFilter={false}>
        <div className={styles.header}>
          <Command.Input
            value={filter}
            onValueChange={setFilter}
            placeholder="Start typing to filter commands"
          />
        </div>
        <Command.List>
          <Command.Empty>No commands found</Command.Empty>
          {filteredCommands.map((command, i) => (
            <Command.Item
              key={`${command.id}-${i}`}
              value={String(i)}
              onSelect={onSelect}
            >
              <span>
                <Highlight
                  text={command.name}
                  filter={filter}
                  highlightClass={styles.highlight}
                />
                {!categoryFilter && command.category === "User" && (
                  <span className={styles.badge}>User</span>
                )}
              </span>
              {command.shortcut_display.length > 0 && (
                <div className={styles.shortcut}>
                  {command.shortcut_display.map((e, i) => (
                    <Fragment key={i}>
                      {i !== 0 ? " + " : ""}
                      <kbd>{e}</kbd>
                    </Fragment>
                  ))}
                </div>
              )}
            </Command.Item>
          ))}
        </Command.List>
      </Palette>
    </Dialog.Content>
  );
}
