import {
  useState,
  useEffect,
  useRef,
  useMemo,
  useLayoutEffect,
  useCallback,
  memo,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import * as ContextMenu from "@radix-ui/react-context-menu";
import iconMapping from "../assets/mapping.json";
import { safeCommand, safeCommandSilent } from "../lib/ipc";
import { modifiers } from "../lib/commands";
import { Breadcrumb, VfsTarget } from "../lib/types";
import { ModalState } from "./modals/ModalContent";
import {
  File,
  FilterMode,
  PaneState,
  DndFileInfo,
  FileRowContext,
} from "./types";
import { getSiPrefixedNumber } from "./utils";
import { ColumnHeader, columns } from "./columns";
import {
  FileContextMenuContent,
  PaneContextMenuContent,
  BreadcrumbContextMenuContent,
} from "./ContextMenu";
import styles from "./Pane.module.scss";
import menuStyles from "./Menu.module.scss";
import columnStyles from "./Columns.module.scss";

function PathBreadcrumbs(props: {
  breadcrumbs: Breadcrumb[];
  paneHandle: number;
  displayPath: string;
}) {
  const { breadcrumbs, paneHandle, displayPath } = props;

  // Build the display path up to each breadcrumb by joining labels.
  // The last breadcrumb gets the full display_path.
  const pathUpTo = (index: number): string => {
    if (index === breadcrumbs.length - 1) return displayPath;
    return breadcrumbs
      .slice(0, index + 1)
      .map((c) => c.label)
      .join("");
  };

  return (
    <>
      {breadcrumbs.map((crumb, i) => (
        <ContextMenu.Root key={i}>
          <ContextMenu.Trigger asChild>
            <a
              className={styles.pathBreadcrumb}
              href="#"
              tabIndex={-1}
              onClick={(e) => {
                e.preventDefault();
                if (i === breadcrumbs.length - 1) {
                  safeCommand("dialog", { paneHandle, dialog: "navigate" });
                } else {
                  safeCommand("navigate", {
                    paneHandle,
                    path: crumb.nav_path,
                    exact: true,
                  });
                }
              }}
            >
              {crumb.label}
            </a>
          </ContextMenu.Trigger>
          <BreadcrumbContextMenuContent displayPath={pathUpTo(i)} />
        </ContextMenu.Root>
      ))}
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
};

const fileNames = iconMapping.light.fileNames as Record<string, string>;
const fileExtensions = iconMapping.light.fileExtensions as Record<
  string,
  string
>;
const iconDefs = iconMapping.iconDefinitions as unknown as Record<
  string,
  { fontCharacter: string; fontColor: string }
>;

function getFileIconChar(
  name: string,
  isDir: boolean,
): { ch: string; color: string } {
  if (isDir) return { ch: "\uE5FF", color: "" }; // folder icon fallback
  const icon =
    fileNames[name] ||
    fileExtensions[name.substr(name.indexOf(".") + 1)] ||
    iconMapping.light.file;
  const { fontCharacter, fontColor } = iconDefs[icon];
  return {
    ch: String.fromCodePoint(parseInt(fontCharacter, 16)),
    color: fontColor,
  };
}

type FileRowProps = {
  row: File;
  isFocused: boolean;
  isSelected: boolean;
  filter: string | null;
  filterMode: FilterMode;
  widthPrefix: string;
  onClick: React.MouseEventHandler<HTMLLIElement>;
  onMouseDown: React.MouseEventHandler<HTMLLIElement>;
  onOpen: (file: File) => void;
};

const FileRow = memo(
  function FileRow({
    row,
    isFocused,
    isSelected,
    filter,
    filterMode,
    widthPrefix,
    onClick,
    onMouseDown,
    onOpen,
  }: FileRowProps) {
    const ctx: FileRowContext = { isFocused, filter, filterMode };
    return (
      <li
        data-name={row.name}
        data-is-dir={row.is_dir ? "true" : undefined}
        className={`${styles.fileItem} ${isFocused ? styles.focused : ""} ${isSelected ? styles.selected : ""}`}
        onClick={onClick}
        onMouseDown={onMouseDown}
        onDoubleClick={() => onOpen(row)}
      >
        {columns.map((column) => (
          <div
            key={column.key}
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
    prev.row.size === next.row.size &&
    prev.row.modified === next.row.modified &&
    prev.row.mode === next.row.mode &&
    prev.row.is_dir === next.row.is_dir &&
    prev.row.is_symlink === next.row.is_symlink &&
    prev.isFocused === next.isFocused &&
    prev.isSelected === next.isSelected &&
    prev.filter === next.filter &&
    prev.filterMode === next.filterMode &&
    prev.widthPrefix === next.widthPrefix &&
    prev.onClick === next.onClick &&
    prev.onMouseDown === next.onMouseDown &&
    prev.onOpen === next.onOpen,
);

const VFS_ICONS: Record<string, string> = {
  local: "\u{f02ca}",
  s3: "\u{f0e0f}",
  sftp: "\u{eb3a}",
  archive: "\u{eaa0}",
  archive_zip: "\u{eaa0}",
};

function VfsSelector({
  vfsDisplayName,
  vfsTargets,
  paneHandle,
  activeVfsId,
  open,
}: {
  vfsDisplayName: string;
  vfsTargets: VfsTarget[];
  paneHandle: number;
  activeVfsId: number;
  open: boolean;
}) {
  // Track when we're opening a mount dialog so we don't steal focus back
  const openingDialogRef = useRef(false);

  return (
    <DropdownMenu.Root
      open={open}
      onOpenChange={(v) => {
        if (!v && !openingDialogRef.current) safeCommand("close_modal");
        openingDialogRef.current = false;
      }}
    >
      <DropdownMenu.Trigger asChild>
        <button
          className={styles.vfsSelector}
          type="button"
          tabIndex={-1}
          onClick={(e) => {
            e.stopPropagation();
            safeCommand("dialog", { paneHandle, dialog: "select_vfs" });
          }}
          onMouseDown={(e) => {
            // Activate this pane without letting the .pane onClick steal focus later
            e.stopPropagation();
            safeCommandSilent("focus", { paneHandle });
          }}
        >
          {vfsDisplayName} &#x25BE;
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
            const isActive =
              target.vfs_id != null && target.vfs_id === activeVfsId;
            const icon = VFS_ICONS[target.type_name];
            return (
              <DropdownMenu.Item
                key={`${target.type_name}-${target.vfs_id ?? i}`}
                className={menuStyles.item}
                onSelect={(e) => {
                  // Prevent Radix from auto-closing the dropdown — the
                  // command handlers replace/close the modal on the Rust
                  // side, which updates `open` via props.
                  e.preventDefault();
                  if (target.vfs_id == null && target.mount_dialog) {
                    openingDialogRef.current = true;
                    safeCommand("dialog", {
                      paneHandle,
                      dialog: target.mount_dialog,
                    });
                  } else {
                    safeCommand("switch_vfs", {
                      paneHandle,
                      vfsId: target.vfs_id,
                      typeName: target.type_name,
                    });
                  }
                }}
              >
                <span className={menuStyles.itemIcon}>{icon}</span>
                <span className={menuStyles.itemLabel}>
                  {target.display_name}
                  {target.label && ` (${target.label})`}
                  {target.vfs_id == null && " (connect...)"}
                </span>
                {isActive && (
                  <span className={menuStyles.itemCheck}>{"\u2713"}</span>
                )}
                {target.vfs_id != null && target.type_name !== "local" && (
                  <button
                    className={menuStyles.itemDismiss}
                    onClick={(e) => {
                      e.stopPropagation();
                      safeCommand("unmount_vfs", {
                        paneHandle,
                        vfsId: target.vfs_id,
                      });
                    }}
                    tabIndex={-1}
                  >
                    {"\u2715"}
                  </button>
                )}
              </DropdownMenu.Item>
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
  inputRef,
  onKeyDown,
  onChange,
  onFocus,
}: {
  value: string | null;
  filterMode: string;
  inputRef: React.RefObject<HTMLInputElement | null>;
  onKeyDown: React.KeyboardEventHandler;
  onChange: (value: string) => void;
  onFocus: () => void;
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

  const input = (
    <input
      className={isVisibleFilter ? styles.filterBarInput : undefined}
      type="text"
      value={isVisibleFilter ? localValue : (value ?? "")}
      placeholder={isVisibleFilter ? "Filter (regex)" : undefined}
      onChange={(e) => {
        setLocalValue(e.target.value);
        onChange(e.target.value);
      }}
      ref={inputRef}
      onKeyDown={onKeyDown}
      onFocus={onFocus}
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

const ITEM_SIZE = 22;

function PaneInner(
  props: PaneState & {
    paneHandle: number;
    active: boolean;
    modalOpen: boolean;
    modal?: ModalState;
  },
) {
  const {
    paneHandle,
    active,
    modalOpen,
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
    modal,
  } = props;

  const isVfsSelectorOpen =
    modal?.type === "select_vfs" && modal?.context?.pane_handle === paneHandle;
  const vfsTargets: VfsTarget[] =
    (isVfsSelectorOpen && modal?.data?.targets) || [];
  const focusedIndex = props.focused_index ?? -1;
  // Allow interaction when loading (partial results visible) but not when pending_path is set (no files yet)
  const isBusy = !!pending_path && !loading;
  const command = (cmd: string, args: object = {}, also_when_busy = false) => {
    if (also_when_busy || !isBusy) {
      safeCommand(cmd, { paneHandle, ...args });
    }
  };

  const [showSpinner, setShowSpinner] = useState(false);

  useEffect(() => {
    let timeout: ReturnType<typeof setTimeout> | null = null;
    // Show full spinner only when pending_path is set and no partial results yet
    if (pending_path && !loading) {
      // 200 ms of grace period before showing the loading screen to
      // appear smoother.
      timeout = setTimeout(() => setShowSpinner(true), 200);
    } else {
      setShowSpinner(false);
    }
    return () => {
      if (timeout) clearTimeout(timeout);
    };
  }, [pending_path, loading]);

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

  // --- DnD (drag-and-drop between panes) refs ---
  const dndRef = useRef<LocalDndState | null>(null);
  const dndGhostRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!active || focusedIndex < 0 || !container) return;

    const containerHeight = container.clientHeight;
    const scrollTop = container.scrollTop;
    const itemTop = focusedIndex * ITEM_SIZE;
    const itemBottom = itemTop + ITEM_SIZE;

    if (itemTop < scrollTop) {
      container.scrollTop = itemTop;
    } else if (itemBottom > scrollTop + containerHeight) {
      container.scrollTop = itemBottom - containerHeight;
    }
    // Intentionally excluding file_window — this should only fire when the
    // focused item changes position, not when the window slides on scroll.
  }, [active, focusedIndex]);

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
      safeCommandSilent("set_selection_by_indices", {
        paneHandle,
        start: startIdx,
        end: endIdx,
        additive: drag.mode === "ctrl",
      });
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
      const startIdx = Math.max(0, Math.floor(rectTop / ITEM_SIZE));
      const endIdx = Math.max(0, Math.ceil(rectBottom / ITEM_SIZE) - 1);

      if (
        startIdx !== drag.lastSentStartIdx ||
        endIdx !== drag.lastSentEndIdx
      ) {
        drag.lastSentStartIdx = startIdx;
        drag.lastSentEndIdx = endIdx;
        sendDragSelection(drag, startIdx, endIdx);
      }
    },
    [sendDragSelection],
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
          safeCommandSilent("end_drag_selection", { paneHandle });
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
        safeCommandSilent("end_drag_selection", { paneHandle });
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
      // Only start drag from empty space — not on file icon or filename text
      const target = e.target as HTMLElement;
      if (target.closest(".file-icon") || target.closest(".filename-part"))
        return;
      e.preventDefault(); // block text selection

      const container = containerRef.current;
      if (!container) return;
      if (!fileWindowRef.current.total_count) return;

      // Focus the file under cursor (using DOM, not scroll math).
      // This must happen on mouseDown, not onClick, because a tiny drag
      // (>5px) suppresses the click event.
      const li = (e.target as HTMLElement).closest(
        "li[data-name]",
      ) as HTMLElement | null;
      if (li?.dataset.name) {
        safeCommandSilent("focus", {
          paneHandle,
          filename: li.dataset.name,
        });
      } else {
        safeCommandSilent("focus", { paneHandle });
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
        mode: e.ctrlKey ? "ctrl" : "normal",
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
      const target = e.target as HTMLElement;
      if (!target.closest(".file-icon") && !target.closest(".filename-part"))
        return;
      const fileName = e.currentTarget.dataset.name;
      if (!fileName || fileName === "..") return;

      if (!e.shiftKey) {
        safeCommandSilent("focus", { paneHandle, filename: fileName });
      }

      e.preventDefault();
      e.stopPropagation(); // prevent drag-to-select

      const fw = fileWindowRef.current;
      const currentSelected = selectedLookupRef.current;
      const filesToDrag = currentSelected.has(fileName)
        ? fw.items.filter((f) => currentSelected.has(f.name) && f.name !== "..")
        : [fw.items.find((f) => f.name === fileName)!];

      dndRef.current = {
        active: false,
        startX: e.clientX,
        startY: e.clientY,
        files: filesToDrag.map((f) => ({ name: f.name, is_dir: f.is_dir })),
      };
    },
    [paneHandle],
  );

  const cleanupDnd = useCallback(() => {
    const ghost = dndGhostRef.current;
    if (ghost) ghost.style.display = "none";
    document
      .querySelectorAll(".dnd-drop-target, .dnd-drop-hover")
      .forEach((el) => {
        el.classList.remove("dnd-drop-target", "dnd-drop-hover");
      });
  }, []);

  useEffect(() => {
    const onDndMouseMove = (e: MouseEvent) => {
      const dnd = dndRef.current;
      if (!dnd) return;

      // Detect mouseup that happened outside the window
      if (e.buttons === 0) {
        if (dnd.active) {
          safeCommandSilent("cancel_dnd");
          suppressClickRef.current = true;
        }
        cleanupDnd();
        dndRef.current = null;
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
            if (f.is_dir) {
              ghost.innerHTML = `<div class="file-icon folder"></div> ${f.name}`;
            } else {
              const { ch, color } = getFileIconChar(f.name, f.is_dir);
              ghost.innerHTML = `<div class="file-icon" style="color: ${color}">${ch}</div> ${f.name}`;
            }
          } else {
            ghost.textContent = `${dnd.files.length} items`;
          }
          ghost.style.display = "flex";
        }

        safeCommandSilent("start_dnd", { paneHandle, files: dnd.files });
      }

      // Position ghost
      const ghost = dndGhostRef.current;
      if (ghost) {
        ghost.style.left = `${e.clientX + 12}px`;
        ghost.style.top = `${e.clientY + 12}px`;
      }

      // Highlight drop targets
      document
        .querySelectorAll(".dnd-drop-target, .dnd-drop-hover")
        .forEach((el) => {
          el.classList.remove("dnd-drop-target", "dnd-drop-hover");
        });

      const elementUnder = document.elementFromPoint(e.clientX, e.clientY);
      if (!elementUnder) return;

      const targetPane = elementUnder.closest(
        "[data-pane-handle]",
      ) as HTMLElement | null;
      if (targetPane) {
        const targetPaneHandle = parseInt(targetPane.dataset.paneHandle!, 10);
        const isSamePane = targetPaneHandle === paneHandle;
        const targetLi = elementUnder.closest(
          "li[data-is-dir='true']",
        ) as HTMLElement | null;

        if (!isSamePane) {
          targetPane.classList.add("dnd-drop-target");
        }

        if (targetLi) {
          const targetName = targetLi.dataset.name!;
          // In same pane: don't highlight ".." or dirs being dragged (can't drop into yourself)
          if (isSamePane) {
            const dnd = dndRef.current;
            const draggedNames = dnd
              ? new Set(dnd.files.map((f) => f.name))
              : new Set<string>();
            if (targetName !== ".." && !draggedNames.has(targetName)) {
              targetLi.classList.add("dnd-drop-hover");
            }
          } else {
            targetLi.classList.add("dnd-drop-hover");
          }
        }
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

      const elementUnder = document.elementFromPoint(e.clientX, e.clientY);
      if (!elementUnder) {
        safeCommandSilent("cancel_dnd");
        return;
      }

      const targetPane = elementUnder.closest(
        "[data-pane-handle]",
      ) as HTMLElement | null;
      if (!targetPane) {
        safeCommandSilent("cancel_dnd");
        return;
      }

      const targetPaneHandle = parseInt(targetPane.dataset.paneHandle!, 10);
      const isSamePane = targetPaneHandle === paneHandle;

      let subdirectory: string | null = null;
      const targetLi = elementUnder.closest(
        "li[data-is-dir='true']",
      ) as HTMLElement | null;
      if (targetLi) {
        const targetName = targetLi.dataset.name || null;
        if (isSamePane) {
          // Same pane: don't drop onto ".." or a dir being dragged
          const draggedNames = new Set(dnd.files.map((f) => f.name));
          if (
            targetName &&
            targetName !== ".." &&
            !draggedNames.has(targetName)
          ) {
            subdirectory = targetName;
          }
        } else if (targetName) {
          subdirectory = targetName;
        }
      }

      // Same pane requires a directory target (otherwise it's a no-op)
      if (isSamePane && !subdirectory) {
        safeCommandSilent("cancel_dnd");
        return;
      }

      safeCommand("execute_dnd", {
        destinationPane: targetPaneHandle,
        subdirectory,
        isMove: e.shiftKey,
      });
    };

    document.addEventListener("mousemove", onDndMouseMove);
    document.addEventListener("mouseup", onDndMouseUp);
    return () => {
      document.removeEventListener("mousemove", onDndMouseMove);
      document.removeEventListener("mouseup", onDndMouseUp);
    };
  }, [paneHandle, cleanupDnd]);

  // Cancel DnD when the directory changes
  useEffect(() => {
    const dnd = dndRef.current;
    if (dnd?.active) {
      cleanupDnd();
      safeCommandSilent("cancel_dnd");
    }
    dndRef.current = null;
  }, [path]);

  // --- End DnD logic ---

  const onOpen = useCallback(
    (file: File) => {
      if (!file) return;
      safeCommand("enter", { paneHandle });
    },
    [paneHandle],
  );

  const relativeJump = (delta: number, withSelection?: boolean) => {
    command("relative_jump", { offset: delta, withSelection: !!withSelection });
  };

  const onKeyDownCommon = (e: React.KeyboardEvent<Element>) => {
    const { isMac, noModifiers, ctrlOrMeta, insertKey } = modifiers(e);

    if (e.key == "ArrowDown" && (noModifiers || e.shiftKey)) {
      relativeJump(1, e.shiftKey);
    } else if (e.key == "ArrowUp" && (noModifiers || e.shiftKey)) {
      relativeJump(-1, e.shiftKey);
    } else if (e.key == "PageDown" && (noModifiers || e.shiftKey)) {
      relativeJump(10, e.shiftKey);
    } else if (e.key == "PageUp" && (noModifiers || e.shiftKey)) {
      relativeJump(-10, e.shiftKey);
    } else if (e.key == "Home" && noModifiers) {
      relativeJump(-Math.pow(2, 31), e.shiftKey);
    } else if (e.key == "End" && noModifiers) {
      relativeJump(Math.pow(2, 31) - 1, e.shiftKey);
    } else if (e.key == "Enter" && noModifiers) {
      const focusedFile = file_window.items[focusedIndex - file_window.offset];
      if (focusedFile) onOpen(focusedFile);
    } else if (e.key == "Tab" && noModifiers) {
      invoke("focus", { paneHandle: 1 - paneHandle });
    } else if (e.key == "Escape" && noModifiers) {
      command("cancel", {}, true);
      command("set_filter", { filter: null });
    } else if (e.key == insertKey && noModifiers) {
      command("toggle_selected", {
        focusNext: true,
      });
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
    const rect = li.getBoundingClientRect();
    setContextMenuIsParentDir(focused === "..");
    li.dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        clientX: rect.left,
        clientY: rect.bottom,
      }),
    );
  }, [focused]);

  const onkeydown = (e: React.KeyboardEvent<Element>) => {
    const { isMac, noModifiers, ctrlOrMeta, insertKey } = modifiers(e);

    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
      openContextMenu();
    } else if (e.key == "Backspace" && noModifiers) {
      command("navigate", { path: "..", exact: true }, true);
    } else if (e.key == "/" && noModifiers) {
      command("set_filter", { filter: "", mode: "filter" });
      inputRef.current?.focus();
    } else if (e.key.length == 1 && !e.ctrlKey && !e.shiftKey) {
      // Is this a good way to check for printable characters? Works for en-US,
      // but I have no idea how well it works for international IMEs.
      inputRef.current?.focus();
      return;
    }

    e.preventDefault();
  };

  const onkeydownFilter: React.KeyboardEventHandler = (e) => {
    const { isMac, noModifiers, ctrlOrMeta, insertKey } = modifiers(e);

    if (onKeyDownCommon(e)) {
      // ...
    } else if (e.key == "/" && noModifiers && filter_mode === "quick_search") {
      command("set_filter", { filter: filter || "", mode: "filter" });
    } else if (
      filter_mode === "quick_search" &&
      e.key == "ArrowLeft" &&
      noModifiers
    ) {
      if (filter !== null && filter.length > 0) {
        command("set_filter", {
          filter: focused!.substring(0, filter.length - 1),
        });
      }
    } else if (
      filter_mode === "quick_search" &&
      e.key == "ArrowRight" &&
      noModifiers
    ) {
      if (filter !== null && focused && filter.length < focused.length) {
        command("set_filter", {
          filter: focused.substring(0, filter.length + 1),
        });
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
      if (e.ctrlKey) {
        safeCommand("toggle_selected", {
          paneHandle,
          filename: e.currentTarget.dataset.name,
          focusNext: false,
        });
      } else if (e.shiftKey) {
        safeCommand("select_range", {
          paneHandle,
          filename: e.currentTarget.dataset.name,
        });
      }
    },
    [paneHandle],
  );

  const contextMenuFileRef = useRef<string | null>(null);
  const [contextMenuIsParentDir, setContextMenuIsParentDir] = useState(false);

  const [contextMenuOnFile, setContextMenuOnFile] = useState(true);

  const onContextMenu = useCallback(
    (e: React.MouseEvent<HTMLUListElement>) => {
      // Find which file row was right-clicked
      const target = e.target as HTMLElement;
      const li = target.closest("li[data-name]") as HTMLElement | null;
      if (!li) {
        // Right-clicked on empty space — show the pane-level context menu
        setContextMenuOnFile(false);
        setContextMenuIsParentDir(false);
        return;
      }

      const fileName = li.dataset.name!;
      contextMenuFileRef.current = fileName;
      setContextMenuOnFile(true);
      setContextMenuIsParentDir(fileName === "..");

      // If right-clicked file is not in the selection, focus it (clearing selection)
      if (fileName !== ".." && !selectedLookupRef.current.has(fileName)) {
        safeCommandSilent("focus", { paneHandle, filename: fileName });
      }
    },
    [paneHandle],
  );

  const lastViewportReportRef = useRef<[number, number, number]>([-1, -1, -1]);
  // Send initial viewport report (and re-send on navigation).
  useEffect(() => {
    lastViewportReportRef.current = [-1, -1, -1];
    const container = containerRef.current;
    if (container) {
      const firstVisible = Math.floor(container.scrollTop / ITEM_SIZE);
      const visibleCount = Math.ceil(container.clientHeight / ITEM_SIZE);
      lastViewportReportRef.current = [firstVisible, visibleCount, -1];
      safeCommandSilent("set_viewport", {
        paneHandle,
        firstVisible,
        visibleCount,
      });
    }
  }, [path, paneHandle]);

  const topSpacerStyle = useMemo(
    () => ({ height: file_window.offset * ITEM_SIZE, flexShrink: 0 }),
    [file_window.offset],
  );
  const bottomSpacerStyle = useMemo(
    () => ({
      height:
        (file_window.total_count -
          file_window.offset -
          file_window.items.length) *
        ITEM_SIZE,
      flexShrink: 0,
    }),
    [file_window.total_count, file_window.offset, file_window.items.length],
  );

  const onScroll: React.UIEventHandler<HTMLElement> = (e) => {
    const el = e.currentTarget;
    tableHeaderRef.current!.scrollLeft = el.scrollLeft;

    // Report viewport position to Rust for window sliding.
    const fw = fileWindowRef.current;
    const firstVisible = Math.floor(el.scrollTop / ITEM_SIZE);
    const visibleCount = Math.ceil(el.clientHeight / ITEM_SIZE);
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
    safeCommandSilent("set_viewport", {
      paneHandle,
      firstVisible,
      visibleCount,
    });
  };

  const widthPrefix = `pane-${paneHandle}-column-`;

  return (
    <div
      className={`${styles.pane} ${showSpinner ? styles.paneBusy : ""}`}
      data-pane-handle={paneHandle}
      onClick={() => command("focus")}
      onMouseDown={(e) => {
        if (e.button === 3) {
          e.preventDefault();
          command("cmd_navigate_back");
        } else if (e.button === 4) {
          e.preventDefault();
          command("cmd_navigate_forward");
        }
      }}
    >
      <FilterInput
        value={filter}
        filterMode={filter_mode}
        inputRef={inputRef}
        onKeyDown={onkeydownFilter}
        onChange={(value) => command("set_filter", { filter: value })}
        onFocus={() => {
          if (filter != null) {
            command("set_filter", { filter: filter });
          }
        }}
      />
      <div className={styles.header}>
        <VfsSelector
          vfsDisplayName={vfs_display_name}
          vfsTargets={vfsTargets}
          paneHandle={paneHandle}
          activeVfsId={path.vfs_id}
          open={isVfsSelectorOpen}
        />
        <div className={styles.headerPath}>
          <PathBreadcrumbs
            breadcrumbs={breadcrumbs}
            paneHandle={paneHandle}
            displayPath={props.display_path}
          />
        </div>
        {fs_stats?.available_bytes !== undefined && (
          <div>{getSiPrefixedNumber(fs_stats.available_bytes)}B free</div>
        )}
      </div>
      <div className={styles.tableHeader} ref={tableHeaderRef}>
        <div className={styles.tableHeaderInner}>
          {columns.map((column) => (
            <ColumnHeader
              key={column.key}
              widthPrefix={widthPrefix}
              sorting={sorting}
              column={column}
              onSort={(key, asc) => {
                command("set_sorting", {
                  sorting: { key, asc },
                });
              }}
            />
          ))}
        </div>
      </div>
      {file_window && (
        <ContextMenu.Root>
          <ContextMenu.Trigger asChild>
            <ul
              className={styles.files}
              ref={containerRef}
              onKeyDown={onkeydown}
              onMouseDown={onMouseDown}
              onClick={(e) => e.stopPropagation()}
              onContextMenu={onContextMenu}
              tabIndex={-1}
              onScroll={onScroll}
            >
              <div style={topSpacerStyle} />
              {file_window.items.map((row) => {
                const isFocused = active && row.name === focused;
                return (
                  <FileRow
                    key={row.name}
                    row={row}
                    isFocused={isFocused}
                    isSelected={selectedLookup.has(row.name)}
                    filter={isFocused ? filter : null}
                    filterMode={filter_mode}
                    widthPrefix={widthPrefix}
                    onClick={onClick}
                    onMouseDown={onDndMouseDown}
                    onOpen={onOpen}
                  />
                );
              })}
              <div style={bottomSpacerStyle} />
              <div className={styles.dragRect} ref={dragRectRef} />
            </ul>
          </ContextMenu.Trigger>
          {contextMenuOnFile ? (
            <FileContextMenuContent
              paneHandle={paneHandle}
              isParentDir={contextMenuIsParentDir}
            />
          ) : (
            <PaneContextMenuContent
              paneHandle={paneHandle}
              isHostLocal={props.is_host_local}
            />
          )}
        </ContextMenu.Root>
      )}
      <div className="dnd-ghost" ref={dndGhostRef} />
      <div className={styles.statusbar}>
        {showSpinner && "Loading file list..."}
        {!showSpinner && loading && (
          <>
            Loading... ({(stats.file_count + stats.dir_count).toLocaleString()}{" "}
            items so far)
          </>
        )}
        {!showSpinner &&
          !loading &&
          stats.selected_file_count + stats.selected_dir_count > 0 && (
            <>
              {stats.selected_file_count} files, {stats.selected_dir_count}{" "}
              directories selected, {stats.selected_bytes.toLocaleString()}{" "}
              bytes total
              {stats.total_count != null &&
                ` (showing ${stats.file_count + stats.dir_count} of ${stats.total_count})`}
            </>
          )}
        {!showSpinner &&
          !loading &&
          stats.selected_file_count + stats.selected_dir_count === 0 && (
            <>
              {stats.file_count} files, {stats.dir_count} directories
              {stats.total_count != null &&
                ` (showing ${stats.file_count + stats.dir_count} of ${stats.total_count})`}
            </>
          )}
        {!showSpinner && !loading && partial && (
          <span className={styles.partial}> (partial)</span>
        )}
      </div>
    </div>
  );
}

export default memo(PaneInner);
