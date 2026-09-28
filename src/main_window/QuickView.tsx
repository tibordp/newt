import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { memo, useEffect, useMemo, useRef, useState } from "react";

import {
  commands,
  type EncodingGroupView,
  type TableDelimiter,
  type VfsPath,
  type ViewerState,
} from "../lib/bindings";
import { normalizeKeyEvent } from "../lib/commands";
import { safe, safeSilent } from "../lib/ipc";
import { usePreferences } from "../lib/preferences";
import { useCommandShortcuts } from "../lib/scopedBindings";
import type { ViewerMode } from "../viewer/helpers";
import {
  ViewerHostContext,
  previewHost,
  type ViewerHost,
} from "../viewer/host";
import { ViewerBody } from "../viewer/Viewer";
import menuStyles from "./Menu.module.scss";
import { IconClose, IconOpenExternal } from "./modals/primitives";
import paneStyles from "./Pane.module.scss";
import styles from "./QuickView.module.scss";
import { REFOCUS_EVENT } from "./types";

const MODIFIERS = new Set(["Meta", "Control", "Shift", "Alt", "AltGraph"]);

/// Keys belong to the file list even while the preview holds DOM focus (a
/// click in it to scroll or select): the first key hands focus back to the
/// list and is replayed there. A replayed event has no default action, so
/// if it lands in a text field (quick search) its editing is done here.
function forwardKey(e: React.KeyboardEvent, root: HTMLElement) {
  const key = e.nativeEvent;
  // A modifier alone does nothing, and moving focus for it would drop the
  // preview's text selection before the chord (the copy shortcut) arrives.
  if (MODIFIERS.has(key.key)) return;
  e.preventDefault();
  e.stopPropagation();

  window.dispatchEvent(new Event(REFOCUS_EVENT));
  const target = document.activeElement;
  if (!target || root.contains(target)) return;

  const replay = new KeyboardEvent("keydown", {
    key: key.key,
    code: key.code,
    location: key.location,
    repeat: key.repeat,
    ctrlKey: key.ctrlKey,
    metaKey: key.metaKey,
    shiftKey: key.shiftKey,
    altKey: key.altKey,
    bubbles: true,
    cancelable: true,
  });
  if (!target.dispatchEvent(replay)) return;

  const field = document.activeElement;
  if (
    !(field instanceof HTMLInputElement) &&
    !(field instanceof HTMLTextAreaElement)
  ) {
    return;
  }
  if (key.key.length === 1 && !key.ctrlKey && !key.metaKey) {
    document.execCommand("insertText", false, key.key);
  } else if (key.key === "Backspace") {
    document.execCommand("delete");
  } else if (key.key === "Delete") {
    document.execCommand("forwardDelete");
  }
}

const MODES: [ViewerMode, string][] = [
  ["text", "Text"],
  ["hex", "Hex"],
  ["table", "Table"],
  ["markdown", "Markdown"],
  ["image", "Image"],
  ["audio", "Audio"],
  ["video", "Video"],
  ["pdf", "PDF"],
];
const TEXTUAL: ViewerMode[] = ["text", "table", "markdown"];
const DELIMITERS: [TableDelimiter, string][] = [
  ["comma", "Comma"],
  ["semicolon", "Semicolon"],
  ["tab", "Tab"],
  ["pipe", "Pipe"],
];

let catalogue: Promise<EncodingGroupView[]> | null = null;
function useEncodingCatalogue() {
  const [groups, setGroups] = useState<EncodingGroupView[]>([]);
  useEffect(() => {
    catalogue ??= commands.encodingCatalogue();
    void catalogue.then(setGroups);
  }, []);
  return groups;
}

/// Menus hand focus back to the list, not to their trigger.
const refocusList = (e: Event) => {
  e.preventDefault();
  window.dispatchEvent(new Event(REFOCUS_EVENT));
};

function Radio({ value, label }: { value: string; label: string }) {
  return (
    <DropdownMenu.RadioItem value={value} className={menuStyles.item}>
      <span className={menuStyles.checkColumn} aria-hidden>
        <DropdownMenu.ItemIndicator>•</DropdownMenu.ItemIndicator>
      </span>
      {label}
    </DropdownMenu.RadioItem>
  );
}

function BarMenu({
  label,
  title,
  children,
}: {
  label: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <DropdownMenu.Root modal={false}>
      <DropdownMenu.Trigger
        className={paneStyles.vfsSelector}
        tabIndex={-1}
        title={title}
      >
        {label} <span aria-hidden>&#x25BE;</span>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={menuStyles.content}
          align="start"
          sideOffset={4}
          loop
          onCloseAutoFocus={refocusList}
        >
          {children}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

/// The viewer window's View, Encoding and Table menus, for the file shown.
function PreviewBar({
  state,
  activePane,
}: {
  state: ViewerState;
  activePane: number;
}) {
  const shortcuts = useCommandShortcuts();
  const groups = useEncodingCatalogue();
  const path = state.file_path;
  const mode = state.mode as ViewerMode;
  const name = state.display_path?.split(/[\\/]/).pop() ?? "";

  const detected = state.encoding.detected;
  const autoEncoding = detected
    ? `Auto-detect (${detected.encoding}${detected.bom_len > 0 ? ", BOM" : ""})`
    : "Auto-detect";
  const table = state.table;
  const autoDelimiter = table.detected_delimiter
    ? `Auto-detect (${DELIMITERS.find(([d]) => d === table.detected_delimiter)?.[1]})`
    : "Auto-detect";
  const autoHeader =
    table.detected_header === null
      ? "Auto-detect"
      : `Auto-detect (${table.detected_header ? "Yes" : "No"})`;

  return (
    <div className={paneStyles.header}>
      <span className={paneStyles.headerPath} title={state.display_path ?? ""}>
        {name}
      </span>
      {path && (
        <>
          <BarMenu
            label={MODES.find(([m]) => m === mode)?.[1] ?? mode}
            title="View mode"
          >
            <DropdownMenu.RadioGroup
              value={mode}
              onValueChange={(m) =>
                safe(commands.setPreviewMode(path, m as ViewerMode))
              }
            >
              {MODES.map(([m, label]) => (
                <Radio key={m} value={m} label={label} />
              ))}
            </DropdownMenu.RadioGroup>
          </BarMenu>
          {TEXTUAL.includes(mode) && (
            <BarMenu
              label={
                state.encoding.selected ?? detected?.encoding ?? "Encoding"
              }
              title="Encoding"
            >
              <DropdownMenu.RadioGroup
                value={state.encoding.selected ?? ""}
                onValueChange={(v) =>
                  safe(commands.setPreviewEncoding(path, v || null))
                }
              >
                <Radio value="" label={autoEncoding} />
                <DropdownMenu.Separator className={menuStyles.separator} />
                {groups.map((group) => (
                  <DropdownMenu.Sub key={group.label}>
                    <DropdownMenu.SubTrigger className={menuStyles.item}>
                      <span className={menuStyles.checkColumn} aria-hidden>
                        {group.encodings.includes(
                          state.encoding.selected ?? "",
                        ) && "•"}
                      </span>
                      {group.label}
                      <span className={menuStyles.shortcut} aria-hidden>
                        ›
                      </span>
                    </DropdownMenu.SubTrigger>
                    <DropdownMenu.Portal>
                      <DropdownMenu.SubContent
                        className={menuStyles.content}
                        loop
                      >
                        {group.encodings.map((e) => (
                          <Radio key={e} value={e} label={e} />
                        ))}
                      </DropdownMenu.SubContent>
                    </DropdownMenu.Portal>
                  </DropdownMenu.Sub>
                ))}
              </DropdownMenu.RadioGroup>
            </BarMenu>
          )}
          {mode === "table" && (
            <BarMenu label="Table" title="Table options">
              <DropdownMenu.Label className={menuStyles.sectionHeader}>
                Delimiter
              </DropdownMenu.Label>
              <DropdownMenu.RadioGroup
                value={table.delimiter ?? "auto"}
                onValueChange={(d) =>
                  safe(commands.setPreviewTableOption(path, `delim_${d}`))
                }
              >
                <Radio value="auto" label={autoDelimiter} />
                {DELIMITERS.map(([d, label]) => (
                  <Radio key={d} value={d} label={label} />
                ))}
              </DropdownMenu.RadioGroup>
              <DropdownMenu.Separator className={menuStyles.separator} />
              <DropdownMenu.Label className={menuStyles.sectionHeader}>
                First row is header
              </DropdownMenu.Label>
              <DropdownMenu.RadioGroup
                value={
                  table.header === null ? "auto" : table.header ? "on" : "off"
                }
                onValueChange={(h) =>
                  safe(commands.setPreviewTableOption(path, `header_${h}`))
                }
              >
                <Radio value="auto" label={autoHeader} />
                <Radio value="on" label="Yes" />
                <Radio value="off" label="No" />
              </DropdownMenu.RadioGroup>
              <DropdownMenu.Separator className={menuStyles.separator} />
              <DropdownMenu.CheckboxItem
                className={menuStyles.item}
                checked={table.quoted}
                onCheckedChange={() =>
                  safe(commands.setPreviewTableOption(path, "quoted"))
                }
              >
                <span className={menuStyles.checkColumn} aria-hidden>
                  <DropdownMenu.ItemIndicator>✓</DropdownMenu.ItemIndicator>
                </span>
                Quoted fields
              </DropdownMenu.CheckboxItem>
            </BarMenu>
          )}
          <button
            type="button"
            className={paneStyles.maximizeButton}
            tabIndex={-1}
            aria-label="Open in viewer"
            title={shortcuts.label("Open in viewer", "view")}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => safe(commands.cmdView(activePane))}
          >
            <IconOpenExternal />
          </button>
        </>
      )}
      <button
        type="button"
        className={paneStyles.maximizeButton}
        tabIndex={-1}
        aria-label="Close Quick View"
        title={shortcuts.label("Close Quick View", "toggle_quick_view")}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => safe(commands.cmdToggleQuickView(0))}
      >
        <IconClose />
      </button>
    </div>
  );
}

/// One file's rendering, with a host bound to that file.
function PreviewLayer({
  state,
  host,
  visible,
  onReady,
}: {
  state: ViewerState;
  host: ViewerHost;
  visible: boolean;
  onReady?: () => void;
}) {
  const filePath = state.file_path!;
  return (
    <div className={visible ? styles.layer : styles.hiddenLayer}>
      <ViewerHostContext.Provider value={host}>
        <ViewerBody
          displayPath={state.display_path ?? ""}
          filePath={filePath}
          fileServerBase={state.file_server_base ?? ""}
          viewerState={state}
          onReady={onReady}
        />
      </ViewerHostContext.Provider>
    </div>
  );
}

type Props = {
  state: ViewerState;
  panesFocused: boolean;
  activePane: number;
};

/// The right slot while Quick View is on: the active pane's focused file,
/// rendered by the viewer without its keys or focus. Memoized: every cursor
/// move patches the main window's state, and should re-render the viewer
/// only when the preview itself changed.
function QuickView({ state, panesFocused, activePane }: Props) {
  const rootRef = useRef<HTMLDivElement>(null);

  // The file on screen stays there until the next one is ready to show,
  // which loads in a hidden layer beneath it meanwhile.
  const key = state.file_path ? JSON.stringify(state.file_path) : null;
  const [shownKey, setShownKey] = useState<string | null>(key);
  const shownState = useRef(state);
  if (key === shownKey) shownState.current = state;
  useEffect(() => {
    if (key === null) setShownKey(null);
  }, [key]);
  const shown = key === shownKey ? state : shownState.current;

  // One host per file on screen or loading, so the copy shortcut can reach
  // the shown viewer's selection.
  const hosts = useRef(new Map<string, ViewerHost>());
  const hostFor = (k: string, path: VfsPath) => {
    let host = hosts.current.get(k);
    if (!host) {
      host = previewHost(path);
      hosts.current.set(k, host);
    }
    return host;
  };
  useEffect(() => {
    for (const k of hosts.current.keys()) {
      if (k !== shownKey && k !== key) hosts.current.delete(k);
    }
  });

  // The pane's copy shortcut copies what's selected in the preview, if
  // anything is, wherever focus is — the text, hex, image and table
  // viewers select without taking focus. Otherwise it goes on to the list
  // and copies the path.
  const preferences = usePreferences();
  const copyKeys = useMemo(
    () =>
      new Set(
        (preferences?.bindings ?? [])
          .filter((b) => b.command === "copy_to_clipboard")
          .map((b) => b.key),
      ),
    [preferences],
  );
  const copyPreviewSelection = () => {
    const root = rootRef.current;
    const selection = window.getSelection();
    const text = selection?.toString() ?? "";
    if (root && text && root.contains(selection!.anchorNode)) {
      void navigator.clipboard.writeText(text);
      return true;
    }
    return (
      shownKey !== null &&
      (hosts.current.get(shownKey)?.copySelection.current?.() ?? false)
    );
  };

  const copyRef = useRef({ copyKeys, copyPreviewSelection });
  copyRef.current = { copyKeys, copyPreviewSelection };
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const { copyKeys, copyPreviewSelection } = copyRef.current;
      if (!copyKeys.has(normalizeKeyEvent(e))) return;
      const target = e.target;
      const outsideField =
        (target instanceof HTMLInputElement ||
          target instanceof HTMLTextAreaElement) &&
        !rootRef.current?.contains(target);
      if (outsideField || !copyPreviewSelection()) return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);

  const onKeyDownCapture = (e: React.KeyboardEvent) => {
    if (rootRef.current) forwardKey(e, rootRef.current);
  };

  return (
    <div
      ref={rootRef}
      className={styles.quickView}
      data-quick-view
      role="region"
      aria-label="Quick View"
      tabIndex={-1}
      onKeyDownCapture={onKeyDownCapture}
      onMouseDown={() => {
        if (!panesFocused) safeSilent(commands.cmdFocusPanes(0));
      }}
    >
      <PreviewBar
        state={key === null ? state : shown}
        activePane={activePane}
      />
      <div className={styles.content}>
        {key === null || shownKey === null ? (
          <div className={styles.placeholder}>No preview</div>
        ) : (
          <PreviewLayer
            key={shownKey}
            state={shown}
            host={hostFor(shownKey, shown.file_path!)}
            visible
          />
        )}
        {key !== null && key !== shownKey && (
          <PreviewLayer
            key={key}
            state={state}
            host={hostFor(key, state.file_path!)}
            visible={false}
            onReady={() => setShownKey(key)}
          />
        )}
      </div>
    </div>
  );
}

export default memo(QuickView);
