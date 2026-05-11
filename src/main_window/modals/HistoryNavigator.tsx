import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { commands, type HistoryEntryView } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { useSuppressInitialPointer } from "../../lib/useSuppressInitialPointer";
import menuStyles from "../Menu.module.scss";
import styles from "./HistoryNavigator.module.scss";

// Step from `from` by `dir` (+1 or -1) inside `entries`, skipping dead
// entries. Returns the original `from` if no live entry exists in that
// direction.
function stepHistoryIndex(
  entries: HistoryEntryView[],
  from: number,
  dir: -1 | 1,
): number {
  let i = from + dir;
  while (i >= 0 && i < entries.length) {
    if (entries[i].is_alive) return i;
    i += dir;
  }
  return from;
}

// Step `count` live entries in `dir`. Stops at the last reachable live entry
// in that direction if `count` exceeds what's available.
function stepHistoryIndexBy(
  entries: HistoryEntryView[],
  from: number,
  dir: -1 | 1,
  count: number,
): number {
  let i = from;
  for (let n = 0; n < count; n++) {
    const next = stepHistoryIndex(entries, i, dir);
    if (next === i) break;
    i = next;
  }
  return i;
}

// First / last live entry in the list. Returns `fallback` if there are none.
function firstLiveIndex(entries: HistoryEntryView[], fallback: number): number {
  for (let i = 0; i < entries.length; i++) if (entries[i].is_alive) return i;
  return fallback;
}
function lastLiveIndex(entries: HistoryEntryView[], fallback: number): number {
  for (let i = entries.length - 1; i >= 0; i--)
    if (entries[i].is_alive) return i;
  return fallback;
}

// Quantize a past timestamp into a coarse bucket label, computed relative to
// `now`. Logarithmic cutoffs — fine near now, coarse far away.
function historyBucketLabel(timestampMs: number, nowMs: number): string {
  const ageSec = Math.max(0, Math.floor((nowMs - timestampMs) / 1000));
  if (ageSec < 60) return "just now";
  const ageMin = Math.floor(ageSec / 60);
  if (ageMin < 3) return "a minute ago";
  if (ageMin < 8) return "5 minutes ago";
  if (ageMin < 23) return "15 minutes ago";
  if (ageMin < 45) return "30 minutes ago";
  const ageHour = Math.floor(ageMin / 60);
  if (ageHour < 2) return "1 hour ago";
  if (ageHour < 4) return "2 hours ago";
  if (ageHour < 9) return "6 hours ago";

  const now = new Date(nowMs);
  const then = new Date(timestampMs);
  const startOf = (d: Date) =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const dayDiff = Math.round((startOf(now) - startOf(then)) / 86_400_000);
  if (dayDiff <= 0) return "earlier today";
  if (dayDiff === 1) return "yesterday";
  if (dayDiff < 7) {
    const weekdays = [
      "Sunday",
      "Monday",
      "Tuesday",
      "Wednesday",
      "Thursday",
      "Friday",
      "Saturday",
    ];
    return weekdays[then.getDay()];
  }
  if (dayDiff < 14) return "last week";
  if (dayDiff < 30) return `${Math.floor(dayDiff / 7)} weeks ago`;
  if (dayDiff < 60) return "last month";
  return "older";
}

export default function HistoryNavigator({
  entries,
  currentIndex,
  initialDirection,
  paneHandle,
  persistent,
  open,
}: {
  entries: HistoryEntryView[];
  currentIndex: number;
  initialDirection: -1 | 1;
  paneHandle: number;
  persistent: boolean;
  open: boolean;
}) {
  // Initial preview: step once in the requested direction, skipping dead
  // entries. If nothing live in that direction, stay on current. Re-seeded
  // every open so reopening picks up the latest entries / direction.
  const [previewIndex, setPreviewIndex] = useState(currentIndex);
  useEffect(() => {
    if (open) {
      setPreviewIndex(
        stepHistoryIndex(entries, currentIndex, initialDirection),
      );
    }
    // entries identity changes per modal-data update (e.g. after a delete);
    // we intentionally only re-seed when `open` flips true.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Freeze "now" at open time so bucket labels don't tick while the dialog
  // is open.
  const [openedAt, setOpenedAt] = useState(() => Date.now());
  useEffect(() => {
    if (open) setOpenedAt(Date.now());
  }, [open]);
  const bucketLabels = useMemo(
    () => entries.map((e) => historyBucketLabel(e.arrived_at, openedAt)),
    [entries, openedAt],
  );

  const commit = useCallback(
    (target: number) => {
      if (target === currentIndex) {
        safe(commands.closeModal());
      } else {
        safe(commands.navigateHistory(paneHandle, target));
      }
    },
    [currentIndex, paneHandle],
  );

  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const el = listRef.current?.querySelector<HTMLElement>(
      `[data-history-index="${previewIndex}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [previewIndex, open]);

  // Ignore hover until the user actually moves the mouse — prevents
  // whichever entry happens to sit under the cursor at open time from
  // hijacking the keyboard's preview selection.
  const pointerActive = useSuppressInitialPointer();

  // Window-level Alt-up commit for the alt-tab style mode. Radix's own
  // keyboard handling on Content covers ArrowUp/ArrowDown/Enter/Esc, but
  // it doesn't know about Alt-up.
  useEffect(() => {
    if (!open || persistent) return;
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === "Alt") {
        e.preventDefault();
        setPreviewIndex((i) => {
          commit(i);
          return i;
        });
      }
    };
    window.addEventListener("keyup", onKeyUp, true);
    return () => window.removeEventListener("keyup", onKeyUp, true);
  }, [open, persistent, commit]);

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
            // Pane focus effect handles restoring focus to the active pane.
            e.preventDefault();
          }}
          // Alt+ArrowUp/ArrowDown also feed the preview, in addition to
          // the bare arrow keys Radix already handles via its menu key
          // navigation. We intercept here so we can drive `previewIndex`
          // ourselves (and skip dead entries).
          onKeyDown={(e) => {
            if ((e.key === "ArrowLeft" && e.altKey) || e.key === "ArrowDown") {
              e.preventDefault();
              setPreviewIndex((i) => stepHistoryIndex(entries, i, 1));
            } else if (
              (e.key === "ArrowRight" && e.altKey) ||
              e.key === "ArrowUp"
            ) {
              e.preventDefault();
              setPreviewIndex((i) => stepHistoryIndex(entries, i, -1));
            } else if (e.key === "PageDown") {
              e.preventDefault();
              setPreviewIndex((i) => stepHistoryIndexBy(entries, i, 1, 10));
            } else if (e.key === "PageUp") {
              e.preventDefault();
              setPreviewIndex((i) => stepHistoryIndexBy(entries, i, -1, 10));
            } else if (e.key === "Home") {
              e.preventDefault();
              setPreviewIndex((i) => firstLiveIndex(entries, i));
            } else if (e.key === "End") {
              e.preventDefault();
              setPreviewIndex((i) => lastLiveIndex(entries, i));
            } else if (e.key === "Enter") {
              e.preventDefault();
              commit(previewIndex);
            }
          }}
        >
          <div className={styles.header}>History</div>
          <div
            ref={listRef}
            className={styles.list}
            role="listbox"
            aria-label="History"
            style={pointerActive ? undefined : { pointerEvents: "none" }}
          >
            {entries.map((entry, i) => {
              const isCurrent = i === currentIndex;
              const isPreviewed = i === previewIndex;
              const bucket = bucketLabels[i];
              const isBucketBoundary =
                i === 0 || bucketLabels[i - 1] !== bucket;
              return (
                <Fragment key={i}>
                  {isBucketBoundary && (
                    <div className={styles.bucket}>{bucket}</div>
                  )}
                  <div
                    data-history-index={i}
                    className={`${menuStyles.item} ${styles.item} ${
                      isCurrent ? styles.itemCurrent : ""
                    } ${isPreviewed ? styles.itemPreviewed : ""}`}
                    data-disabled={entry.is_alive ? undefined : ""}
                    role="option"
                    aria-selected={isPreviewed}
                    onMouseEnter={() => {
                      if (entry.is_alive) setPreviewIndex(i);
                    }}
                    onClick={(e) => {
                      e.stopPropagation();
                      if (!entry.is_alive) return;
                      commit(i);
                    }}
                  >
                    <span className={styles.path}>{entry.display_path}</span>
                    {isCurrent && (
                      <span className={styles.currentTag}>current</span>
                    )}
                    {!entry.is_alive && (
                      <span className={styles.deadTag}>unmounted</span>
                    )}
                    {persistent && !isCurrent && (
                      <button
                        type="button"
                        className={styles.deleteButton}
                        aria-label="Remove entry from history"
                        title="Remove from history"
                        onMouseDown={(e) => {
                          e.stopPropagation();
                        }}
                        onClick={(e) => {
                          e.stopPropagation();
                          safe(commands.deleteHistoryEntry(paneHandle, i));
                        }}
                      >
                        ×
                      </button>
                    )}
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
