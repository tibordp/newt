import { Fragment, useMemo, useState, ReactElement } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Command } from "cmdk";
import { safeCommand } from "../../lib/ipc";
import { PreferencesState } from "../../lib/preferences";
import { MainWindowState } from "../types";
import styles from "./CommandPalette.module.scss";

type CommandPaletteProps = {
  open: boolean;
  preferences: PreferencesState | null;
  state: MainWindowState | null;
  onClose: () => void;
  onCloseAutoFocus: (e: Event) => void;
};

function Highlight(props: { name: string; filter: string }) {
  const { name, filter } = props;
  let key = 0;
  let a = 0;
  let b = 0;
  const parts: ReactElement[] = [];
  while (a < filter.length && b < name.length) {
    if (filter[a].toLowerCase() === name[b].toLowerCase()) {
      parts.push(
        <span key={key++} className={styles.highlight}>
          {name[b]}
        </span>,
      );
      a++;
      b++;
    } else {
      parts.push(<span key={key++}>{name[b]}</span>);
      b++;
    }
  }

  if (b < name.length) {
    parts.push(<span key={key}>{name.slice(b)}</span>);
  }

  return <span>{parts}</span>;
}

export default function CommandPalette({
  open,
  preferences,
  state,
  onClose,
  onCloseAutoFocus,
}: CommandPaletteProps) {
  const [filter, setFilter] = useState("");

  const paneHandle =
    state?.display_options.panes_focused && state?.display_options.active_pane;

  const allCommands = preferences?.commands ?? [];

  const filteredCommands = useMemo(() => {
    let ret = allCommands.map((command) => {
      let a = 0;
      let b = 0;
      let consecutive = 0;
      let maxConsecutive = 0;

      while (a < filter.length && b < command.name.length) {
        if (filter[a].toLowerCase() === command.name[b].toLowerCase()) {
          consecutive++;
          a++;
          b++;
        } else {
          maxConsecutive = Math.max(maxConsecutive, consecutive);
          consecutive = 0;
          b++;
        }
      }

      return {
        matches: a === filter.length,
        score: maxConsecutive,
        command: command,
      };
    });

    ret = ret.filter(
      ({ matches, command }) =>
        matches &&
        // Hide internal commands from palette
        command.id !== "command_palette" &&
        command.id !== "hot_paths" &&
        (!command.needs_pane || !!paneHandle || paneHandle === 0),
    );
    ret.sort((a, b) => a.score - b.score);
    return ret.map(({ command }) => command);
  }, [filter, paneHandle, allCommands]);

  const onSelect = (value: string) => {
    const index = parseInt(value, 10);
    const command = filteredCommands[index];
    if (!command) return;

    safeCommand("cmd_" + command.id, {
      paneHandle: paneHandle || 0,
    });
  };

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(o) => {
        if (!o) onClose();
      }}
    >
      <Dialog.Portal>
        <Dialog.Content
          className={styles.content}
          onCloseAutoFocus={onCloseAutoFocus}
        >
          <Dialog.Title className="sr-only">Command Palette</Dialog.Title>
          <Command shouldFilter={false}>
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
                  <Highlight name={command.name} filter={filter} />
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
          </Command>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
