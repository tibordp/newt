import {
  useState,
  useEffect,
  useRef,
  useMemo,
  useLayoutEffect,
  useCallback,
  useId,
  memo,
  Fragment,
} from "react";
import { DropdownMenu } from "../lib/menus";
import { ContextMenu, isKeyboardContextMenu, openMenuAt } from "../lib/menus";
import { ownsEvent } from "../lib/events";
import { useAfterGrace } from "../lib/useAfterGrace";
import iconMapping from "../assets/mapping.json";
import { fileIconGlyph } from "../lib/fileIcons";
import { commands, type Result, type RowActions } from "../lib/bindings";
import { rawCommands, safe, safeSilent } from "../lib/ipc";
import { modifiers, normalizeKeyEvent, numpadOperator } from "../lib/commands";
import { PaneBreadcrumb, VfsTarget, HistoryEntryView } from "../lib/types";
import HistoryNavigator from "./modals/HistoryNavigator";
import SortMenu from "./modals/SortMenu";
import type { ModalData } from "../lib/bindings";
import {
  FileView,
  ColumnDef,
  ContextBadge,
  FilterMode,
  PaneState,
  DndFileInfo,
  FileRowContext,
  KEYBOARD_MENU_EVENT,
  REFOCUS_EVENT,
} from "./types";
import type { VfsProgress } from "../lib/bindings";
import { useFormatBytes, useSizeUnits } from "../lib/size";
import type { SizeUnits } from "../lib/bindings";
import {
  ColumnHeader,
  fitColumnWidth,
  getVisibleColumns,
  moveColumn,
} from "./columns";
import { rowHeightFor } from "./density";
import { usePreferences } from "../lib/preferences";
import { useCommandShortcuts } from "../lib/scopedBindings";
import { IconMaximize, IconPaneSide, IconRestore } from "./modals/primitives";
import { useLocale } from "../lib/locale";
import { useRuntimeState } from "../lib/runtimeState";
import { useWheelScroll } from "../lib/useWheelScroll";
import {
  FileContextMenuContent,
  PaneContextMenuContent,
  LocationContextMenuContent,
  ColumnsContextMenuContent,
  type FollowKind,
} from "./ContextMenu";
import styles from "./Pane.module.scss";
import menuStyles from "./Menu.module.scss";
import columnStyles from "./Columns.module.scss";
import Spinner from "./Spinner";
import { visibleName } from "../lib/bidi";

function PathBreadcrumbs(props: {
  breadcrumbs: PaneBreadcrumb[];
  paneHandle: number;
  displayPath: string;
  onMenuCloseAutoFocus: (e: Event) => void;
}) {
  const { breadcrumbs, paneHandle, displayPath, onMenuCloseAutoFocus } = props;
  const shortcuts = useCommandShortcuts();

  // The last breadcrumb gets the full display_path; earlier ones join labels.
  const pathUpTo = (index: number): string => {
    if (index === breadcrumbs.length - 1) return displayPath;
    return breadcrumbs
      .slice(0, index + 1)
      .map((c) => c.label)
      .join("");
  };

  return (
    <>
      {breadcrumbs.map((crumb, i) => {
        const isLast = i === breadcrumbs.length - 1;
        return (
          <Fragment key={i}>
            <ContextMenu.Root>
              <ContextMenu.Trigger asChild>
                <a
                  className={`${styles.pathBreadcrumb} ${isLast ? styles.pathBreadcrumbCurrent : ""}`}
                  href="#"
                  tabIndex={-1}
                  aria-current={isLast ? "location" : undefined}
                  title={
                    isLast
                      ? shortcuts.label("Go to path", "navigate")
                      : undefined
                  }
                  onClick={(e) => {
                    e.preventDefault();
                    if (isLast) {
                      commands.dialog("navigate", paneHandle);
                    } else {
                      safe(commands.navigateToPath(paneHandle, crumb.path));
                    }
                  }}
                >
                  {visibleName(crumb.label)}
                </a>
              </ContextMenu.Trigger>
              <LocationContextMenuContent
                paneHandle={paneHandle}
                displayPath={pathUpTo(i)}
                onCloseAutoFocus={onMenuCloseAutoFocus}
              />
            </ContextMenu.Root>
            {crumb.note && (
              <span
                className={styles.pathBreadcrumbNote}
                title={crumb.note.details
                  .map((d) => `${d.label}: ${d.value}`)
                  .join("\n")}
              >
                {` ${crumb.note.text}`}
              </span>
            )}
          </Fragment>
        );
      })}
    </>
  );
}

type DragMode = "normal" | "ctrl" | "shift";

type DragState = {
  active: boolean;
  startX: number;
  startY: number;
  startScrollX: number;
  startScrollY: number;
  mode: DragMode;
  lastSentStartIdx: number;
  lastSentEndIdx: number;
  lastClientY: number;
  lastCurScrollX: number;
  scrollIntervalId: number | null;
};

type LocalDndState = {
  active: boolean;
  startX: number;
  startY: number;
  files: DndFileInfo[];
  /** An escalation to a native OS drag is in flight (or done). */
  escalating?: boolean;
};

const iconDefs = iconMapping.iconDefinitions as unknown as Record<
  string,
  { fontCharacter: string; fontColor: string }
>;

function getFileIconChar(
  name: string,
  isDir: boolean,
): { ch: string; color: string } {
  if (isDir) return { ch: "\uE5FF", color: "" };
  return fileIconGlyph(name);
}

/**
 * Render the drag ghost equivalent (icon glyph + name, or "N items") to PNG
 * bytes for the native drag-out preview. Returns [] on any failure — the
 * backend falls back to the app icon (drag-rs aborts on invalid bytes).
 * Rendered at 1x: canvas PNGs carry no DPI metadata, so a 2x bitmap would
 * draw double-size; slight blur on retina displays is accepted.
 */
async function renderDragImage(files: DndFileInfo[]): Promise<Uint8Array> {
  try {
    const label = files.length === 1 ? files[0].name : `${files.length} items`;
    const bodyStyle = getComputedStyle(document.body);
    const font = `${bodyStyle.fontSize} ${bodyStyle.fontFamily}`;
    const single = files.length === 1 && !files[0].is_dir;
    const iconW = single ? 22 : 0;
    const padX = 8;
    const h = 24;

    const canvas = document.createElement("canvas");
    const c = canvas.getContext("2d");
    if (!c) return new Uint8Array();
    c.font = font;
    const textW = Math.ceil(c.measureText(label).width);
    canvas.width = Math.min(padX * 2 + iconW + textW, 320);
    canvas.height = h;

    // Context state resets when the canvas is resized — set everything after.
    c.globalAlpha = 0.9;
    c.fillStyle = bodyStyle.backgroundColor;
    c.beginPath();
    c.roundRect(0, 0, canvas.width, canvas.height, 6);
    c.fill();
    c.globalAlpha = 1;
    c.textBaseline = "middle";
    if (single) {
      const { ch, color } = getFileIconChar(files[0].name, false);
      c.font = '16px "seti"';
      c.fillStyle = color || bodyStyle.color;
      c.fillText(ch, padX, h / 2);
    }
    c.font = font;
    c.fillStyle = bodyStyle.color;
    c.fillText(label, padX + iconW, h / 2, canvas.width - padX * 2 - iconW);

    const blob = await new Promise<Blob | null>((resolve) =>
      canvas.toBlob(resolve, "image/png"),
    );
    if (!blob) return new Uint8Array();
    return new Uint8Array(await blob.arrayBuffer());
  } catch {
    return new Uint8Array();
  }
}

// Enricher annotations are tiny (0–2 entries) but structurally fresh on
// every window projection, so the memo comparator compares by value.
function annotationsEqual(
  a: FileView["annotations"],
  b: FileView["annotations"],
) {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}

/// Whether a press here drags files rather than starting a drag
/// rectangle: anywhere in the name cell (`stem` when the extension has a
/// column of its own).
/// The name cell of a draggable row; `..` can't be dragged, so its name
/// cell starts a drag rectangle like the rest of the row.
function isDndHandle(target: HTMLElement): boolean {
  const cell = target.closest('[data-column="name"], [data-column="stem"]');
  return (
    cell !== null &&
    cell.closest<HTMLElement>("li[data-name]")?.dataset.name !== ".."
  );
}

type FileRowProps = {
  row: FileView;
  id: string;
  posInSet: number;
  setSize: number;
  columns: ColumnDef[];
  /// The pane's cursor row, whether or not the pane has focus.
  isCursor: boolean;
  isFocused: boolean;
  isSelected: boolean;
  filter: string | null;
  filterMode: FilterMode;
  widthPrefix: string;
  dateFormat?: string;
  timeFormat?: string;
  locale?: string;
  siSizePrefixes?: boolean;
  sizeUnits?: SizeUnits;
  onClick: React.MouseEventHandler<HTMLLIElement>;
  onMouseDown: React.MouseEventHandler<HTMLLIElement>;
  onOpen: (file: FileView) => void;
};

const FileRow = memo(
  function FileRow({
    row,
    id,
    posInSet,
    setSize,
    columns,
    isCursor,
    isFocused,
    isSelected,
    filter,
    filterMode,
    widthPrefix,
    dateFormat,
    timeFormat,
    locale,
    siSizePrefixes,
    sizeUnits,
    onClick,
    onMouseDown,
    onOpen,
  }: FileRowProps) {
    const ctx: FileRowContext = {
      isFocused,
      filter,
      filterMode,
      dateFormat,
      timeFormat,
      locale,
      siSizePrefixes,
      sizeUnits,
    };
    return (
      <li
        id={id}
        role="option"
        aria-selected={isSelected}
        aria-posinset={posInSet}
        aria-setsize={setSize}
        data-name={row.key ?? row.name}
        data-is-dir={row.is_dir ? "true" : undefined}
        className={`${styles.fileItem} ${isFocused ? styles.focused : isCursor ? styles.cursor : ""} ${isSelected ? styles.selected : ""}`}
        onClick={onClick}
        onMouseDown={onMouseDown}
        onDoubleClick={() => onOpen(row)}
      >
        {columns.map((column) => (
          <div
            key={column.key}
            data-column={column.key}
            style={{
              textAlign: column.align,
              width: `var(--${widthPrefix}-${column.key})`,
            }}
            className={columnStyles.datum}
          >
            {column.render(row, ctx)}
          </div>
        ))}
      </li>
    );
  },
  (prev, next) =>
    prev.row.name === next.row.name &&
    prev.row.key === next.row.key &&
    prev.id === next.id &&
    prev.posInSet === next.posInSet &&
    prev.setSize === next.setSize &&
    prev.row.source === next.row.source &&
    prev.row.size === next.row.size &&
    prev.row.modified === next.row.modified &&
    prev.row.mode === next.row.mode &&
    prev.row.is_dir === next.row.is_dir &&
    prev.row.is_symlink === next.row.is_symlink &&
    annotationsEqual(prev.row.annotations, next.row.annotations) &&
    prev.columns === next.columns &&
    prev.isCursor === next.isCursor &&
    prev.isFocused === next.isFocused &&
    prev.isSelected === next.isSelected &&
    prev.filter === next.filter &&
    prev.filterMode === next.filterMode &&
    prev.widthPrefix === next.widthPrefix &&
    prev.dateFormat === next.dateFormat &&
    prev.timeFormat === next.timeFormat &&
    prev.locale === next.locale &&
    prev.siSizePrefixes === next.siSizePrefixes &&
    prev.sizeUnits === next.sizeUnits &&
    prev.onClick === next.onClick &&
    prev.onMouseDown === next.onMouseDown &&
    prev.onOpen === next.onOpen,
);

const IS_MAC_HOST = navigator.platform.startsWith("Mac");
const IS_WINDOWS_HOST = navigator.platform.startsWith("Win");

// Platform "mod" for mouse interactions (add-to-selection): ⌘ on macOS —
// where Ctrl+click is the native context-menu gesture — Ctrl elsewhere.
const modKey = (e: { ctrlKey: boolean; metaKey: boolean }) =>
  IS_MAC_HOST ? e.metaKey : e.ctrlKey;

const VFS_ICONS: Record<string, string> = {
  local: "\u{f02ca}",
  s3: "\u{f0e0f}",
  sftp: "\u{eb3a}",
  archive: "\u{eaa0}",
  archive_zip: "\u{eaa0}",
  archive_7z: "\u{eaa0}",
  compressed_file: "\u{eaa0}",
  remote: "\u{f048d}",
  agent: "\u{f048d}",
};

// Icons for drives and volumes, keyed by VolumeKind.
const VOLUME_KIND_ICONS: Record<string, string> = {
  Fixed: "\u{f02ca}", // md-harddisk
  Removable: "\u{f129e}", // md-usb_flash_drive
  Optical: "\u{ede9}", // fa-compact_disc
  Network: "\u{f0870}", // md-folder_network
  RamDisk: "\u{f035b}", // md-memory
  Substituted: "\u{f0337}", // md-link
  DiskImage: "\u{f03d7}", // md-package_variant_closed
  Device: "\u{f011c}", // md-cellphone
};

// Whether `path` is `location` or lies under it.
function within(location: string, path: string): boolean {
  return (
    path === location ||
    path.startsWith(location.endsWith("/") ? location : location + "/")
  );
}

function VfsSelector({
  vfsDisplayName,
  vfsTargets,
  paneHandle,
  activeVfsId,
  activePath,
  open,
}: {
  vfsDisplayName: string;
  vfsTargets: VfsTarget[];
  paneHandle: number;
  activeVfsId: number;
  activePath: string;
  open: boolean;
}) {
  const formatBytes = useFormatBytes();
  const shortcuts = useCommandShortcuts();
  // Track when we're opening a mount dialog so we don't steal focus back
  const openingDialogRef = useRef(false);

  // The ✓ goes to the most specific entry holding the pane's path: a
  // volume over the filesystem it's mounted in.
  let activeIndex = -1;
  vfsTargets.forEach((target, i) => {
    if (target.kind === "Connect" || target.vfs_id !== activeVfsId) return;
    if (target.path != null && !within(target.path, activePath)) return;
    const best = vfsTargets[activeIndex];
    if (!best || (target.path?.length ?? 0) > (best.path?.length ?? 0)) {
      activeIndex = i;
    }
  });

  return (
    <DropdownMenu.Root
      open={open}
      onOpenChange={(v) => {
        if (!v && !openingDialogRef.current) safe(commands.closeModal());
        openingDialogRef.current = false;
      }}
    >
      <DropdownMenu.Trigger asChild>
        <button
          className={styles.vfsSelector}
          type="button"
          tabIndex={-1}
          title={shortcuts.label("Switch filesystem", "select_vfs")}
          onClick={(e) => {
            e.stopPropagation();
            commands.dialog("select_vfs", paneHandle);
          }}
          onMouseDown={(e) => {
            // Activate this pane without letting the .pane onClick steal focus later
            e.stopPropagation();
            safeSilent(commands.focus(paneHandle, null));
          }}
        >
          {vfsDisplayName} <span aria-hidden>&#x25BE;</span>
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={menuStyles.content}
          align="start"
          sideOffset={4}
          loop
          onCloseAutoFocus={(e) => {
            // Prevent Radix from focusing the trigger button — the pane
            // focus effect handles restoring focus to the correct (active)
            // pane when modalOpen/isVfsSelectorOpen change.
            e.preventDefault();
          }}
        >
          {vfsTargets.map((target, i) => {
            // A filesystem's locations are grouped under a header for it
            // and closed off with a divider: a split-root filesystem's
            // drives, or a filesystem followed by the volumes mounted in
            // it. They sort contiguously, so detect the run by its
            // neighbours.
            const prev = vfsTargets[i - 1];
            const next = vfsTargets[i + 1];
            const sameFs = (other?: VfsTarget) =>
              other != null && other.vfs_id === target.vfs_id;
            const isLocation =
              target.kind === "Drive" ||
              target.kind === "Volume" ||
              (target.kind === "Mounted" &&
                next?.kind === "Volume" &&
                sameFs(next));
            const groupStart =
              (target.kind === "Drive" &&
                !(prev?.kind === "Drive" && sameFs(prev))) ||
              (target.kind === "Mounted" && isLocation);
            const groupEnd =
              (target.kind === "Drive" &&
                !(next?.kind === "Drive" && sameFs(next))) ||
              (target.kind === "Volume" &&
                !(next?.kind === "Volume" && sameFs(next)));
            // The connect entries form a trailing section opened by a
            // separator, unless a group just closed with one.
            const connectStart =
              target.kind === "Connect" &&
              prev != null &&
              prev.kind !== "Connect" &&
              prev.kind !== "Drive" &&
              prev.kind !== "Volume";
            const icon =
              (target.volume && VOLUME_KIND_ICONS[target.volume.kind]) ||
              (target.kind === "Volume"
                ? VOLUME_KIND_ICONS.Fixed
                : VFS_ICONS[target.type_name]);
            return (
              <Fragment
                key={`${target.kind}-${target.type_name}-${target.vfs_id ?? `u${i}`}-${target.path ?? ""}`}
              >
                {connectStart && (
                  <DropdownMenu.Separator className={menuStyles.separator} />
                )}
                {groupStart && (
                  <DropdownMenu.Label className={menuStyles.sectionHeader}>
                    {target.display_name}
                  </DropdownMenu.Label>
                )}
                <DropdownMenu.Item
                  className={menuStyles.item}
                  onSelect={(e) => {
                    // Prevent Radix from auto-closing the dropdown — the
                    // command handlers replace/close the modal on the Rust
                    // side, which updates `open` via props.
                    e.preventDefault();
                    if (target.vfs_id == null && target.mount_dialog) {
                      openingDialogRef.current = true;
                      commands.dialog(
                        target.mount_dialog as
                          "mount_s3" | "mount_sftp" | "mount_remote",
                        paneHandle,
                      );
                    } else {
                      safe(
                        commands.switchVfs(
                          paneHandle,
                          target.vfs_id,
                          target.type_name,
                          target.path,
                        ),
                      );
                    }
                  }}
                >
                  <span className={menuStyles.itemIcon} aria-hidden>
                    {icon}
                  </span>
                  <span className={menuStyles.itemLabel}>
                    {isLocation ? (
                      // Under the group header: the drive and its volume
                      // label, the volume's name, or the root, then where
                      // it points or lives, dimmed.
                      <>
                        {target.label ?? target.detail}
                        {target.kind === "Drive" &&
                          target.volume?.label &&
                          ` (${target.volume.label})`}
                        {target.label != null && target.detail != null && (
                          <span className={menuStyles.itemDetail}>
                            {target.detail}
                          </span>
                        )}
                      </>
                    ) : (
                      <>
                        {target.display_name}
                        {target.label && ` (${target.label})`}
                        {target.kind === "Connect" && "…"}
                      </>
                    )}
                  </span>
                  {target.available_bytes != null && (
                    <span className={menuStyles.itemMeta}>
                      {formatBytes(target.available_bytes)} free
                    </span>
                  )}
                  {i === activeIndex && (
                    <span className={menuStyles.itemCheck}>{"✓"}</span>
                  )}
                  {(target.kind === "Mounted" || target.kind === "Drive") &&
                    target.type_name !== "local" && (
                      <button
                        className={menuStyles.itemDismiss}
                        title="Disconnect"
                        // Mouse-only: focus never leaves the menu item, and
                        // the keyboard route is the unmount_vfs command.
                        aria-hidden
                        onClick={(e) => {
                          e.stopPropagation();
                          if (target.vfs_id !== null) {
                            safe(
                              commands.unmountVfs(paneHandle, target.vfs_id),
                            );
                          }
                        }}
                        tabIndex={-1}
                      >
                        {"✕"}
                      </button>
                    )}
                </DropdownMenu.Item>
                {groupEnd && (
                  <DropdownMenu.Separator className={menuStyles.separator} />
                )}
              </Fragment>
            );
          })}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

function FilterInput({
  value,
  filterMode,
  listId,
  activeDescendant,
  inputRef,
  onKeyDown,
  onChange,
}: {
  value: string | null;
  filterMode: string;
  listId: string;
  activeDescendant: string | undefined;
  inputRef: React.RefObject<HTMLInputElement | null>;
  onKeyDown: React.KeyboardEventHandler;
  onChange: (value: string) => void;
}) {
  const [localValue, setLocalValue] = useState(value ?? "");
  // filter=null means no active session → always hidden.
  // filter="" (user backspaced everything) keeps the bar visible until Escape.
  const isVisibleFilter = filterMode === "filter" && value != null;

  // Sync local value when the remote value changes (e.g. quick-search
  // prefix updates from the backend).
  useEffect(() => {
    setLocalValue(value ?? "");
  }, [value]);

  // Typing here moves the file list's focused row, as a combobox drives its
  // listbox.
  const input = (
    <input
      className={isVisibleFilter ? styles.filterBarInput : undefined}
      type="text"
      role="combobox"
      aria-label={filterMode === "filter" ? "Filter" : "Quick search"}
      aria-autocomplete="list"
      aria-expanded
      aria-controls={listId}
      aria-activedescendant={activeDescendant}
      value={isVisibleFilter ? localValue : (value ?? "")}
      placeholder={isVisibleFilter ? "Filter (regex)" : undefined}
      onChange={(e) => {
        setLocalValue(e.target.value);
        onChange(e.target.value);
      }}
      ref={inputRef}
      onKeyDown={onKeyDown}
      tabIndex={-1}
      autoComplete="off"
      autoCorrect="off"
      autoCapitalize="off"
      spellCheck={false}
    />
  );

  if (isVisibleFilter) {
    return <div className={styles.filterBar}>{input}</div>;
  }

  return <div className={styles.filterInput}>{input}</div>;
}

/// What Shift+Enter would do on a row, or `null` when it would do nothing.
/// An alias source wins over the entry's own symlink target, mirroring
/// `cmd_follow_symlink`.
function followKind(row: FileView | undefined): FollowKind {
  if (!row) return null;
  if (row.source_display) return "source";
  return row.is_symlink ? "symlink" : null;
}

// --- Shared DnD helpers (used by both internal and external drag-and-drop) ---

/** Clear all drop-target highlights in the document. */
function clearDropHighlights() {
  document
    .querySelectorAll(".dnd-drop-target, .dnd-drop-hover")
    .forEach((el) => {
      el.classList.remove("dnd-drop-target", "dnd-drop-hover");
    });
}

type DropTarget = {
  paneEl: HTMLElement;
  paneHandle: number;
  subdirectory: string | null;
};

/** Find the pane and directory row under a screen point. */
function resolveDropTarget(x: number, y: number): DropTarget | null {
  const el = document.elementFromPoint(x, y);
  if (!el) return null;
  const paneEl = el.closest("[data-pane-handle]") as HTMLElement | null;
  if (!paneEl) return null;
  const paneHandle = parseInt(paneEl.dataset.paneHandle!, 10);
  const li = el.closest("li[data-is-dir='true']") as HTMLElement | null;
  const subdirectory = li?.dataset.name ?? null;
  return { paneEl, paneHandle, subdirectory };
}

/** Apply drop-target highlighting for a resolved target.
 *  Pane background and folder row highlights are mutually exclusive:
 *  hovering a folder highlights the row, otherwise the pane. */
function highlightDropTarget(
  target: DropTarget,
  sourcePaneHandle?: number,
  excludeNames?: Set<string>,
) {
  clearDropHighlights();
  const isCrossPane =
    sourcePaneHandle === undefined || target.paneHandle !== sourcePaneHandle;

  // Try to highlight a folder row
  if (target.subdirectory) {
    if (!excludeNames || !excludeNames.has(target.subdirectory)) {
      const li = target.paneEl.querySelector(
        `li[data-name="${CSS.escape(target.subdirectory)}"]`,
      );
      if (li) {
        li.classList.add("dnd-drop-hover");
        return;
      }
    }
  }

  // No folder target — highlight the pane background (current dir)
  // For internal same-pane, this is not a valid drop (no-op)
  if (isCrossPane) {
    target.paneEl.classList.add("dnd-drop-target");
  }
}

/// Branch indicator for the pane header, fed by the git enricher's
/// context badge. Quiet by design: muted text, seti git glyph, a `*`
/// suffix for uncommitted changes, ↑/↓ only when ahead/behind.
function GitBranchBadge({ badge }: { badge: ContextBadge }) {
  if (!("git_branch" in badge) || !badge.git_branch.name) return null;
  const branch = badge.git_branch;
  const git = iconDefs["git"];
  const parts = [branch.name + (branch.dirty ? "*" : "")];
  if (branch.ahead > 0) parts.push(`↑${branch.ahead}`);
  if (branch.behind > 0) parts.push(`↓${branch.behind}`);
  const commits = (n: number) => `${n} ${n === 1 ? "commit" : "commits"}`;
  const title = [
    branch.detached
      ? `Detached HEAD at ${branch.name}`
      : `Branch ${branch.name}`,
    branch.dirty ? "uncommitted changes" : null,
    branch.ahead > 0 ? `${commits(branch.ahead)} ahead` : null,
    branch.behind > 0 ? `${commits(branch.behind)} behind` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <div className={styles.gitBadge} title={title}>
      {git && (
        <span className={styles.gitBadgeIcon} aria-hidden>
          {String.fromCodePoint(parseInt(git.fontCharacter, 16))}
        </span>
      )}
      {parts.join(" ")}
    </div>
  );
}

/// Inline progress line for VFS background work (search walker, …).
/// Pulls the well-known `extra.path` key out as a trailing path
/// fragment (rendered through ellipsis-friendly CSS) and folds the
/// rest of the `extra` map alongside the dominant counter.
function VfsProgressLine({
  progress,
  locale,
}: {
  progress: VfsProgress;
  locale?: string;
}) {
  const extra = { ...(progress.extra ?? {}) };
  const path = extra.path;
  delete extra.path;

  const parts: string[] = [progress.stage];
  if (progress.processed != null) {
    const counter =
      progress.total != null
        ? `${progress.processed.toLocaleString(locale)}/${progress.total.toLocaleString(locale)}`
        : `${progress.processed.toLocaleString(locale)} items`;
    parts.push(counter);
  }
  for (const [key, value] of Object.entries(extra)) {
    parts.push(`${key}: ${value}`);
  }
  if (path) {
    parts.push(path);
  }
  return <>{parts.join(" · ")}</>;
}

function PaneInner(
  props: PaneState & {
    paneHandle: number;
    active: boolean;
    /// The session's active pane, focused or not (the terminal may have
    /// the focus).
    current: boolean;
    modalOpen: boolean;
    modal: ModalData | null;
    vfsProgress?: VfsProgress;
    windowsDrives: boolean;
    /// This pane is shown alone rather than in the split layout.
    maximized: boolean;
    /// Only one pane is on screen (Quick View, maximized): offer a switch to
    /// the other.
    swappable?: boolean;
  },
) {
  const {
    paneHandle,
    active,
    current,
    modalOpen,
    windowsDrives,
    maximized,
    swappable,
    filter,
    filter_mode,
    path,
    file_window,
    selected,
    sorting,
    focused,
    pending_path,
    loading,
    partial,
    fs_stats,
    stats,
    breadcrumbs,
    vfs_display_name,
    metadata_traits,
    context_badges,
    enrichment_activity,
    modal,
    vfsProgress,
  } = props;

  const isVfsSelectorOpen =
    modal?.type === "select_vfs" && modal?.context?.pane_handle === paneHandle;
  const vfsTargets: VfsTarget[] =
    (isVfsSelectorOpen && modal?.data?.targets) || [];
  const isSortMenuOpen =
    modal?.type === "sort_menu" && modal?.context?.pane_handle === paneHandle;
  const sortFoldersFirst: boolean =
    (isSortMenuOpen && modal?.data?.folders_first) || false;
  const isHistoryNavigatorOpen =
    modal?.type === "history_navigator" &&
    modal?.context?.pane_handle === paneHandle;
  const historyEntries: HistoryEntryView[] =
    (isHistoryNavigatorOpen && modal?.data?.entries) || [];
  const historyCurrentIndex: number =
    (isHistoryNavigatorOpen && modal?.data?.current_index) || 0;
  const historyInitialDirection: -1 | 1 =
    isHistoryNavigatorOpen && modal?.data?.initial_direction === 1 ? 1 : -1;
  const historyPersistent: boolean =
    (isHistoryNavigatorOpen && modal?.data?.persistent) || false;
  const focusedIndex = props.focused_index ?? -1;
  const idBase = useId();
  const listId = `${idBase}-files`;
  const emptyId = `${idBase}-empty`;
  const rowId = (index: number) => `${idBase}-row-${index}`;
  // DOM focus stays on the list (or the filter input) while the focused row
  // is only styled, so assistive tech learns of it via aria-activedescendant.
  const activeDescendant =
    focusedIndex >= file_window.offset &&
    focusedIndex < file_window.offset + file_window.items.length
      ? rowId(focusedIndex)
      : undefined;
  // Allow interaction when loading (partial results visible) but not when pending_path is set (no files yet)
  const isBusy = !!pending_path && !loading;
  // Run a typed command, skipping it while the pane is busy (awaiting the
  // first listing of a navigated-to path); `alsoWhenBusy` overrides the guard
  // for commands that must always fire (Escape cancel/clear-filter, dir-up).
  // Takes a thunk because invoking `commands.X(...)` already sends the IPC, so
  // the guard must gate promise construction, not just the safe() wrap.
  const guarded = <T,>(
    factory: () => Promise<Result<T, string>>,
    alsoWhenBusy = false,
  ) => {
    if (alsoWhenBusy || !isBusy) safe(factory());
  };

  const preferences = usePreferences();
  const shortcuts = useCommandShortcuts();
  const boundKeys = useMemo(
    () => new Set(preferences?.bindings.map((b) => b.key)),
    [preferences],
  );
  // Passed explicitly to every Intl call rather than letting the runtime
  // pick — see `ResolvedPreferences.locale`.
  const locale = useLocale();
  const formatBytes = useFormatBytes();
  const sizeUnits = useSizeUnits();

  // The key that switches this pane into filter mode. Rebindable like any
  // other command (`start_filter`), which matters for layouts where `/`
  // is a shifted key — QWERTZ produces it as Shift+7, so a hardcoded
  // unmodified `/` never matched there. Resolved here rather than
  // dispatched from the window-level handler because the pane's own
  // keydown runs first and swallows every unmodified printable character
  // into quick-search; matching it here keeps that ordering intact.
  const startFilterKey = useMemo(
    () => preferences?.bindings.find((b) => b.command === "start_filter")?.key,
    [preferences?.bindings],
  );

  const rowHeight = rowHeightFor(preferences?.settings?.appearance?.density);

  const columns = useMemo(
    () =>
      getVisibleColumns(
        preferences?.settings?.appearance?.columns,
        metadata_traits,
      ),
    [preferences?.settings?.appearance?.columns, metadata_traits],
  );

  // The full spinner, only while navigation has no partial results yet.
  const showSpinner = useAfterGrace(!!pending_path && !loading);

  // Enrichment activity line, held back so a quick run (a typical git
  // status) doesn't flash the status bar on every navigation.
  const activityLabels = Object.values(enrichment_activity ?? {});
  const hasActivity = activityLabels.length > 0;
  const showActivity = useAfterGrace(hasActivity);
  // Whatever the status bar reports as under way.
  const busy = useAfterGrace(
    (!!pending_path && !loading) || loading || hasActivity || !!vfsProgress,
  );

  // Without this lookup, rendering suddenly becomes O(n^2), which is very slow
  // when someone Ctrl+A's a directory with 1000+ files.
  const selectedLookup = useMemo(() => {
    return new Set(selected);
  }, [selected]);

  const containerRef = useRef<HTMLUListElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const tableHeaderRef = useRef<HTMLDivElement>(null);

  const dragRef = useRef<DragState | null>(null);
  const dragRectRef = useRef<HTMLDivElement>(null);
  const suppressClickRef = useRef(false);
  const fileWindowRef = useRef(file_window);
  fileWindowRef.current = file_window;
  const selectedLookupRef = useRef(selectedLookup);
  selectedLookupRef.current = selectedLookup;
  const focusedRef = useRef(focused);
  focusedRef.current = focused;

  // --- DnD (drag-and-drop between panes) refs ---
  const dndRef = useRef<LocalDndState | null>(null);
  const dndGhostRef = useRef<HTMLDivElement>(null);

  // Rust moved the viewport with the cursor as the list changed around it;
  // follow by the same number of rows, before the frame is painted.
  const scrollShift = props.scroll_shift;
  const lastScrollShiftRef = useRef(scrollShift);
  useLayoutEffect(() => {
    const rows = scrollShift - lastScrollShiftRef.current;
    lastScrollShiftRef.current = scrollShift;
    const container = containerRef.current;
    if (rows !== 0 && container) container.scrollTop += rows * rowHeight;
  }, [scrollShift, rowHeight]);
  // The cursor's entry and index as last rendered, to tell the cursor
  // moving from the list changing around it.
  const prevCursorRef = useRef<{
    path: string;
    key: string | null;
    index: number;
  }>({ path: "", key: null, index: -1 });
  // A scroll to the cursor that lands outside the rows Rust has sent waits
  // for them, keeping the old rows on screen meanwhile: the cursor can be
  // put far from the window, e.g. by a focus restored after a refresh.
  const pendingCursorScrollRef = useRef<number | null>(null);
  const windowCovers = (top: number, height: number) => {
    const fw = fileWindowRef.current;
    const first = Math.floor(top / rowHeight);
    const end = Math.min(Math.ceil((top + height) / rowHeight), fw.total_count);
    return first >= fw.offset && end <= fw.offset + fw.items.length;
  };
  useLayoutEffect(() => {
    const container = containerRef.current;
    const prev = prevCursorRef.current;
    const pathKey = `${path.vfs_id}:${path.path}`;
    prevCursorRef.current = {
      path: pathKey,
      key: focused,
      index: focusedIndex,
    };
    // A scroll still waiting for its rows is where the view is headed.
    const pending = pendingCursorScrollRef.current;
    pendingCursorScrollRef.current = null;
    if (focusedIndex < 0 || !container) return;

    const containerHeight = container.clientHeight;
    const scrollTop = pending ?? container.scrollTop;
    let target: number;
    if (
      pathKey === prev.path &&
      focused === prev.key &&
      prev.index >= 0 &&
      focusedIndex !== prev.index
    ) {
      // The list changed around the cursor (entries sorted in above it, a
      // refresh, a re-sort). Rust has moved the view with a cursor that was
      // in it (`scroll_shift`); one out of view is no reason to move it.
      target = scrollTop;
    } else {
      if (!active) return;
      const itemTop = focusedIndex * rowHeight;
      const itemBottom = itemTop + rowHeight;
      if (itemTop < scrollTop) {
        target = itemTop;
      } else if (itemBottom > scrollTop + containerHeight) {
        target = itemBottom - containerHeight;
      } else {
        target = scrollTop;
      }
    }
    target = Math.max(0, target);
    if (target === container.scrollTop) return;
    if (windowCovers(target, containerHeight)) {
      container.scrollTop = target;
      return;
    }
    pendingCursorScrollRef.current = target;
    const firstVisible = Math.floor(target / rowHeight);
    const visibleCount = Math.ceil(containerHeight / rowHeight);
    lastViewportReportRef.current = [
      firstVisible,
      visibleCount,
      fileWindowRef.current.offset,
    ];
    safeSilent(commands.setViewport(paneHandle, firstVisible, visibleCount));
    // Intentionally excluding file_window — this should only fire when the
    // focused item changes position, not when the window slides on scroll.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, path, focused, focusedIndex, rowHeight]);
  useLayoutEffect(() => {
    const container = containerRef.current;
    const target = pendingCursorScrollRef.current;
    if (target === null || !container) return;
    if (windowCovers(target, container.clientHeight)) {
      pendingCursorScrollRef.current = null;
      container.scrollTop = target;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [file_window]);

  useEffect(() => {
    if (active && !modalOpen) {
      if (!isVfsSelectorOpen) {
        if (filter != null) {
          inputRef.current?.focus();
        } else {
          containerRef.current?.focus();
        }
      }
    } else if (!active) {
      inputRef.current?.blur();
      containerRef.current?.blur();
    }
  }, [active, path, filter, filter_mode, modalOpen, isVfsSelectorOpen]);

  // --- Drag-to-select logic ---

  const sendDragSelection = useCallback(
    (drag: DragState, startIdx: number, endIdx: number) => {
      safeSilent(
        commands.setSelectionByIndices(
          paneHandle,
          startIdx,
          endIdx,
          drag.mode === "ctrl",
        ),
      );
    },
    [paneHandle],
  );

  const updateDragRect = useCallback(
    (drag: DragState, curScrollX: number, curScrollY: number) => {
      const el = dragRectRef.current;
      if (!el) return;
      el.style.display = "block";
      el.style.left = Math.min(drag.startScrollX, curScrollX) + "px";
      el.style.top = Math.min(drag.startScrollY, curScrollY) + "px";
      el.style.width = Math.abs(curScrollX - drag.startScrollX) + "px";
      el.style.height = Math.abs(curScrollY - drag.startScrollY) + "px";
    },
    [],
  );

  const hideDragRect = useCallback(() => {
    const el = dragRectRef.current;
    if (el) el.style.display = "none";
  }, []);

  const updateDragSelection = useCallback(
    (drag: DragState, curScrollY: number) => {
      const rectTop = Math.min(drag.startScrollY, curScrollY);
      const rectBottom = Math.max(drag.startScrollY, curScrollY);
      const startIdx = Math.max(0, Math.floor(rectTop / rowHeight));
      const endIdx = Math.max(0, Math.ceil(rectBottom / rowHeight) - 1);

      if (
        startIdx !== drag.lastSentStartIdx ||
        endIdx !== drag.lastSentEndIdx
      ) {
        drag.lastSentStartIdx = startIdx;
        drag.lastSentEndIdx = endIdx;
        sendDragSelection(drag, startIdx, endIdx);
      }
    },
    [sendDragSelection, rowHeight],
  );

  const updateAutoScroll = useCallback(
    (clientY: number) => {
      const drag = dragRef.current;
      if (!drag || !drag.active) return;

      const container = containerRef.current;
      if (!container) return;

      const rect = container.getBoundingClientRect();
      const edgeZone = 44; // 2 rows
      const topEdge = clientY - rect.top;
      const bottomEdge = rect.bottom - clientY;

      if (topEdge < edgeZone && topEdge >= 0) {
        const speed = Math.max(1, Math.round((1 - topEdge / edgeZone) * 10));
        if (drag.scrollIntervalId === null) {
          drag.scrollIntervalId = window.setInterval(() => {
            const d = dragRef.current;
            if (!d || !d.active) return;
            container.scrollTop = Math.max(0, container.scrollTop - speed);
            const r = container.getBoundingClientRect();
            const curScrollY = d.lastClientY - r.top + container.scrollTop;
            updateDragRect(d, d.lastCurScrollX, curScrollY);
            updateDragSelection(d, curScrollY);
          }, 16);
        }
      } else if (bottomEdge < edgeZone && bottomEdge >= 0) {
        const speed = Math.max(1, Math.round((1 - bottomEdge / edgeZone) * 10));
        if (drag.scrollIntervalId === null) {
          drag.scrollIntervalId = window.setInterval(() => {
            const d = dragRef.current;
            if (!d || !d.active) return;
            container.scrollTop += speed;
            const r = container.getBoundingClientRect();
            const curScrollY = d.lastClientY - r.top + container.scrollTop;
            updateDragRect(d, d.lastCurScrollX, curScrollY);
            updateDragSelection(d, curScrollY);
          }, 16);
        }
      } else {
        if (drag.scrollIntervalId !== null) {
          clearInterval(drag.scrollIntervalId);
          drag.scrollIntervalId = null;
        }
      }
    },
    [updateDragRect, updateDragSelection],
  );

  useEffect(() => {
    const onMouseMove = (e: MouseEvent) => {
      const drag = dragRef.current;
      if (!drag) return;

      const container = containerRef.current;
      if (!container) return;

      // Detect mouseup that happened outside the window
      if (e.buttons === 0) {
        if (drag.scrollIntervalId !== null)
          clearInterval(drag.scrollIntervalId);
        if (drag.active) {
          suppressClickRef.current = true;
          safeSilent(commands.endDragSelection(paneHandle));
        }
        hideDragRect();
        dragRef.current = null;
        return;
      }

      drag.lastClientY = e.clientY;

      if (!drag.active) {
        const dx = e.clientX - drag.startX;
        const dy = e.clientY - drag.startY;
        if (dx * dx + dy * dy < 25) return; // 5px threshold
        drag.active = true;
      }

      const rect = container.getBoundingClientRect();
      const curScrollX = e.clientX - rect.left + container.scrollLeft;
      const curScrollY = e.clientY - rect.top + container.scrollTop;
      drag.lastCurScrollX = curScrollX;

      updateDragRect(drag, curScrollX, curScrollY);
      updateDragSelection(drag, curScrollY);
      updateAutoScroll(e.clientY);
    };

    const onMouseUp = (_e: MouseEvent) => {
      const drag = dragRef.current;
      if (!drag) return;
      if (drag.scrollIntervalId !== null) clearInterval(drag.scrollIntervalId);
      if (drag.active) {
        suppressClickRef.current = true;
        // Finalize the drag so the next Ctrl+drag snapshots the
        // accumulated selection as its new base.
        safeSilent(commands.endDragSelection(paneHandle));
      }
      hideDragRect();
      dragRef.current = null;
    };

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
    return () => {
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };
  }, [
    sendDragSelection,
    updateAutoScroll,
    updateDragRect,
    updateDragSelection,
    hideDragRect,
  ]);

  // Cancel drag when the directory changes (e.g. navigation)
  useEffect(() => {
    const drag = dragRef.current;
    if (drag) {
      if (drag.scrollIntervalId !== null) clearInterval(drag.scrollIntervalId);
      hideDragRect();
      dragRef.current = null;
    }
  }, [path]);

  const onMouseDown = useCallback(
    (e: React.MouseEvent<HTMLUListElement>) => {
      if (e.button !== 0 || e.shiftKey) return;
      // The name cell drags files (onDndMouseDown); everywhere else starts
      // a drag rectangle.
      if (isDndHandle(e.target as HTMLElement)) return;
      e.preventDefault(); // block text selection

      const container = containerRef.current;
      if (!container) return;

      // Focus the file under cursor (using DOM, not scroll math).
      // This must happen on mouseDown, not onClick, because a tiny drag
      // (>5px) suppresses the click event.
      const li = (e.target as HTMLElement).closest(
        "li[data-name]",
      ) as HTMLElement | null;
      if (li?.dataset.name) {
        safeSilent(commands.focus(paneHandle, li.dataset.name));
      } else {
        safeSilent(commands.focus(paneHandle, null));
      }

      const rect = container.getBoundingClientRect();
      const startScrollX = e.clientX - rect.left + container.scrollLeft;
      const startScrollY = e.clientY - rect.top + container.scrollTop;

      dragRef.current = {
        active: false,
        startX: e.clientX,
        startY: e.clientY,
        startScrollX,
        startScrollY,
        mode: modKey(e) ? "ctrl" : "normal",
        lastSentStartIdx: -1,
        lastSentEndIdx: -1,
        lastClientY: e.clientY,
        lastCurScrollX: startScrollX,
        scrollIntervalId: null,
      };
    },
    [paneHandle, focused],
  );

  // --- End drag-to-select logic ---

  // --- DnD (drag-and-drop between panes) logic ---

  const onDndMouseDown = useCallback(
    (e: React.MouseEvent<HTMLLIElement>) => {
      if (e.button !== 0) return;
      if (!isDndHandle(e.target as HTMLElement)) return;
      const fileName = e.currentTarget.dataset.name;
      if (!fileName) return;

      if (!e.shiftKey) {
        safeSilent(commands.focus(paneHandle, fileName));
      }

      e.preventDefault();
      e.stopPropagation(); // prevent drag-to-select

      const fw = fileWindowRef.current;
      const currentSelected = selectedLookupRef.current;
      const fileKey = (f: FileView) => f.key ?? f.name;
      const filesToDrag = currentSelected.has(fileName)
        ? fw.items.filter(
            (f) => currentSelected.has(fileKey(f)) && f.name !== "..",
          )
        : [fw.items.find((f) => fileKey(f) === fileName)!];

      dndRef.current = {
        active: false,
        startX: e.clientX,
        startY: e.clientY,
        files: filesToDrag.map((f) => ({
          name: fileKey(f),
          is_dir: f.is_dir,
        })),
      };
    },
    [paneHandle],
  );

  const cleanupDnd = useCallback(() => {
    const ghost = dndGhostRef.current;
    if (ghost) ghost.style.display = "none";
    clearDropHighlights();
  }, []);

  useEffect(() => {
    // Hand the drag off to a native OS drag session so it can leave the
    // window. Only tear down the internal drag once the backend accepts —
    // a declined escalation (non-host-local files) keeps today's behavior:
    // the ghost stays and an outside mouseup cancels via `buttons === 0`.
    const escalateDnd = async () => {
      const dnd = dndRef.current;
      if (!dnd?.active || dnd.escalating) return;
      dnd.escalating = true;
      try {
        const image = await renderDragImage(dnd.files);
        const accepted = await rawCommands.dndDragOut(image);
        if (accepted.status === "ok" && accepted.data) {
          cleanupDnd();
          dndRef.current = null;
          suppressClickRef.current = true;
        } else {
          dnd.escalating = false;
        }
      } catch {
        if (dndRef.current) dndRef.current.escalating = false;
      }
    };

    const onDndMouseMove = (e: MouseEvent) => {
      const dnd = dndRef.current;
      if (!dnd) return;

      // Detect mouseup that happened outside the window
      if (e.buttons === 0) {
        if (dnd.active) {
          safeSilent(commands.cancelDnd());
          suppressClickRef.current = true;
        }
        cleanupDnd();
        dndRef.current = null;
        return;
      }

      // Pointer left the window with the button held: escalate. The webview
      // captures the mouse during a drag, so moves keep firing with
      // out-of-viewport coordinates.
      if (
        dnd.active &&
        !dnd.escalating &&
        (e.clientX < 0 ||
          e.clientY < 0 ||
          e.clientX >= window.innerWidth ||
          e.clientY >= window.innerHeight)
      ) {
        void escalateDnd();
        return;
      }

      if (!dnd.active) {
        const dx = e.clientX - dnd.startX;
        const dy = e.clientY - dnd.startY;
        if (dx * dx + dy * dy < 25) return; // 5px threshold
        dnd.active = true;

        // Populate and show ghost
        const ghost = dndGhostRef.current;
        if (ghost) {
          if (dnd.files.length === 1) {
            const f = dnd.files[0];
            const icon = document.createElement("div");
            icon.className = f.is_dir ? "file-icon folder" : "file-icon";
            if (!f.is_dir) {
              const { ch, color } = getFileIconChar(f.name, f.is_dir);
              icon.style.setProperty("--icon-color", color);
              icon.textContent = ch;
            }
            ghost.replaceChildren(icon, ` ${f.name}`);
          } else {
            ghost.textContent = `${dnd.files.length} items`;
          }
          ghost.style.display = "flex";
        }

        safeSilent(commands.startDnd(paneHandle, dnd.files));
      }

      // Position ghost
      const ghost = dndGhostRef.current;
      if (ghost) {
        ghost.style.left = `${e.clientX + 12}px`;
        ghost.style.top = `${e.clientY + 12}px`;
      }

      // Highlight drop targets
      const target = resolveDropTarget(e.clientX, e.clientY);
      if (target) {
        const dnd = dndRef.current;
        const draggedNames = dnd
          ? new Set(dnd.files.map((f) => f.name))
          : undefined;
        highlightDropTarget(target, paneHandle, draggedNames);
      } else {
        clearDropHighlights();
      }
    };

    const onDndMouseUp = (e: MouseEvent) => {
      const dnd = dndRef.current;
      if (!dnd) return;

      cleanupDnd();

      if (!dnd.active) {
        dndRef.current = null;
        return;
      }

      suppressClickRef.current = true;
      dndRef.current = null;

      const target = resolveDropTarget(e.clientX, e.clientY);
      if (!target) {
        safeSilent(commands.cancelDnd());
        return;
      }

      const isSamePane = target.paneHandle === paneHandle;
      let subdirectory = target.subdirectory;

      // In same pane: don't drop onto dirs being dragged (can't drop into yourself)
      if (isSamePane && subdirectory) {
        const draggedNames = new Set(dnd.files.map((f) => f.name));
        if (draggedNames.has(subdirectory)) {
          subdirectory = null;
        }
      }

      // Same pane requires a directory target (otherwise it's a no-op)
      if (isSamePane && !subdirectory) {
        safeSilent(commands.cancelDnd());
        return;
      }

      safe(commands.executeDnd(target.paneHandle, subdirectory, e.shiftKey));
    };

    // Fallback for platforms where mouse capture stops move delivery at the
    // window edge. A false positive (hovered node removed mid-drag) is
    // benign: the escalated drag's drops route through the self-drop path
    // with the same copy semantics.
    const onDndMouseOut = (e: MouseEvent) => {
      if (e.relatedTarget !== null || e.buttons === 0) return;
      const dnd = dndRef.current;
      if (dnd?.active && !dnd.escalating) void escalateDnd();
    };

    document.addEventListener("mousemove", onDndMouseMove);
    document.addEventListener("mouseup", onDndMouseUp);
    document.addEventListener("mouseout", onDndMouseOut);
    return () => {
      document.removeEventListener("mousemove", onDndMouseMove);
      document.removeEventListener("mouseup", onDndMouseUp);
      document.removeEventListener("mouseout", onDndMouseOut);
    };
  }, [paneHandle, cleanupDnd]);

  // Cancel DnD when the directory changes
  useEffect(() => {
    const dnd = dndRef.current;
    if (dnd?.active) {
      cleanupDnd();
      safeSilent(commands.cancelDnd());
    }
    dndRef.current = null;
  }, [path]);

  // --- End DnD logic ---

  const onOpen = useCallback(
    (file: FileView) => {
      if (!file) return;
      safe(commands.enter(paneHandle));
    },
    [paneHandle],
  );

  const relativeJump = (delta: number, withSelection?: boolean) => {
    guarded(() => commands.relativeJump(paneHandle, delta, !!withSelection));
  };

  const onKeyDownCommon = (e: React.KeyboardEvent<Element>) => {
    const { noModifiers } = modifiers(e);

    if (e.key == "ArrowDown" && (noModifiers || e.shiftKey)) {
      relativeJump(1, e.shiftKey);
    } else if (e.key == "ArrowUp" && (noModifiers || e.shiftKey)) {
      relativeJump(-1, e.shiftKey);
    } else if (e.key == "PageDown" && (noModifiers || e.shiftKey)) {
      const pageSize = Math.floor(
        (containerRef.current?.clientHeight ?? 220) / rowHeight,
      );
      relativeJump(pageSize, e.shiftKey);
    } else if (e.key == "PageUp" && (noModifiers || e.shiftKey)) {
      const pageSize = Math.floor(
        (containerRef.current?.clientHeight ?? 220) / rowHeight,
      );
      relativeJump(-pageSize, e.shiftKey);
    } else if (e.key == "Home" && (noModifiers || e.shiftKey)) {
      relativeJump(-Math.pow(2, 31), e.shiftKey);
    } else if (e.key == "End" && (noModifiers || e.shiftKey)) {
      relativeJump(Math.pow(2, 31) - 1, e.shiftKey);
    } else if (e.key == "Enter" && noModifiers) {
      const focusedFile = file_window.items[focusedIndex - file_window.offset];
      if (focusedFile) onOpen(focusedFile);
    } else if (e.key == "Tab" && noModifiers) {
      safe(commands.focus(1 - paneHandle, null));
    } else if (e.key == "Escape" && noModifiers) {
      guarded(() => commands.cancel(paneHandle), true);
      guarded(() => commands.setFilter(paneHandle, null, null));
    } else {
      return false;
    }

    return true;
  };

  const openContextMenu = useCallback(() => {
    const container = containerRef.current;
    if (!container || !focused) return;
    const li = container.querySelector(
      `li[data-name="${CSS.escape(focused)}"]`,
    );
    if (!li) return;
    setContextMenuIsParentDir(focused === "..");
    openMenuAt(li);
  }, [focused]);

  // Serve keyboard-requested menus while this is the active pane, wherever
  // the webview aimed its event (or if it never reached a pane at all).
  useEffect(() => {
    if (!active) return;
    const onRequest = () => openContextMenu();
    window.addEventListener(KEYBOARD_MENU_EVENT, onRequest);
    return () => window.removeEventListener(KEYBOARD_MENU_EVENT, onRequest);
  }, [active, openContextMenu]);

  const onkeydown = (e: React.KeyboardEvent<Element>) => {
    const { noModifiers } = modifiers(e);

    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      openContextMenu();
    } else if (e.key == "Backspace" && noModifiers) {
      guarded(() => commands.navigate(paneHandle, "..", true), true);
    } else if (
      startFilterKey &&
      normalizeKeyEvent(e.nativeEvent) === startFilterKey
    ) {
      guarded(() => commands.cmdStartFilter(paneHandle));
      // The state change focuses the input on its own, but only after the
      // round trip — focus now so a fast follow-up keystroke isn't typed
      // at the file list and lost.
      inputRef.current?.focus();
      // Handled here; the key is also bound at the window level, and
      // `preventDefault` alone would let the dispatcher run it a second
      // time.
      e.stopPropagation();
    } else if (
      windowsDrives &&
      e.shiftKey &&
      !e.ctrlKey &&
      !e.altKey &&
      !e.metaKey &&
      /^[a-z]$/i.test(e.key)
    ) {
      // Shift+<drive letter> → that drive's root.
      guarded(
        () => commands.navigate(paneHandle, `${e.key.toUpperCase()}:\\`, true),
        true,
      );
    } else if (
      e.key.length == 1 &&
      !e.ctrlKey &&
      !e.metaKey &&
      !e.altKey &&
      (!e.shiftKey || !boundKeys.has(normalizeKeyEvent(e.nativeEvent))) &&
      !numpadOperator(e.nativeEvent.code)
    ) {
      // Printable-character heuristic: routes typing into quick-search,
      // Shift included (capitals, digits on AZERTY) unless the chord is
      // bound. Modifier chords (⌘H, Alt+…) are app shortcuts handled by the
      // window-level binding dispatcher, which runs *after* this handler
      // — stealing focus into the filter input here would strand them.
      inputRef.current?.focus();
      return;
    }

    e.preventDefault();
  };

  const onkeydownFilter: React.KeyboardEventHandler = (e) => {
    // A key an IME is composing with is the IME's: Enter committing a
    // candidate must not open the focused file. WebKit sends that Enter
    // after `compositionend`, flagged only by keyCode 229.
    if (e.nativeEvent.isComposing || e.nativeEvent.keyCode === 229) return;
    const { noModifiers } = modifiers(e);

    // Unmodified Backspace always edits the filter text, and Delete does
    // too in regex-filter mode — keep those from reaching the window-level
    // binding dispatcher, where plain Delete is bound to delete_selected.
    // In quick-search mode Delete falls through and deletes files.
    if (
      (e.key === "Backspace" ||
        (e.key === "Delete" && filter_mode === "filter")) &&
      noModifiers
    ) {
      e.stopPropagation();
      return;
    }

    if (onKeyDownCommon(e)) {
      // ...
    } else if (
      filter_mode === "quick_search" &&
      startFilterKey &&
      normalizeKeyEvent(e.nativeEvent) === startFilterKey
    ) {
      // Same command from inside the box: it keeps whatever is typed, so
      // quick-search text carries over into the filter.
      guarded(() => commands.cmdStartFilter(paneHandle));
      e.stopPropagation();
    } else if (
      filter_mode === "quick_search" &&
      e.key == "ArrowLeft" &&
      noModifiers
    ) {
      if (filter !== null && focused && filter.length > 0) {
        guarded(() =>
          commands.setFilter(
            paneHandle,
            focused.substring(0, filter.length - 1),
            null,
          ),
        );
      }
    } else if (
      filter_mode === "quick_search" &&
      e.key == "ArrowRight" &&
      noModifiers
    ) {
      if (filter !== null && focused && filter.length < focused.length) {
        guarded(() =>
          commands.setFilter(
            paneHandle,
            focused.substring(0, filter.length + 1),
            null,
          ),
        );
      }
    } else {
      return;
    }

    e.preventDefault();
  };

  const onClick = useCallback(
    (e: React.MouseEvent<HTMLLIElement>) => {
      if (suppressClickRef.current) {
        suppressClickRef.current = false;
        return;
      }
      // Focus is handled by mouseDown (both <ul> and DnD handlers).
      // onClick only handles modifier-key actions.
      const name = e.currentTarget.dataset.name;
      if (!name) return;
      if (modKey(e)) {
        safe(commands.toggleSelected(paneHandle, name, false));
      } else if (e.shiftKey) {
        safe(commands.selectRange(paneHandle, name));
      }
    },
    [paneHandle],
  );

  const contextMenuFileRef = useRef<string | null>(null);
  const contextMenuPosRef = useRef({ x: 0, y: 0 });
  const [contextMenuIsParentDir, setContextMenuIsParentDir] = useState(false);
  const [contextMenuFollow, setContextMenuFollow] = useState<FollowKind>(null);
  const [contextMenuActions, setContextMenuActions] =
    useState<RowActions | null>(null);
  const [contextMenuIsFile, setContextMenuIsFile] = useState(false);

  const [contextMenuOnFile, setContextMenuOnFile] = useState(true);

  // Whether the native Windows shell menu applies to this pane.
  const shellMenuAvailable = IS_WINDOWS_HOST && props.is_host_local;

  /// Pop the classic shell context menu at the last right-click position.
  /// `fileName` null means the pane background (and `..` also targets the
  /// directory itself — the shell has no concept of a parent-dir row).
  const openShellMenu = useCallback(
    async (fileName: string | null) => {
      const { x, y } = contextMenuPosRef.current;
      const onBackground = fileName === null || fileName === "..";
      if (!onBackground && !selectedLookupRef.current.has(fileName)) {
        // Focus must land before the backend reads the selection.
        await safeSilent(commands.focus(paneHandle, fileName));
      }
      safe(commands.shellContextMenu(paneHandle, x, y, onBackground));
    },
    [paneHandle],
  );

  const focusSelf = useCallback(() => {
    (filter != null ? inputRef : containerRef).current?.focus();
  }, [filter]);

  /// Restore focus ourselves when a context menu closes. Radix would aim
  /// for whatever held focus when the menu opened, which is a node inside
  /// the previous menu whenever one context menu is opened directly from
  /// another — unmounted by then, leaving focus on `<body>`. The column
  /// header additionally isn't focusable at all. A menu on a pane that
  /// doesn't hold focus hands it back to the pane or terminal that does.
  const refocusPane = useCallback(
    (e: Event) => {
      e.preventDefault();
      if (modalOpen) return;
      if (active) focusSelf();
      else window.dispatchEvent(new Event(REFOCUS_EVENT));
    },
    [active, modalOpen, focusSelf],
  );
  useEffect(() => {
    if (!active || modalOpen) return;
    window.addEventListener(REFOCUS_EVENT, focusSelf);
    return () => window.removeEventListener(REFOCUS_EVENT, focusSelf);
  }, [active, modalOpen, focusSelf]);

  const onContextMenu = useCallback(
    (e: React.MouseEvent<HTMLUListElement>) => {
      // Keyboard-requested menu — MainWindow relays it to the active pane.
      // preventDefault keeps Radix from anchoring at the webview's point.
      if (isKeyboardContextMenu(e.nativeEvent)) {
        e.preventDefault();
        return;
      }

      // Find which file row was right-clicked
      const target = e.target as HTMLElement;
      const li = target.closest("li[data-name]") as HTMLElement | null;
      contextMenuPosRef.current = { x: e.clientX, y: e.clientY };

      // Shift+right-click skips our menu and goes straight to the shell
      // menu; preventDefault keeps the Radix trigger from opening.
      if (shellMenuAvailable && e.shiftKey) {
        e.preventDefault();
        void openShellMenu(li ? li.dataset.name! : null);
        return;
      }

      if (!li) {
        // Right-clicked on empty space — show the pane-level context menu
        setContextMenuOnFile(false);
        setContextMenuIsParentDir(false);
        setContextMenuFollow(null);
        setContextMenuActions(null);
        setContextMenuIsFile(false);
        return;
      }

      const fileName = li.dataset.name!;
      contextMenuFileRef.current = fileName;
      setContextMenuOnFile(true);
      setContextMenuIsParentDir(fileName === "..");

      // If right-clicked file is not in the selection, focus it (clearing selection)
      const inSelection = selectedLookupRef.current.has(fileName);
      if (fileName !== ".." && !inSelection) {
        safeSilent(commands.focus(paneHandle, fileName));
      }

      // Entry-specific items describe what the commands will act on: focus
      // stays put when the click lands inside the selection, so that row —
      // not the clicked one — is the target.
      const targetKey = inSelection
        ? (focusedRef.current ?? fileName)
        : fileName;
      const row = fileWindowRef.current.items.find(
        (f) => (f.key ?? f.name) === targetKey,
      );
      setContextMenuFollow(followKind(row));
      setContextMenuActions(row?.actions ?? null);
      setContextMenuIsFile(!!row && !row.is_dir && row.name !== "..");
    },
    [paneHandle, shellMenuAvailable, openShellMenu],
  );

  const lastViewportReportRef = useRef<[number, number, number]>([-1, -1, -1]);
  // Send initial viewport report (and re-send on navigation, or when a
  // density change alters how many rows the pane holds).
  useEffect(() => {
    lastViewportReportRef.current = [-1, -1, -1];
    const container = containerRef.current;
    if (container) {
      const firstVisible = Math.floor(container.scrollTop / rowHeight);
      const visibleCount = Math.ceil(container.clientHeight / rowHeight);
      lastViewportReportRef.current = [firstVisible, visibleCount, -1];
      safeSilent(commands.setViewport(paneHandle, firstVisible, visibleCount));
    }
  }, [path, paneHandle, rowHeight]);

  // The window is sized to the viewport, and a pane that grows without
  // scrolling would otherwise never ask for the rows it now shows.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver(() => {
      const visibleCount = Math.ceil(container.clientHeight / rowHeight);
      if (visibleCount === lastViewportReportRef.current[1]) return;
      const firstVisible = Math.floor(container.scrollTop / rowHeight);
      lastViewportReportRef.current = [
        firstVisible,
        visibleCount,
        fileWindowRef.current.offset,
      ];
      safeSilent(commands.setViewport(paneHandle, firstVisible, visibleCount));
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, [paneHandle, rowHeight]);

  const isEmpty = !showSpinner && !loading && file_window.total_count === 0;

  const topSpacerStyle = useMemo(
    () => ({ height: file_window.offset * rowHeight, flexShrink: 0 }),
    [file_window.offset, rowHeight],
  );
  const bottomSpacerStyle = useMemo(
    () => ({
      height:
        (file_window.total_count -
          file_window.offset -
          file_window.items.length) *
        rowHeight,
      flexShrink: 0,
    }),
    [
      file_window.total_count,
      file_window.offset,
      file_window.items.length,
      rowHeight,
    ],
  );

  const syncScroll = (el: HTMLElement) => {
    tableHeaderRef.current!.scrollLeft = el.scrollLeft;

    // Report viewport position to Rust for window sliding.
    const fw = fileWindowRef.current;
    const firstVisible = Math.floor(el.scrollTop / rowHeight);
    const visibleCount = Math.ceil(el.clientHeight / rowHeight);
    const isInitial = lastViewportReportRef.current[0] === -1;

    if (!isInitial) {
      const margin = visibleCount;
      const nearStart = firstVisible <= fw.offset + margin && fw.offset > 0;
      const nearEnd =
        firstVisible + visibleCount >= fw.offset + fw.items.length - margin &&
        fw.offset + fw.items.length < fw.total_count;
      if (!nearStart && !nearEnd) return;
    }

    if (
      lastViewportReportRef.current[0] === firstVisible &&
      lastViewportReportRef.current[1] === visibleCount &&
      lastViewportReportRef.current[2] === fw.offset
    ) {
      return;
    }
    lastViewportReportRef.current = [firstVisible, visibleCount, fw.offset];
    safeSilent(commands.setViewport(paneHandle, firstVisible, visibleCount));
  };
  const onScroll: React.UIEventHandler<HTMLElement> = (e) =>
    syncScroll(e.currentTarget);
  // Alt+wheel moves the cursor a row per wheel event, whatever its delta:
  // scroll acceleration makes a slow notch a fraction of a row and a fast
  // one a dozen, while each notch is still one event.
  const cursorWheel = (e: WheelEvent) => {
    if (!e.altKey) return false;
    // macOS turns Shift+wheel on a mouse into horizontal deltas.
    const jump = Math.sign(
      e.deltaY !== 0 ? e.deltaY : e.shiftKey ? e.deltaX : 0,
    );
    const selecting = e.shiftKey;
    if (wheelSelectingRef.current && !selecting) endWheelSelect();
    if (jump !== 0) {
      if (selecting) {
        wheelSelectingRef.current = true;
        guarded(() => commands.wheelSelect(paneHandle, jump));
      } else {
        relativeJump(jump);
      }
    }
    return true;
  };
  // A Shift+Alt+wheel selection lasts while both are held.
  const wheelSelectingRef = useRef(false);
  const endWheelSelect = useCallback(() => {
    if (!wheelSelectingRef.current) return;
    wheelSelectingRef.current = false;
    safeSilent(commands.endDragSelection(paneHandle));
  }, [paneHandle]);
  useEffect(() => {
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === "Shift" || e.key === "Alt") endWheelSelect();
    };
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", endWheelSelect);
    return () => {
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", endWheelSelect);
    };
  }, [endWheelSelect]);
  // The wheel stops at the edge of the rows Rust has sent, and goes on
  // once the window slides. Alt+wheel moves the cursor instead.
  useWheelScroll(containerRef, {
    claim: cursorWheel,
    sync: syncScroll,
    bounds: (el) => {
      const fw = fileWindowRef.current;
      const end = fw.offset + fw.items.length;
      return [
        fw.offset > 0 ? fw.offset * rowHeight : 0,
        end < fw.total_count
          ? end * rowHeight - el.clientHeight
          : Number.POSITIVE_INFINITY,
      ];
    },
  });

  const widthPrefix = `pane-${paneHandle}-column-`;
  const paneRef = useRef<HTMLDivElement>(null);

  const runtimeState = useRuntimeState();
  const savedWidths = runtimeState?.column_widths?.[String(paneHandle)];

  const otherPaneHandle = 1 - paneHandle;
  const columnPanes = (bothPanes: boolean) =>
    bothPanes ? [paneHandle, otherPaneHandle] : [paneHandle];

  const commitColumnWidth = (key: string, px: number, bothPanes: boolean) => {
    for (const handle of columnPanes(bothPanes)) {
      safe(
        commands.updateRuntimeState(
          `column_widths.${handle}.${key}`,
          Math.round(px),
        ),
      );
    }
  };

  const autoSizeColumn = (key: string, bothPanes: boolean) => {
    const panes = columnPanes(bothPanes);
    commitColumnWidth(key, fitColumnWidth(panes, key), bothPanes);
  };

  // External drag-and-drop (files from outside the app).
  // MainWindow dispatches custom events on the [data-pane-handle] element.
  useEffect(() => {
    const el = paneRef.current;
    if (!el) return;

    const onDragOver = (e: Event) => {
      const { x, y } = (e as CustomEvent).detail;
      const target = resolveDropTarget(x, y);
      if (target) {
        highlightDropTarget(target);
      }
    };

    const onDragLeave = () => {
      clearDropHighlights();
    };

    const onDrop = (e: Event) => {
      const { paths, x, y } = (e as CustomEvent).detail;
      const target = resolveDropTarget(x, y);
      clearDropHighlights();
      if (paths?.length) {
        safe(
          commands.externalDrop(
            paneHandle,
            target?.subdirectory ?? null,
            paths,
          ),
        );
      }
    };

    el.addEventListener("external-drag-over", onDragOver);
    el.addEventListener("external-drag-leave", onDragLeave);
    el.addEventListener("external-drop", onDrop);
    return () => {
      el.removeEventListener("external-drag-over", onDragOver);
      el.removeEventListener("external-drag-leave", onDragLeave);
      el.removeEventListener("external-drop", onDrop);
    };
  }, [paneHandle, path]);

  return (
    <div
      ref={paneRef}
      className={styles.pane}
      role="region"
      aria-label={paneHandle === 0 ? "Left pane" : "Right pane"}
      data-pane-handle={paneHandle}
      onClick={() => guarded(() => commands.focus(paneHandle, null))}
      onMouseDownCapture={(e) => {
        // A click on the pane that has focus re-takes it. Rust already
        // holds it as focused, so no state change would; this recovers
        // with the mouse if the DOM ever loses track.
        if (active && !modalOpen && ownsEvent(paneRef.current, e)) {
          focusSelf();
        }
      }}
      onMouseDown={(e) => {
        if (e.button === 3) {
          e.preventDefault();
          guarded(() => commands.cmdNavigateBack(paneHandle));
        } else if (e.button === 4) {
          e.preventDefault();
          guarded(() => commands.cmdNavigateForward(paneHandle));
        }
      }}
    >
      <FilterInput
        value={filter}
        filterMode={filter_mode}
        listId={listId}
        activeDescendant={activeDescendant}
        inputRef={inputRef}
        onKeyDown={onkeydownFilter}
        onChange={(value) =>
          guarded(() => commands.setFilter(paneHandle, value, null))
        }
      />
      <div className={styles.headerArea}>
        {preferences?.settings.appearance?.show_pane_header !== false ? (
          <ContextMenu.Root>
            <ContextMenu.Trigger asChild>
              <div
                className={`${styles.header} ${active ? styles.headerActive : current ? styles.headerCurrent : ""}`}
              >
                <span className={styles.focusDot} aria-hidden />
                {swappable && (
                  <button
                    type="button"
                    className={styles.swapButton}
                    tabIndex={-1}
                    aria-label={
                      paneHandle === 0 ? "Show right pane" : "Show left pane"
                    }
                    title={
                      paneHandle === 0
                        ? "Show right pane (Tab)"
                        : "Show left pane (Tab)"
                    }
                    onMouseDown={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                    }}
                    onClick={(e) => {
                      e.stopPropagation();
                      safe(commands.focus(1 - paneHandle, null));
                    }}
                  >
                    <IconPaneSide side={paneHandle} />
                  </button>
                )}
                <VfsSelector
                  vfsDisplayName={vfs_display_name}
                  vfsTargets={vfsTargets}
                  paneHandle={paneHandle}
                  activeVfsId={path.vfs_id}
                  activePath={path.path}
                  open={isVfsSelectorOpen}
                />
                <div
                  className={styles.headerPath}
                  role="navigation"
                  aria-label="Path"
                  title={props.display_path}
                >
                  <PathBreadcrumbs
                    breadcrumbs={breadcrumbs}
                    paneHandle={paneHandle}
                    displayPath={props.display_path}
                    onMenuCloseAutoFocus={refocusPane}
                  />
                </div>
                {(context_badges ?? []).map((badge, i) => (
                  <GitBranchBadge key={i} badge={badge} />
                ))}
                {fs_stats?.available_bytes !== undefined && (
                  <button
                    type="button"
                    className={styles.freeSpace}
                    tabIndex={-1}
                    title="Volume properties"
                    onMouseDown={(e) => {
                      // Don't let the pane's click handler steal focus.
                      e.stopPropagation();
                    }}
                    onClick={(e) => {
                      e.stopPropagation();
                      commands.dialog("root_properties", paneHandle);
                    }}
                  >
                    {formatBytes(fs_stats.available_bytes)} free
                  </button>
                )}
                <button
                  type="button"
                  className={styles.maximizeButton}
                  tabIndex={-1}
                  aria-label={
                    maximized ? "Restore split layout" : "Maximize pane"
                  }
                  title={shortcuts.label(
                    maximized ? "Restore split layout" : "Maximize pane",
                    "toggle_maximized",
                  )}
                  onMouseDown={(e) => {
                    // Keep focus in the file list, and out of the pane's click
                    // handler.
                    e.preventDefault();
                    e.stopPropagation();
                  }}
                  onClick={async (e) => {
                    e.stopPropagation();
                    // What maximizes is whatever has focus, so claim it first.
                    await safe(commands.focus(paneHandle, null));
                    safe(commands.cmdToggleMaximized(paneHandle));
                  }}
                >
                  {maximized ? <IconRestore /> : <IconMaximize />}
                </button>
              </div>
            </ContextMenu.Trigger>
            <LocationContextMenuContent
              paneHandle={paneHandle}
              displayPath={props.display_path}
              onCloseAutoFocus={refocusPane}
            />
          </ContextMenu.Root>
        ) : (
          <div className={styles.hiddenAnchor} aria-hidden>
            <VfsSelector
              vfsDisplayName={vfs_display_name}
              vfsTargets={vfsTargets}
              paneHandle={paneHandle}
              activeVfsId={path.vfs_id}
              activePath={path.path}
              open={isVfsSelectorOpen}
            />
          </div>
        )}
        <HistoryNavigator
          entries={historyEntries}
          currentIndex={historyCurrentIndex}
          initialDirection={historyInitialDirection}
          paneHandle={paneHandle}
          persistent={historyPersistent}
          open={isHistoryNavigatorOpen}
        />
        <SortMenu
          sorting={sorting}
          foldersFirst={sortFoldersFirst}
          paneHandle={paneHandle}
          open={isSortMenuOpen}
        />
      </div>
      <ContextMenu.Root>
        <ContextMenu.Trigger asChild>
          <div className={styles.tableHeader} ref={tableHeaderRef}>
            <div className={styles.tableHeaderInner}>
              {columns.map((column, i) => (
                <ColumnHeader
                  key={column.key}
                  widthPrefix={widthPrefix}
                  linkedWidthPrefix={`pane-${otherPaneHandle}-column-`}
                  sorting={sorting}
                  column={column}
                  index={i}
                  savedWidth={savedWidths?.[column.key]}
                  onWidthCommit={(px, bothPanes) =>
                    commitColumnWidth(column.key, px, bothPanes)
                  }
                  onAutoSize={(bothPanes) =>
                    autoSizeColumn(column.key, bothPanes)
                  }
                  onSort={(key, asc) => {
                    guarded(() =>
                      commands.setSorting(paneHandle, {
                        key: key as Parameters<
                          typeof commands.setSorting
                        >[1]["key"],
                        asc,
                      }),
                    );
                  }}
                  onReorder={(from, to) => {
                    const cfg = preferences?.settings?.appearance?.columns;
                    if (!cfg || cfg.length === 0) return;
                    safe(
                      commands.updatePreference(
                        "appearance.columns",
                        moveColumn(cfg, from, to, metadata_traits),
                      ),
                    );
                  }}
                />
              ))}
              <div className={columnStyles.dropIndicator} />
            </div>
          </div>
        </ContextMenu.Trigger>
        <ColumnsContextMenuContent
          columns={preferences?.settings?.appearance?.columns}
          traits={metadata_traits}
          onCloseAutoFocus={refocusPane}
        />
      </ContextMenu.Root>
      {file_window && (
        <div className={styles.filesFrame}>
          <ContextMenu.Root>
            <ContextMenu.Trigger asChild>
              <ul
                className={`${styles.files} ${
                  filter && filter_mode === "filter" ? styles.filesFiltered : ""
                } ${showSpinner ? styles.filesStale : ""}`}
                id={listId}
                role="listbox"
                aria-multiselectable
                aria-label={props.display_path}
                aria-activedescendant={activeDescendant}
                aria-busy={isBusy || loading}
                aria-describedby={isEmpty ? emptyId : undefined}
                ref={containerRef}
                onKeyDown={onkeydown}
                onMouseDown={onMouseDown}
                onClick={(e) => e.stopPropagation()}
                onContextMenu={onContextMenu}
                tabIndex={-1}
                onScroll={onScroll}
              >
                <div style={topSpacerStyle} aria-hidden />
                {isEmpty && (
                  <li
                    id={emptyId}
                    className={styles.emptyPlaceholder}
                    aria-hidden
                  >
                    Nothing to see here
                  </li>
                )}
                {file_window.items.map((row, i) => {
                  const rowKey = row.key ?? row.name;
                  const isCursor = rowKey === focused;
                  const isFocused = active && isCursor;
                  return (
                    <FileRow
                      key={rowKey}
                      row={row}
                      id={rowId(file_window.offset + i)}
                      posInSet={file_window.offset + i + 1}
                      // Unknown (-1) while streaming: the total moves with
                      // every batch, and would re-render each row with it.
                      setSize={loading ? -1 : file_window.total_count}
                      columns={columns}
                      isCursor={isCursor}
                      isFocused={isFocused}
                      isSelected={selectedLookup.has(rowKey)}
                      filter={isFocused ? filter : null}
                      filterMode={filter_mode}
                      widthPrefix={widthPrefix}
                      dateFormat={
                        preferences?.settings?.appearance?.date_format
                      }
                      timeFormat={
                        preferences?.settings?.appearance?.time_format
                      }
                      locale={locale}
                      siSizePrefixes={
                        preferences?.settings?.appearance?.si_size_prefixes
                      }
                      sizeUnits={sizeUnits}
                      onClick={onClick}
                      onMouseDown={onDndMouseDown}
                      onOpen={onOpen}
                    />
                  );
                })}
                <div style={bottomSpacerStyle} aria-hidden />
                <div
                  className={styles.dragRect}
                  ref={dragRectRef}
                  aria-hidden
                />
              </ul>
            </ContextMenu.Trigger>
            {contextMenuOnFile ? (
              <FileContextMenuContent
                paneHandle={paneHandle}
                isParentDir={contextMenuIsParentDir}
                follow={contextMenuFollow}
                actions={contextMenuActions}
                isFile={contextMenuIsFile}
                onCloseAutoFocus={refocusPane}
                onShellMenu={
                  shellMenuAvailable
                    ? () => void openShellMenu(contextMenuFileRef.current)
                    : undefined
                }
              />
            ) : (
              <PaneContextMenuContent
                paneHandle={paneHandle}
                isHostLocal={props.is_host_local}
                onCloseAutoFocus={refocusPane}
                onShellMenu={
                  shellMenuAvailable
                    ? () => void openShellMenu(null)
                    : undefined
                }
              />
            )}
          </ContextMenu.Root>
          {/* WebKit aims the keyboard's menu key at the focused element's
            bottom-left pixel and sends nothing when a scrollbar is there
            (EventHandler::sendContextMenuEventForKey) — here, the list's
            own horizontal scrollbar. This pixel takes the event instead,
            and the window hands it to the active pane. */}
          <div className={styles.menuKeyTarget} aria-hidden />
        </div>
      )}
      <div className="dnd-ghost" ref={dndGhostRef} aria-hidden />
      {preferences?.settings.appearance?.show_pane_status !== false && (
        <div className={styles.statusbar}>
          <div className={styles.statusbarInner}>
            {busy && (
              <>
                <Spinner />{" "}
              </>
            )}
            {showSpinner && "Loading file list..."}
            {!showSpinner && loading && (
              <>
                Loading... (
                {(stats.file_count + stats.dir_count).toLocaleString(locale)}{" "}
                items so far)
              </>
            )}
            {!showSpinner &&
              !loading &&
              stats.selected_file_count + stats.selected_dir_count > 0 && (
                <>
                  {stats.selected_file_count} files, {stats.selected_dir_count}{" "}
                  directories selected,{" "}
                  {stats.selected_bytes.toLocaleString(locale)} bytes total
                  {stats.total_count != null &&
                    ` (showing ${stats.file_count + stats.dir_count} of ${stats.total_count})`}
                </>
              )}
            {!showSpinner &&
              !loading &&
              stats.selected_file_count + stats.selected_dir_count === 0 && (
                <>
                  {stats.file_count} files, {stats.dir_count} directories
                  {stats.hidden_count > 0 && `, ${stats.hidden_count} hidden`}
                  {stats.total_count != null &&
                    ` (showing ${stats.file_count + stats.dir_count} of ${stats.total_count})`}
                </>
              )}
            {!showSpinner && !loading && partial != null && (
              <span className={styles.partial} title={partial}>
                {" "}
                (partial)
              </span>
            )}
            {showActivity && hasActivity && (
              <span>
                {" · "}
                {activityLabels.join(" · ")}… (Esc cancels)
              </span>
            )}
            {vfsProgress && (
              <span>
                {" · "}
                <VfsProgressLine progress={vfsProgress} locale={locale} />
              </span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

export default memo(PaneInner);
