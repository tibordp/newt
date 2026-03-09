import { useMemo } from "react";
import * as CM from "@radix-ui/react-context-menu";
import { safeCommand } from "../lib/ipc";
import { usePreferences, CommandInfo } from "../lib/preferences";
import styles from "./Menu.module.scss";

type FileContextMenuProps = {
  paneHandle: number;
  isParentDir: boolean;
};

function Shortcut({ commands, id }: { commands?: CommandInfo[]; id: string }) {
  const display = commands?.find((c) => c.id === id)?.shortcut_display;
  if (!display || display.length === 0) return null;
  return <span className={styles.shortcut}>{display.join("+")}</span>;
}

export function FileContextMenuContent({
  paneHandle,
  isParentDir,
}: FileContextMenuProps) {
  const preferences = usePreferences();
  const commands = useMemo(
    () => preferences?.commands,
    [preferences?.commands],
  );

  const cmd = (command: string) => {
    safeCommand(command, { paneHandle });
  };

  return (
    <CM.Portal>
      <CM.Content className={styles.content} loop>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_open")}
        >
          Open
          <Shortcut commands={commands} id="open" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_view")}
        >
          View
          <Shortcut commands={commands} id="view" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_edit")}
        >
          Edit
          <Shortcut commands={commands} id="edit" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          onSelect={() => cmd("cmd_copy_to_clipboard")}
        >
          Copy Path
          <Shortcut commands={commands} id="copy_to_clipboard" />
        </CM.Item>

        <CM.Separator className={styles.separator} />

        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_rename")}
        >
          Rename
          <Shortcut commands={commands} id="rename" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_delete_selected")}
        >
          Delete
          <Shortcut commands={commands} id="delete_selected" />
        </CM.Item>

        <CM.Separator className={styles.separator} />

        <CM.Item
          className={styles.item}
          onSelect={() => cmd("cmd_send_to_terminal")}
        >
          Open in Terminal
          <Shortcut commands={commands} id="send_to_terminal" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_properties")}
        >
          Properties
          <Shortcut commands={commands} id="properties" />
        </CM.Item>
      </CM.Content>
    </CM.Portal>
  );
}
