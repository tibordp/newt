import { useMemo } from "react";
import * as CM from "@radix-ui/react-context-menu";

import { commands as ipc } from "../lib/bindings";
import { safe } from "../lib/ipc";
import { usePreferences, CommandInfo } from "../lib/preferences";
import styles from "./Menu.module.scss";

function Shortcut({ commands, id }: { commands?: CommandInfo[]; id: string }) {
  const display = commands?.find((c) => c.id === id)?.shortcut_display;
  if (!display || display.length === 0) return null;
  return <span className={styles.shortcut}>{display.join("+")}</span>;
}

function useCommands() {
  const preferences = usePreferences();
  return useMemo(() => preferences?.commands, [preferences?.commands]);
}

type FileContextMenuProps = {
  paneHandle: number;
  isParentDir: boolean;
};

export function FileContextMenuContent({
  paneHandle,
  isParentDir,
}: FileContextMenuProps) {
  const commands = useCommands();

  return (
    <CM.Portal>
      <CM.Content className={styles.content} loop>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => safe(ipc.cmdOpen(paneHandle))}
        >
          Open
          <Shortcut commands={commands} id="open" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => safe(ipc.cmdView(paneHandle))}
        >
          View
          <Shortcut commands={commands} id="view" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => safe(ipc.cmdEdit(paneHandle))}
        >
          Edit
          <Shortcut commands={commands} id="edit" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          onSelect={() => safe(ipc.cmdCopyToClipboard(paneHandle))}
        >
          Copy Path
          <Shortcut commands={commands} id="copy_to_clipboard" />
        </CM.Item>

        <CM.Separator className={styles.separator} />

        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => safe(ipc.cmdRename(paneHandle))}
        >
          Rename
          <Shortcut commands={commands} id="rename" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => safe(ipc.cmdDeleteSelected(paneHandle))}
        >
          Delete
          <Shortcut commands={commands} id="delete_selected" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => safe(ipc.cmdDeletePermanent(paneHandle))}
        >
          Delete Permanently
          <Shortcut commands={commands} id="delete_permanent" />
        </CM.Item>

        <CM.Separator className={styles.separator} />

        <CM.Item
          className={styles.item}
          onSelect={() => safe(ipc.cmdSendToTerminal(paneHandle))}
        >
          Open in Terminal
          <Shortcut commands={commands} id="send_to_terminal" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          disabled={isParentDir}
          onSelect={() => safe(ipc.cmdProperties(paneHandle))}
        >
          Properties
          <Shortcut commands={commands} id="properties" />
        </CM.Item>
      </CM.Content>
    </CM.Portal>
  );
}

type PaneContextMenuProps = {
  paneHandle: number;
  isHostLocal: boolean;
};

export function PaneContextMenuContent({
  paneHandle,
  isHostLocal,
}: PaneContextMenuProps) {
  const commands = useCommands();

  return (
    <CM.Portal>
      <CM.Content className={styles.content} loop>
        {isHostLocal && (
          <>
            <CM.Item
              className={styles.item}
              onSelect={() => safe(ipc.cmdOpenFolder(paneHandle))}
            >
              Open in Default App
              <Shortcut commands={commands} id="open_folder" />
            </CM.Item>
            <CM.Separator className={styles.separator} />
          </>
        )}

        <CM.Item
          className={styles.item}
          onSelect={() => safe(ipc.cmdCreateDirectory(paneHandle))}
        >
          New Directory
          <Shortcut commands={commands} id="create_directory" />
        </CM.Item>
        <CM.Item
          className={styles.item}
          onSelect={() => safe(ipc.cmdCreateFile(paneHandle))}
        >
          New File
          <Shortcut commands={commands} id="create_file" />
        </CM.Item>

        <CM.Separator className={styles.separator} />

        <CM.Item
          className={styles.item}
          onSelect={() => safe(ipc.cmdDirectoryProperties(paneHandle))}
        >
          Directory Properties
        </CM.Item>
      </CM.Content>
    </CM.Portal>
  );
}

type BreadcrumbContextMenuProps = {
  displayPath: string;
};

export function BreadcrumbContextMenuContent({
  displayPath,
}: BreadcrumbContextMenuProps) {
  return (
    <CM.Portal>
      <CM.Content className={styles.content} loop>
        <CM.Item
          className={styles.item}
          onSelect={() => navigator.clipboard.writeText(displayPath)}
        >
          Copy Path
        </CM.Item>
      </CM.Content>
    </CM.Portal>
  );
}
