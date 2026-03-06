import * as CM from "@radix-ui/react-context-menu";
import { safeCommand } from "../lib/ipc";
import styles from "./Menu.module.scss";

type FileContextMenuProps = {
  paneHandle: number;
  isParentDir: boolean;
};

export function FileContextMenuContent({
  paneHandle,
  isParentDir,
}: FileContextMenuProps) {
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
          Open<span className={styles.shortcut}>Enter</span>
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_view")}
        >
          View<span className={styles.shortcut}>F3</span>
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_edit")}
        >
          Edit<span className={styles.shortcut}>F4</span>
        </CM.Item>
        <CM.Item
          className={styles.item}
          onSelect={() => cmd("cmd_copy_to_clipboard")}
        >
          Copy Path
        </CM.Item>

        <CM.Separator className={styles.separator} />

        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_rename")}
        >
          Rename<span className={styles.shortcut}>F2</span>
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_delete_selected")}
        >
          Delete<span className={styles.shortcut}>Del</span>
        </CM.Item>

        <CM.Separator className={styles.separator} />

        <CM.Item
          className={styles.item}
          onSelect={() => cmd("cmd_send_to_terminal")}
        >
          Open in Terminal
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => cmd("cmd_properties")}
        >
          Properties<span className={styles.shortcut}>Alt+Enter</span>
        </CM.Item>
      </CM.Content>
    </CM.Portal>
  );
}
