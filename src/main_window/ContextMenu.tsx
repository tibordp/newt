import { useMemo } from "react";
import * as CM from "@radix-ui/react-context-menu";

import { commands as ipc, type MetadataTraits } from "../lib/bindings";
import { safe } from "../lib/ipc";
import { usePreferences, CommandInfo } from "../lib/preferences";
import {
  COLUMN_CHOICES,
  TIMESTAMP_BASES,
  TIMESTAMP_STATE_LABELS,
  TRAIT_GATES,
  TimestampColumnState,
  getTimestampState,
  insertColumnKey,
  isTimestampPart,
  setTimestampState,
} from "./columns";
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

type ColumnsContextMenuProps = {
  columns?: string[];
  /// The pane's VFS metadata traits — trait-gated columns the VFS can't
  /// populate are omitted from the picker (they wouldn't render anyway).
  traits?: MetadataTraits;
  onCloseAutoFocus?: (e: Event) => void;
};

function TimestampSubmenu({
  base,
  label,
  current,
  onChange,
}: {
  base: string;
  label: string;
  current: string[];
  onChange: (base: string, state: TimestampColumnState) => void;
}) {
  const state = getTimestampState(current, base);
  return (
    <CM.Sub>
      <CM.SubTrigger className={styles.item}>
        <span className={styles.checkColumn}>{state !== "hidden" && "✓"}</span>
        {label}
        <span className={styles.shortcut}>
          {TIMESTAMP_STATE_LABELS[state]} ›
        </span>
      </CM.SubTrigger>
      <CM.Portal>
        <CM.SubContent className={styles.content} loop>
          <CM.RadioGroup
            value={state}
            onValueChange={(v) => onChange(base, v as TimestampColumnState)}
          >
            {(
              Object.keys(TIMESTAMP_STATE_LABELS) as TimestampColumnState[]
            ).map((s) => (
              <CM.RadioItem
                key={s}
                value={s}
                className={styles.item}
                onSelect={(e) => e.preventDefault()}
              >
                <span className={styles.checkColumn}>
                  <CM.ItemIndicator>•</CM.ItemIndicator>
                </span>
                {TIMESTAMP_STATE_LABELS[s]}
              </CM.RadioItem>
            ))}
          </CM.RadioGroup>
        </CM.SubContent>
      </CM.Portal>
    </CM.Sub>
  );
}

/// Quick column visibility picker for the column header row. Writes the
/// `appearance.columns` preference; the preference reload then re-renders
/// every pane. Simple columns are checkboxes; each timestamp gets a
/// submenu choosing between compound, date-only, split, and hidden. Stays
/// open across toggles so several columns can be flipped in one visit.
export function ColumnsContextMenuContent({
  columns,
  traits,
  onCloseAutoFocus,
}: ColumnsContextMenuProps) {
  // An empty preference list means "all columns" (getVisibleColumns fallback).
  const current =
    columns && columns.length > 0 ? columns : COLUMN_CHOICES.map((c) => c.key);
  const choices = COLUMN_CHOICES.filter((col) => {
    const gate = TRAIT_GATES[col.key];
    return !gate || !traits || traits[gate];
  });

  const toggle = (key: string, checked: boolean) => {
    const next = checked
      ? insertColumnKey(current, key)
      : current.filter((k) => k !== key);
    safe(ipc.updatePreference("appearance.columns", next));
  };

  const setTimestamp = (base: string, state: TimestampColumnState) => {
    safe(
      ipc.updatePreference(
        "appearance.columns",
        setTimestampState(current, base, state),
      ),
    );
  };

  return (
    <CM.Portal>
      <CM.Content
        className={styles.content}
        loop
        onCloseAutoFocus={onCloseAutoFocus}
      >
        {choices.map((col) => {
          if (TIMESTAMP_BASES.includes(col.key)) {
            return (
              <TimestampSubmenu
                key={col.key}
                base={col.key}
                label={col.label}
                current={current}
                onChange={setTimestamp}
              />
            );
          }
          // Date/time parts are covered by their base's submenu
          if (isTimestampPart(col.key)) {
            return null;
          }
          return (
            <CM.CheckboxItem
              key={col.key}
              className={styles.item}
              checked={current.includes(col.key)}
              disabled={col.key === "name"}
              onSelect={(e) => e.preventDefault()}
              onCheckedChange={(checked) => toggle(col.key, checked === true)}
            >
              <span className={styles.checkColumn}>
                <CM.ItemIndicator>✓</CM.ItemIndicator>
              </span>
              {col.label}
            </CM.CheckboxItem>
          );
        })}
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
