import {
  useState,
  useEffect,
  useRef,
  useMemo,
  useLayoutEffect,
  useCallback,
  Fragment,
  memo,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import * as ContextMenu from "@radix-ui/react-context-menu";
import iconMapping from "../assets/mapping.json";
import { ViewportList, ViewportListRef } from "../lib/viewPortList";
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
import { FileContextMenuContent } from "./ContextMenu";
import styles from "./Pane.module.scss";
import menuStyles from "./Menu.module.scss";
import columnStyles from "./Columns.module.scss";

function PathBreadcrumbs(props: {
  breadcrumbs: Breadcrumb[];
  paneHandle: number;
}) {
  const { breadcrumbs, paneHandle } = props;

  return (
    <>
      {breadcrumbs.map((crumb, i) => (
        <Fragment key={i}>
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
        </Fragment>
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
  baseSelection: Set<string>;
  lastSentStartIdx: number;
  lastSentEndIdx: number;
  lastClientY: number;
  lastCurScrollX: number;
  scrollIntervalId: number | null;
};

function computeDragSelection(
  startIdx: number,
  endIdx: number,
  files: File[],
  baseSelection: Set<string> | null,
): string[] {
  const lo = Math.min(startIdx, endIdx);
  const hi = Math.max(startIdx, endIdx);
  const range = new Set<string>();
  for (let i = lo; i <= hi; i++) {
    const name = files[i].name;
    if (name !== "..") range.add(name);
  }

  if (baseSelection) {
    for (const name of baseSelection) {
      range.add(name);
    }
  }

  return [...range];
}

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
  filter?: string;
  filterMode: FilterMode;
  widthPrefix: string;
  onClick: React.MouseEventHandler<HTMLLIElement>;
  onMouseDown: React.MouseEventHandler<HTMLLIElement>;
  onOpen: (file: File) => void;
};

const FileRow = memo(function FileRow({
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
});

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
  onRestoreFocus,
}: {
  vfsDisplayName: string;
  vfsTargets: VfsTarget[];
  paneHandle: number;
  activeVfsId: number;
  open: boolean;
  onRestoreFocus: () => void;
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
            e.preventDefault();
            if (!openingDialogRef.current) {
              onRestoreFocus();
            }
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

function FilterBar({
  initialValue,
  inputRef,
  onKeyDown,
  onChange,
}: {
  initialValue: string;
  inputRef: React.RefObject<HTMLInputElement | null>;
  onKeyDown: React.KeyboardEventHandler;
  onChange: (value: string) => void;
}) {
  const [localValue, setLocalValue] = useState(initialValue);

  return (
    <div className={styles.filterBar}>
      <input
        className={styles.filterBarInput}
        type="text"
        value={localValue}
        placeholder="Filter (regex)"
        onChange={(e) => {
          setLocalValue(e.target.value);
          onChange(e.target.value);
        }}
        ref={inputRef}
        onKeyDown={onKeyDown}
        autoFocus
        tabIndex={-1}
      />
    </div>
  );
}

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
    files,
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
  const viewPortRef = useRef<ViewportListRef>(null);
  const tableHeaderRef = useRef<HTMLDivElement>(null);

  const dragRef = useRef<DragState | null>(null);
  const dragRectRef = useRef<HTMLDivElement>(null);
  const suppressClickRef = useRef(false);
  const filesRef = useRef(files);
  filesRef.current = files;
  const selectedLookupRef = useRef(selectedLookup);
  selectedLookupRef.current = selectedLookup;
  const pendingPathRef = useRef(pending_path);
  pendingPathRef.current = pending_path;

  // --- DnD (drag-and-drop between panes) refs ---
  const dndRef = useRef<LocalDndState | null>(null);
  const dndGhostRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (active && files && viewPortRef.current) {
      const containerHeight = containerRef.current!.offsetHeight;
      const pos = viewPortRef.current.getScrollPosition();
      if (
        focusedIndex < pos.index ||
        (focusedIndex == pos.index && pos.offset > 0)
      ) {
        viewPortRef.current.scrollToIndex({
          index: focusedIndex,
          delay: 0,
          alignToTop: true,
          prerender: Math.ceil(containerHeight / 22),
        });
      } else if (focusedIndex >= pos.index + Math.floor(containerHeight / 22)) {
        viewPortRef.current.scrollToIndex({
          index: focusedIndex,
          delay: 0,
          alignToTop: false,
          prerender: Math.ceil(containerHeight / 22),
        });
      }
    }
  }, [active, files, focusedIndex]);

  useEffect(() => {
    if (active && !modalOpen) {
      if (!isVfsSelectorOpen) {
        if (filter == null && filter_mode !== "filter") {
          containerRef.current?.focus();
        } else {
          inputRef.current?.focus();
        }
      }
    } else if (!active) {
      inputRef.current?.blur();
      containerRef.current?.blur();
    }
  }, [active, path, filter, filter_mode, modalOpen, isVfsSelectorOpen]);

  // --- Drag-to-select logic ---

  const getFileIndexAtY = useCallback((clientY: number): number => {
    const container = containerRef.current;
    if (!container) return 0;
    const rect = container.getBoundingClientRect();
    const index = Math.floor((clientY - rect.top + container.scrollTop) / 22);
    return Math.max(0, Math.min(filesRef.current.length - 1, index));
  }, []);

  const sendDragSelection = useCallback(
    (drag: DragState, startIdx: number, endIdx: number) => {
      const currentFiles = filesRef.current;
      if (!currentFiles.length) return;
      const base = drag.mode === "ctrl" ? drag.baseSelection : null;
      const sel = computeDragSelection(startIdx, endIdx, currentFiles, base);
      safeCommandSilent("set_selection", {
        paneHandle,
        selected: sel,
        focused: null,
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
      const currentFiles = filesRef.current;
      if (!currentFiles.length) return;

      const rectTop = Math.min(drag.startScrollY, curScrollY);
      const rectBottom = Math.max(drag.startScrollY, curScrollY);
      const startIdx = Math.max(0, Math.floor(rectTop / 22));
      const endIdx = Math.min(
        currentFiles.length - 1,
        Math.ceil(rectBottom / 22) - 1,
      );

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
        if (drag.active) suppressClickRef.current = true;
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
      if (drag.active) suppressClickRef.current = true;
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
    getFileIndexAtY,
    sendDragSelection,
    updateAutoScroll,
    updateDragRect,
    updateDragSelection,
    hideDragRect,
  ]);

  // Cancel drag when files change (e.g. directory navigation)
  useEffect(() => {
    const drag = dragRef.current;
    if (drag) {
      if (drag.scrollIntervalId !== null) clearInterval(drag.scrollIntervalId);
      hideDragRect();
      dragRef.current = null;
    }
  }, [files]);

  const onMouseDown = useCallback(
    (e: React.MouseEvent<HTMLUListElement>) => {
      if (e.button !== 0) return;
      // Only start drag from empty space — not on file icon or filename text
      const target = e.target as HTMLElement;
      if (target.closest(".file-icon") || target.closest(".filename-part"))
        return;
      e.preventDefault(); // block text selection

      const container = containerRef.current;
      if (!container) return;
      const currentFiles = filesRef.current;
      if (!currentFiles.length) return;

      // Focus the pane and the file under cursor (if any)
      const rect = container.getBoundingClientRect();
      const clickScrollY = e.clientY - rect.top + container.scrollTop;
      const clickIdx = Math.floor(clickScrollY / 22);
      if (clickIdx >= 0 && clickIdx < currentFiles.length) {
        safeCommandSilent("focus", {
          paneHandle,
          filename: currentFiles[clickIdx].name,
        });
      } else if (!active) {
        safeCommandSilent("focus", { paneHandle });
      }

      const startScrollX = e.clientX - rect.left + container.scrollLeft;
      let startScrollY = e.clientY - rect.top + container.scrollTop;

      let mode: DragMode = "normal";
      let baseSelection = new Set<string>();

      if (e.ctrlKey) {
        mode = "ctrl";
        baseSelection = new Set(selected);
      } else if (e.shiftKey) {
        mode = "shift";
        // Start rect from focused file's top edge
        const fi = currentFiles.findIndex((f) => f.name === focused);
        if (fi >= 0) startScrollY = fi * 22;
      }

      dragRef.current = {
        active: false,
        startX: e.clientX,
        startY: e.clientY,
        startScrollX,
        startScrollY,
        mode,
        baseSelection,
        lastSentStartIdx: -1,
        lastSentEndIdx: -1,
        lastClientY: e.clientY,
        lastCurScrollX: startScrollX,
        scrollIntervalId: null,
      };
    },
    [active, paneHandle, selected, focused],
  );

  // --- End drag-to-select logic ---

  // --- DnD (drag-and-drop between panes) logic ---

  const onDndMouseDown = useCallback((e: React.MouseEvent<HTMLLIElement>) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement;
    if (!target.closest(".file-icon") && !target.closest(".filename-part"))
      return;
    const fileName = e.currentTarget.dataset.name;
    if (!fileName || fileName === "..") return;

    e.preventDefault();
    e.stopPropagation(); // prevent drag-to-select

    const currentFiles = filesRef.current;
    const currentSelected = selectedLookupRef.current;
    const filesToDrag = currentSelected.has(fileName)
      ? currentFiles.filter(
          (f) => currentSelected.has(f.name) && f.name !== "..",
        )
      : [currentFiles.find((f) => f.name === fileName)!];

    dndRef.current = {
      active: false,
      startX: e.clientX,
      startY: e.clientY,
      files: filesToDrag.map((f) => ({ name: f.name, is_dir: f.is_dir })),
    };
  }, []);

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

  // Cancel DnD when files change
  useEffect(() => {
    const dnd = dndRef.current;
    if (dnd?.active) {
      cleanupDnd();
      safeCommandSilent("cancel_dnd");
    }
    dndRef.current = null;
  }, [files]);

  // --- End DnD logic ---

  const onOpen = useCallback(
    (file: File) => {
      if (!file || pendingPathRef.current) return;
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
      onOpen(files[focusedIndex]);
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
      if (filter && filter.length > 0) {
        command("set_filter", {
          filter: focused!.substring(0, filter.length - 1),
        });
      }
    } else if (
      filter_mode === "quick_search" &&
      e.key == "ArrowRight" &&
      noModifiers
    ) {
      if (filter && focused && filter.length < focused.length) {
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
      if (pendingPathRef.current) return;
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
      } else {
        safeCommand("focus", {
          paneHandle,
          filename: e.currentTarget.dataset.name,
        });
      }
    },
    [paneHandle],
  );

  const contextMenuFileRef = useRef<string | null>(null);
  const [contextMenuIsParentDir, setContextMenuIsParentDir] = useState(false);

  const onContextMenu = useCallback(
    (e: React.MouseEvent<HTMLUListElement>) => {
      // Find which file row was right-clicked
      const target = e.target as HTMLElement;
      const li = target.closest("li[data-name]") as HTMLElement | null;
      if (!li) {
        e.preventDefault();
        return;
      }

      const fileName = li.dataset.name!;
      contextMenuFileRef.current = fileName;
      setContextMenuIsParentDir(fileName === "..");

      // If right-clicked file is not in the selection, focus it (clearing selection)
      if (fileName !== ".." && !selectedLookupRef.current.has(fileName)) {
        safeCommandSilent("focus", { paneHandle, filename: fileName });
      }
    },
    [paneHandle],
  );

  const onScroll: React.UIEventHandler<HTMLElement> = (e) => {
    tableHeaderRef.current!.scrollLeft = e.currentTarget.scrollLeft;
  };

  const widthPrefix = `pane-${paneHandle}-column-`;

  return (
    <div
      className={`${styles.pane} ${showSpinner ? styles.paneBusy : ""}`}
      data-pane-handle={paneHandle}
      onClick={() => command("focus")}
    >
      {filter_mode !== "filter" && (
        <input
          className={styles.filterInput}
          type="text"
          value={filter || ""}
          onChange={(e) => command("set_filter", { filter: e.target.value })}
          ref={inputRef}
          onKeyDown={onkeydownFilter}
          onFocus={() => command("set_filter", { filter: filter || "" })}
          tabIndex={-1}
        />
      )}
      <div className={styles.header}>
        <VfsSelector
          vfsDisplayName={vfs_display_name}
          vfsTargets={vfsTargets}
          paneHandle={paneHandle}
          activeVfsId={path.vfs_id}
          open={isVfsSelectorOpen}
          onRestoreFocus={() => containerRef.current?.focus()}
        />
        <div className={styles.headerPath}>
          <PathBreadcrumbs breadcrumbs={breadcrumbs} paneHandle={paneHandle} />
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
      {files && (
        <ContextMenu.Root>
          <ContextMenu.Trigger asChild>
            <ul
              className={styles.files}
              ref={containerRef}
              onKeyDown={onkeydown}
              onMouseDown={onMouseDown}
              onContextMenu={onContextMenu}
              tabIndex={-1}
              onScroll={onScroll}
            >
              <ViewportList
                overscan={0}
                initialIndex={focusedIndex}
                ref={viewPortRef}
                viewportRef={containerRef}
                items={files}
                itemSize={22}
              >
                {(row: File) => {
                  const isFocused = active && row.name === focused;
                  return (
                    <FileRow
                      key={row.name}
                      row={row}
                      isFocused={isFocused}
                      isSelected={selectedLookup.has(row.name)}
                      filter={isFocused ? filter : undefined}
                      filterMode={filter_mode}
                      widthPrefix={widthPrefix}
                      onClick={onClick}
                      onMouseDown={onDndMouseDown}
                      onOpen={onOpen}
                    />
                  );
                }}
              </ViewportList>
              <div className={styles.dragRect} ref={dragRectRef} />
            </ul>
          </ContextMenu.Trigger>
          <FileContextMenuContent
            paneHandle={paneHandle}
            isParentDir={contextMenuIsParentDir}
          />
        </ContextMenu.Root>
      )}
      <div className="dnd-ghost" ref={dndGhostRef} />
      {filter_mode === "filter" && (
        <FilterBar
          initialValue={filter || ""}
          inputRef={inputRef}
          onKeyDown={onkeydownFilter}
          onChange={(value) => command("set_filter", { filter: value })}
        />
      )}
      <div className={styles.statusbar}>
        {showSpinner && "Loading file list..."}
        {!showSpinner && loading && (
          <>
            Loading... ({(stats.file_count + stats.dir_count).toLocaleString()}{" "}
            items so far)
          </>
        )}
        {!showSpinner && !loading && selected.length > 0 && (
          <>
            {stats.selected_file_count} files, {stats.selected_dir_count}{" "}
            directories selected, {stats.selected_bytes.toLocaleString()} bytes
            total
            {stats.total_count != null &&
              ` (showing ${stats.file_count + stats.dir_count} of ${stats.total_count})`}
          </>
        )}
        {!showSpinner && !loading && selected.length == 0 && (
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
