import { Fragment, useEffect, useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { commands, type Sorting, type SortingKey } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { useSuppressInitialPointer } from "../../lib/useSuppressInitialPointer";
import menuStyles from "../Menu.module.scss";
import styles from "./SortMenu.module.scss";

type Row =
  | { kind: "sort"; key: SortingKey; label: string; accel: string }
  | { kind: "reverse"; label: string; accel: string }
  | { kind: "folders"; label: string; accel: string };

// Accel letters are unique across every row (mode → "o" since "m" is taken by
// modified); the underlined letter in each label is the accelerator.
const SORT_ROWS: Row[] = [
  { kind: "sort", key: "name", label: "Name", accel: "n" },
  { kind: "sort", key: "extension", label: "Extension", accel: "e" },
  { kind: "sort", key: "size", label: "Size", accel: "s" },
  { kind: "sort", key: "modified", label: "Modified", accel: "m" },
  { kind: "sort", key: "accessed", label: "Accessed", accel: "a" },
  { kind: "sort", key: "created", label: "Created", accel: "c" },
  { kind: "sort", key: "user", label: "User", accel: "u" },
  { kind: "sort", key: "group", label: "Group", accel: "g" },
  { kind: "sort", key: "mode", label: "Mode", accel: "o" },
];
const ACTION_ROWS: Row[] = [
  { kind: "reverse", label: "Reverse direction", accel: "r" },
  { kind: "folders", label: "Folders first", accel: "f" },
];
const ROWS: Row[] = [...SORT_ROWS, ...ACTION_ROWS];

// Underline the accelerator letter at its position in the label.
function LabelWithAccel({ label, accel }: { label: string; accel: string }) {
  const i = label.toLowerCase().indexOf(accel);
  if (i < 0) return <>{label}</>;
  return (
    <>
      {label.slice(0, i)}
      <span className={styles.accel}>{label[i]}</span>
      {label.slice(i + 1)}
    </>
  );
}

export default function SortMenu({
  sorting,
  foldersFirst,
  paneHandle,
  open,
}: {
  sorting: Sorting;
  foldersFirst: boolean;
  paneHandle: number;
  open: boolean;
}) {
  // Seed the highlight on the current sort key so Enter reverses it.
  const currentIndex = SORT_ROWS.findIndex(
    (r) => r.kind === "sort" && r.key === sorting.key,
  );
  const [highlight, setHighlight] = useState(
    currentIndex < 0 ? 0 : currentIndex,
  );
  useEffect(() => {
    if (open) setHighlight(currentIndex < 0 ? 0 : currentIndex);
    // Re-seed only when the menu opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const pointerActive = useSuppressInitialPointer();

  // Holding Shift arms "reverse": Shift+<key> applies that sort descending,
  // and the Reverse row lights up while held. Tracked via window listeners so
  // release is caught wherever focus sits inside the menu.
  const [shiftHeld, setShiftHeld] = useState(false);
  useEffect(() => {
    if (!open) {
      setShiftHeld(false);
      return;
    }
    const sync = (e: KeyboardEvent) => setShiftHeld(e.shiftKey);
    window.addEventListener("keydown", sync, true);
    window.addEventListener("keyup", sync, true);
    return () => {
      window.removeEventListener("keydown", sync, true);
      window.removeEventListener("keyup", sync, true);
    };
  }, [open]);

  // Every pick commits and closes — the menu is a quick launcher, so a second
  // ⌘⇧S,<key> is how you flip direction (same-key toggle, like a header click).
  // `reversed` (Shift held) instead forces the sort descending outright.
  const apply = (row: Row, reversed: boolean) => {
    if (row.kind === "sort") {
      const asc = reversed
        ? false
        : row.key === sorting.key
          ? !sorting.asc
          : true;
      safe(commands.setSorting(paneHandle, { key: row.key, asc }));
    } else if (row.kind === "reverse") {
      safe(
        commands.setSorting(paneHandle, {
          key: sorting.key,
          asc: !sorting.asc,
        }),
      );
    } else {
      safe(
        commands.updatePreference("appearance.folders_first", !foldersFirst),
      );
    }
    safe(commands.closeModal());
  };

  return (
    <DropdownMenu.Root
      open={open}
      onOpenChange={(v) => {
        if (!v) safe(commands.closeModal());
      }}
    >
      <DropdownMenu.Trigger asChild>
        <span className={styles.anchor} aria-hidden tabIndex={-1} />
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={`${menuStyles.content} ${styles.content}`}
          align="start"
          sideOffset={4}
          onCloseAutoFocus={(e) => {
            // Pane focus effect restores focus to the active pane.
            e.preventDefault();
          }}
          onKeyDown={(e) => {
            if (e.metaKey || e.ctrlKey || e.altKey) return;
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setHighlight((i) => Math.min(i + 1, ROWS.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setHighlight((i) => Math.max(i - 1, 0));
            } else if (e.key === "Home") {
              e.preventDefault();
              setHighlight(0);
            } else if (e.key === "End") {
              e.preventDefault();
              setHighlight(ROWS.length - 1);
            } else if (e.key === "Enter") {
              e.preventDefault();
              apply(ROWS[highlight], e.shiftKey);
            } else {
              // `e.code` for digits: Shift+3 reports "#" in `e.key`, but
              // "Digit3" in `e.code` regardless of the modifier / layout.
              const digit = /^Digit([1-9])$/.exec(e.code);
              if (digit) {
                e.preventDefault();
                apply(SORT_ROWS[Number(digit[1]) - 1], e.shiftKey);
                return;
              }
              const row = ROWS.find((r) => r.accel === e.key.toLowerCase());
              if (row) {
                e.preventDefault();
                apply(row, e.shiftKey);
              }
            }
          }}
        >
          <div className={styles.header}>Sort by</div>
          <div
            className={styles.list}
            role="listbox"
            aria-label="Sort by"
            style={pointerActive ? undefined : { pointerEvents: "none" }}
          >
            {ROWS.map((row, i) => {
              const isCurrent = row.kind === "sort" && row.key === sorting.key;
              const isChecked = row.kind === "folders" && foldersFirst;
              return (
                <Fragment key={row.kind === "sort" ? row.key : row.kind}>
                  {row.kind === "reverse" && (
                    <div className={menuStyles.separator} />
                  )}
                  <div
                    className={`${menuStyles.item} ${styles.item} ${
                      isCurrent ? styles.itemCurrent : ""
                    } ${i === highlight ? styles.itemHighlighted : ""} ${
                      row.kind === "reverse" && shiftHeld
                        ? styles.itemReverseArmed
                        : ""
                    }`}
                    role="option"
                    aria-selected={i === highlight}
                    onMouseEnter={() => setHighlight(i)}
                    onClick={(e) => {
                      e.stopPropagation();
                      apply(row, e.shiftKey);
                    }}
                  >
                    <span className={styles.label}>
                      <LabelWithAccel label={row.label} accel={row.accel} />
                    </span>
                    {row.kind === "reverse" && (
                      <kbd className={styles.kbd}>Shift</kbd>
                    )}
                    {isCurrent && (
                      <span className={styles.arrow}>
                        {sorting.asc ? "▲" : "▼"}
                      </span>
                    )}
                    {isChecked && <span className={styles.arrow}>✓</span>}
                  </div>
                </Fragment>
              );
            })}
          </div>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
