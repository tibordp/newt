import { useState, useEffect, useRef } from "react";
import iconMapping from "../assets/mapping.json";
import {
  FileView,
  ColumnDef,
  Sorting,
  gitStatus,
  recursiveSize,
} from "./types";
import { modeString } from "./utils";
import { formatDate, formatDateTime, formatTime } from "../lib/datetime";
import type { MetadataTraits } from "../lib/bindings";
import styles from "./Columns.module.scss";

const fileNames = iconMapping.light.fileNames as Record<string, string>;
const fileExtensions = iconMapping.light.fileExtensions as Record<
  string,
  string
>;
const iconDefinitions = iconMapping.iconDefinitions as unknown as Record<
  string,
  { fontCharacter: string; fontColor: string }
>;

function fileStem(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.substring(0, dot) : name;
}

// Windows FILE_ATTRIBUTE_* bits rendered in the Attr column, in display
// order: Readonly, Hidden, System, Archive, reparse point (Link),
// Compressed, Encrypted.
const ATTRIBUTE_FLAGS: [number, string][] = [
  [0x1, "R"],
  [0x2, "H"],
  [0x4, "S"],
  [0x20, "A"],
  [0x400, "L"],
  [0x800, "C"],
  [0x4000, "E"],
];

function attributesString(attributes: number): string {
  return ATTRIBUTE_FLAGS.filter(([bit]) => attributes & bit)
    .map(([, letter]) => letter)
    .join("");
}

function FileName({
  focused,
  filter,
  filterMode,
  info,
  displayName,
}: {
  focused: boolean;
  filter: string | null;
  filterMode: string;
  info: FileView;
  displayName: string;
}) {
  const { name, is_dir, is_symlink, is_hidden } = info;
  const git = gitStatus(info);

  const icon =
    fileNames[name] ||
    fileExtensions[name.substr(name.indexOf(".") + 1)] ||
    iconMapping.light.file;

  const { fontCharacter, fontColor } = iconDefinitions[icon];
  const ch = String.fromCodePoint(parseInt(fontCharacter, 16));

  const nameElement = (
    <>
      {(!focused || filter == null || filterMode === "filter") && (
        <>{displayName}</>
      )}
      {focused && filter != null && filterMode !== "filter" && (
        <>
          <span className={styles.filterHead}>
            {displayName.substr(0, filter.length)}
          </span>
          <span>{displayName.substr(filter.length)}</span>
        </>
      )}
    </>
  );

  const iconElement = is_dir ? (
    <div className="file-icon folder" />
  ) : (
    <div className="file-icon" style={{ color: fontColor }}>
      {ch}
    </div>
  );

  // `source_display` (pre-rendered by the host for search results) is shown
  // inline as a "where from" hint so identically-named matches stay distinct.
  return (
    <div
      className={`${styles.filename} ${is_hidden ? "hidden-file" : ""} ${
        is_symlink ? "symlink" : ""
      } ${git ? `git-${git}` : ""}`}
    >
      {iconElement}
      <div className={focused ? "filename-part focused" : "filename-part"}>
        {nameElement}
        {info.source_display && (
          <span className={styles.sourceHint}> ({info.source_display})</span>
        )}
      </div>
    </div>
  );
}

export const allColumns: ColumnDef[] = [
  {
    align: "left",
    key: "name",
    subcolumns: [
      {
        sortKey: "name",
        name: "Name",
        style: {
          flexBasis: "60px",
        },
      },
      {
        sortKey: "extension",
        name: "Ext",
      },
    ],
    render: (info, { isFocused, filter, filterMode }) => (
      <FileName
        filter={filter}
        filterMode={filterMode}
        focused={isFocused}
        info={info}
        displayName={info.name}
      />
    ),
    initialWidth: 250,
  },
  {
    align: "left",
    key: "stem",
    subcolumns: [
      {
        sortKey: "name",
        name: "Name",
      },
    ],
    render: (info, { isFocused, filter, filterMode }) => (
      <FileName
        filter={filter}
        filterMode={filterMode}
        focused={isFocused}
        info={info}
        displayName={info.is_dir ? info.name : fileStem(info.name)}
      />
    ),
    initialWidth: 200,
  },
  {
    align: "right",
    key: "size",
    initialWidth: 100,
    subcolumns: [
      {
        name: "Size",
        sortKey: "size",
      },
    ],
    render: (info) => {
      // Computed recursive size (du enricher) beats the entry's own
      // size; still-growing / cancelled values get a trailing "+".
      const du = recursiveSize(info);
      if (du != null) {
        return (
          <span className={du.complete ? undefined : styles.partialValue}>
            {du.bytes.toLocaleString()}
            {!du.complete && "+"}
          </span>
        );
      }
      return (
        <>
          {info.size != null
            ? info.size.toLocaleString()
            : info.is_dir
              ? "DIR"
              : "???"}
        </>
      );
    },
  },
  {
    align: "right",
    initialWidth: 145,
    key: "modified",
    subcolumns: [
      {
        name: "Modified",
        sortKey: "modified",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.modified != null
          ? formatDateTime(info.modified, ctx.dateFormat, ctx.timeFormat)
          : ""}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 80,
    key: "modified_date",
    subcolumns: [
      {
        name: "Date",
        sortKey: "modified",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.modified != null ? formatDate(info.modified, ctx.dateFormat) : ""}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 80,
    key: "modified_time",
    subcolumns: [
      {
        name: "Time",
        sortKey: "modified",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.modified != null ? formatTime(info.modified, ctx.timeFormat) : ""}
      </>
    ),
  },
  {
    align: "left",
    initialWidth: 70,
    key: "user",
    subcolumns: [
      {
        name: "User",
        sortKey: "user",
      },
    ],
    render: (info) => (
      <>{info.user && ("name" in info.user ? info.user.name : info.user.id)}</>
    ),
  },
  {
    align: "left",
    initialWidth: 70,
    key: "group",
    subcolumns: [
      {
        name: "Group",
        sortKey: "group",
      },
    ],
    render: (info) => (
      <>
        {info.group && ("name" in info.group ? info.group.name : info.group.id)}
      </>
    ),
  },
  {
    align: "left",
    initialWidth: 70,
    key: "mode",
    subcolumns: [
      {
        name: "Mode",
        sortKey: "mode",
      },
    ],
    render: (info) => <>{info.mode != null ? modeString(info.mode) : ""}</>,
  },
  {
    align: "right",
    initialWidth: 60,
    key: "attributes",
    subcolumns: [
      {
        name: "Attr",
        sortKey: "attributes",
      },
    ],
    render: (info) => (
      <>{info.attributes != null ? attributesString(info.attributes) : ""}</>
    ),
  },
  {
    align: "left",
    initialWidth: 60,
    key: "extension",
    subcolumns: [
      {
        name: "Ext",
        sortKey: "extension",
      },
    ],
    render: (info) => {
      if (info.is_dir) return <></>;
      const dot = info.name.lastIndexOf(".");
      return <>{dot > 0 ? info.name.substring(dot + 1) : ""}</>;
    },
  },
  {
    align: "right",
    initialWidth: 145,
    key: "accessed",
    subcolumns: [
      {
        name: "Accessed",
        sortKey: "accessed",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.accessed != null
          ? formatDateTime(info.accessed, ctx.dateFormat, ctx.timeFormat)
          : ""}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 80,
    key: "accessed_date",
    subcolumns: [
      {
        name: "Accessed",
        sortKey: "accessed",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.accessed != null ? formatDate(info.accessed, ctx.dateFormat) : ""}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 80,
    key: "accessed_time",
    subcolumns: [
      {
        name: "Acc. Time",
        sortKey: "accessed",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.accessed != null ? formatTime(info.accessed, ctx.timeFormat) : ""}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 145,
    key: "created",
    subcolumns: [
      {
        name: "Created",
        sortKey: "created",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.created != null
          ? formatDateTime(info.created, ctx.dateFormat, ctx.timeFormat)
          : ""}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 80,
    key: "created_date",
    subcolumns: [
      {
        name: "Created",
        sortKey: "created",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.created != null ? formatDate(info.created, ctx.dateFormat) : ""}
      </>
    ),
  },
  {
    align: "right",
    initialWidth: 80,
    key: "created_time",
    subcolumns: [
      {
        name: "Cr. Time",
        sortKey: "created",
      },
    ],
    render: (info, ctx) => (
      <>
        {info.created != null ? formatTime(info.created, ctx.timeFormat) : ""}
      </>
    ),
  },
  {
    align: "left",
    initialWidth: 150,
    key: "symlink_target",
    subcolumns: [
      {
        name: "Link Target",
      },
    ],
    render: (info) => <>{info.symlink_target ?? ""}</>,
  },
];

const columnsByKey = new Map(allColumns.map((c) => [c.key, c]));

/// Every user-selectable column with its display label, in canonical order.
/// "stem" is absent — it's the internal name-column swap, not a choice. The
/// compound timestamp columns ("modified" etc.) show date+time in one cell
/// and swap down to date-only when the paired time column is visible.
export const COLUMN_CHOICES = [
  { key: "name", label: "Name" },
  { key: "size", label: "Size" },
  { key: "extension", label: "Extension" },
  { key: "modified", label: "Modified" },
  { key: "modified_date", label: "Modified Date" },
  { key: "modified_time", label: "Modified Time" },
  { key: "accessed", label: "Accessed" },
  { key: "accessed_date", label: "Accessed Date" },
  { key: "accessed_time", label: "Accessed Time" },
  { key: "created", label: "Created" },
  { key: "created_date", label: "Created Date" },
  { key: "created_time", label: "Created Time" },
  { key: "user", label: "User" },
  { key: "group", label: "Group" },
  { key: "mode", label: "Mode" },
  { key: "attributes", label: "Attributes" },
  { key: "symlink_target", label: "Link Target" },
];

/// Columns that only exist where the pane's VFS populates their metadata
/// family (see `MetadataTraits`). Unlisted keys are trait-free.
export const TRAIT_GATES: Record<string, keyof MetadataTraits> = {
  user: "unix_owner",
  group: "unix_owner",
  mode: "unix_owner",
  attributes: "windows_attributes",
};

/// Drop config keys whose metadata family the pane's VFS doesn't
/// populate. No traits (settings previews, etc.) means no filtering.
function traitFiltered(
  columnKeys: string[],
  traits?: MetadataTraits,
): string[] {
  if (!traits) return columnKeys;
  return columnKeys.filter((k) => {
    const gate = TRAIT_GATES[k];
    return !gate || traits[gate];
  });
}

const canonicalOrder = COLUMN_CHOICES.map((c) => c.key);

/// Compound timestamp column → its date-only swap target (used when the
/// paired time column is visible).
const DATE_SWAPS: Record<string, { dateOnly: string; time: string }> = {
  modified: { dateOnly: "modified_date", time: "modified_time" },
  accessed: { dateOnly: "accessed_date", time: "accessed_time" },
  created: { dateOnly: "created_date", time: "created_time" },
};

export const TIMESTAMP_BASES = Object.keys(DATE_SWAPS);

/// True for the "*_date" / "*_time" halves of a timestamp — the column
/// pickers present those through their base's control, not as own entries.
export function isTimestampPart(key: string): boolean {
  return TIMESTAMP_BASES.some(
    (b) => key === `${b}_date` || key === `${b}_time`,
  );
}

/// Collapse a config key onto its picker row: timestamp parts map to their
/// base, everything else to itself.
export function columnRowId(key: string): string {
  for (const b of TIMESTAMP_BASES) {
    if (key === `${b}_date` || key === `${b}_time`) return b;
  }
  return key;
}

/// The four presentations of a timestamp in the file list. "datetime" is
/// the compound single column; "split" is separate date and time columns.
export type TimestampColumnState = "datetime" | "date" | "split" | "hidden";

export const TIMESTAMP_STATE_LABELS: Record<TimestampColumnState, string> = {
  datetime: "Date & time",
  date: "Date only",
  split: "Separate columns",
  hidden: "Hidden",
};

export function getTimestampState(
  keys: string[],
  base: string,
): TimestampColumnState {
  const hasCompound = keys.includes(base);
  const hasDate = keys.includes(`${base}_date`);
  const hasTime = keys.includes(`${base}_time`);
  if ((hasCompound || hasDate) && hasTime) return "split";
  if (hasCompound) return "datetime";
  if (hasDate) return "date";
  // A lone time column (hand-edited config) reports "hidden"; picking any
  // state rewrites it into a canonical form.
  return "hidden";
}

/// Rewrite the column list so `base` is presented in `state`, replacing the
/// timestamp's existing columns in place (or inserting canonically if it
/// was hidden).
export function setTimestampState(
  keys: string[],
  base: string,
  state: TimestampColumnState,
): string[] {
  const related = [base, `${base}_date`, `${base}_time`];
  const rest = keys.filter((k) => !related.includes(k));
  const insert = {
    datetime: [base],
    date: [`${base}_date`],
    split: [`${base}_date`, `${base}_time`],
    hidden: [],
  }[state];
  if (insert.length === 0) return rest;
  const firstIdx = keys.findIndex((k) => related.includes(k));
  if (firstIdx < 0) {
    return insert.reduce((acc, k) => insertColumnKey(acc, k), rest);
  }
  const pos = keys
    .slice(0, firstIdx)
    .filter((k) => !related.includes(k)).length;
  return [...rest.slice(0, pos), ...insert, ...rest.slice(pos)];
}

/// The config key behind each rendered column, index-aligned with
/// `getVisibleColumns` (unresolvable keys dropped, "name" forced in,
/// trait-gated keys the pane's VFS lacks filtered out).
export function visibleConfigKeys(
  allColumnKeys: string[],
  traits?: MetadataTraits,
): string[] {
  const columnKeys = traitFiltered(allColumnKeys, traits);
  const hasExtension = columnKeys.includes("extension");
  const result: string[] = [];
  for (const key of columnKeys) {
    if (key === "stem") continue;
    let resolvedKey = key === "name" && hasExtension ? "stem" : key;
    const swap = DATE_SWAPS[key];
    if (swap && columnKeys.includes(swap.time)) resolvedKey = swap.dateOnly;
    if (columnsByKey.has(resolvedKey)) result.push(key);
  }
  if (!result.includes("name")) result.unshift("name");
  return result;
}

/// Reorder the config list by moving the rendered column at visible index
/// `from` to insertion boundary `to` (0..visible count). Keys hidden on
/// this pane (trait-gated, unresolvable) keep their config positions —
/// reordering on an S3 pane must not drop `mode` from the global list.
export function moveColumn(
  columnKeys: string[],
  from: number,
  to: number,
  traits?: MetadataTraits,
): string[] {
  const ordered = visibleConfigKeys(columnKeys, traits);
  if (from < 0 || from >= ordered.length) return ordered;
  const insert = from < to ? to - 1 : to;
  const [key] = ordered.splice(from, 1);
  ordered.splice(insert, 0, key);

  // Weave the reordered visible keys back through the full config,
  // leaving invisible keys in place.
  const visibleSet = new Set(visibleConfigKeys(columnKeys, traits));
  let vi = 0;
  const woven = columnKeys.map((k) => (visibleSet.has(k) ? ordered[vi++] : k));
  // A forced-in "name" exists in `ordered` but not in `columnKeys`.
  woven.push(...ordered.slice(vi));
  return woven;
}

/// Insert a column after the last visible column that canonically precedes
/// it, so quick-toggling keeps a sensibly ordered header without a full
/// reorder UI. Falls back to appending relative to unknown keys.
export function insertColumnKey(current: string[], key: string): string[] {
  const ci = canonicalOrder.indexOf(key);
  let pos = 0;
  current.forEach((k, i) => {
    if (canonicalOrder.indexOf(k) < ci) pos = i + 1;
  });
  return [...current.slice(0, pos), key, ...current.slice(pos)];
}

/** Returns columns filtered and ordered by the preference list.
 *  Falls back to all columns if the list is empty or missing.
 *  When "extension" is in the list, "name" is swapped for "stem"; likewise
 *  a compound timestamp column ("modified" etc.) is swapped for its
 *  date-only variant when the paired time column is in the list.
 *  Trait-gated columns the pane's VFS doesn't populate are dropped. */
export function getVisibleColumns(
  allColumnKeys?: string[],
  traits?: MetadataTraits,
): ColumnDef[] {
  if (!allColumnKeys || allColumnKeys.length === 0) {
    return allColumns.filter((c) => {
      const gate = TRAIT_GATES[c.key];
      return !gate || !traits || traits[gate];
    });
  }
  const columnKeys = traitFiltered(allColumnKeys, traits);
  const hasExtension = columnKeys.includes("extension");
  const result: ColumnDef[] = [];
  for (const key of columnKeys) {
    let resolvedKey = key;
    // When extension is a separate column, swap name → stem
    if (key === "name" && hasExtension) resolvedKey = "stem";
    const swap = DATE_SWAPS[key];
    if (swap && columnKeys.includes(swap.time)) resolvedKey = swap.dateOnly;
    // "stem" shouldn't appear directly in config — it's an internal swap
    if (key === "stem") continue;
    const col = columnsByKey.get(resolvedKey);
    if (col) result.push(col);
  }
  // Always include a name column even if somehow omitted
  if (!result.some((c) => c.key === "name" || c.key === "stem")) {
    result.unshift(columnsByKey.get(hasExtension ? "stem" : "name")!);
  }
  return result;
}

type ColumnHeaderProps = {
  widthPrefix: string;
  column: ColumnDef;
  sorting: Sorting;
  index: number;
  /// Width restored from runtime state; falls back to the column default.
  savedWidth?: number;
  onSort: (key: string, asc: boolean) => void;
  onReorder: (from: number, to: number) => void;
  onWidthCommit: (px: number) => void;
  onAutoSize: () => void;
};

export function ColumnHeader({
  widthPrefix,
  column,
  sorting,
  index,
  savedWidth,
  onSort,
  onReorder,
  onWidthCommit,
  onAutoSize,
}: ColumnHeaderProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [startOffset, setStartOffset] = useState<number | null>(null);
  const [reorderStart, setReorderStart] = useState<number | null>(null);
  // True once a reorder drag passed the threshold; consumed by the
  // subcolumn click handler so a drop doesn't also trigger a sort.
  const didDragRef = useRef(false);
  const dropTargetRef = useRef<number | null>(null);
  // Last width applied during a resize drag; null while no actual
  // movement happened, so a plain click on the grip commits nothing.
  const resizeWidthRef = useRef<number | null>(null);

  const onmousedown = (e: React.MouseEvent) => {
    e.preventDefault();
    resizeWidthRef.current = null;
    setStartOffset(ref.current!.offsetWidth - e.clientX);
  };

  const onmouseup = (e: MouseEvent) => {
    if (startOffset !== null) {
      e.preventDefault();
      setStartOffset(null);
      if (resizeWidthRef.current !== null) {
        onWidthCommit(resizeWidthRef.current);
        resizeWidthRef.current = null;
      }
    }
  };

  const onmousemove = (e: MouseEvent) => {
    if (startOffset !== null && startOffset + e.clientX > 10) {
      e.preventDefault();
      const width = startOffset + e.clientX;
      resizeWidthRef.current = width;
      const root = document.querySelector(":root") as HTMLElement;
      root.style.setProperty(`--${widthPrefix}-${column.key}`, `${width}px`);
    }
  };

  useEffect(() => {
    document.addEventListener("mouseup", onmouseup);
    document.addEventListener("mousemove", onmousemove);

    return () => {
      document.removeEventListener("mouseup", onmouseup);
      document.removeEventListener("mousemove", onmousemove);
    };
  }, [startOffset]);

  const onReorderMouseDown = (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    didDragRef.current = false;
    setReorderStart(e.clientX);
  };

  useEffect(() => {
    if (reorderStart === null) return;
    const container = ref.current?.parentElement;
    const indicator = container?.querySelector<HTMLElement>(
      `.${styles.dropIndicator}`,
    );

    const onmove = (e: MouseEvent) => {
      if (!didDragRef.current && Math.abs(e.clientX - reorderStart) < 5) {
        return;
      }
      if (!didDragRef.current) {
        didDragRef.current = true;
        ref.current?.classList.add(styles.reordering);
      }
      e.preventDefault();
      if (!container) return;
      const cols = Array.from(
        container.querySelectorAll<HTMLElement>(`.${styles.column}`),
      );
      let target = cols.length;
      for (let i = 0; i < cols.length; i++) {
        const r = cols[i].getBoundingClientRect();
        if (e.clientX < r.left + r.width / 2) {
          target = i;
          break;
        }
      }
      dropTargetRef.current = target;
      if (indicator) {
        const noop = target === index || target === index + 1;
        const x =
          target < cols.length
            ? cols[target].getBoundingClientRect().left
            : cols[cols.length - 1].getBoundingClientRect().right;
        indicator.style.left = `${x - container.getBoundingClientRect().left}px`;
        indicator.style.display = noop ? "none" : "block";
      }
    };

    const onup = () => {
      setReorderStart(null);
      ref.current?.classList.remove(styles.reordering);
      if (indicator) indicator.style.display = "none";
      const target = dropTargetRef.current;
      dropTargetRef.current = null;
      if (
        didDragRef.current &&
        target !== null &&
        target !== index &&
        target !== index + 1
      ) {
        onReorder(index, target);
      }
    };

    document.addEventListener("mousemove", onmove);
    document.addEventListener("mouseup", onup);
    return () => {
      document.removeEventListener("mousemove", onmove);
      document.removeEventListener("mouseup", onup);
    };
  }, [reorderStart, index, onReorder]);

  // Keyed on the restored number: fires on mount, when runtime state
  // arrives asynchronously, and on cross-window updates — but never
  // stomps an in-progress drag (the stored value doesn't change mid-drag).
  useEffect(() => {
    const root = document.querySelector(":root") as HTMLElement;
    root.style.setProperty(
      `--${widthPrefix}-${column.key}`,
      `${savedWidth ?? column.initialWidth}px`,
    );
  }, [savedWidth]);

  const defaultSubcolStyle = {
    flexGrow: 1,
    flexShrink: 1,
  };

  return (
    <>
      <div
        ref={ref}
        className={styles.column}
        onMouseDown={onReorderMouseDown}
        style={{
          width: `var(--${widthPrefix}-${column.key})`,
          textAlign: column.align,
        }}
      >
        {column.subcolumns?.map((subcol, i) => (
          <div
            key={i}
            ref={ref}
            className={`${styles.subcolumn} ${subcol.sortKey ? styles.sortable : ""}`}
            onClick={(e: React.MouseEvent) => {
              e.stopPropagation();
              if (didDragRef.current) {
                didDragRef.current = false;
                return;
              }
              if (subcol.sortKey) {
                onSort(
                  subcol.sortKey,
                  sorting.key != subcol.sortKey || !sorting.asc,
                );
              }
            }}
            style={subcol.style || defaultSubcolStyle}
          >
            {column.align == "right" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className={styles.sortingIndicator}>▲ </span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className={styles.sortingIndicator}>▼ </span>
                )}
              </>
            )}
            {subcol.name}
            {column.align == "left" && (
              <>
                {sorting.key == subcol.sortKey && sorting.asc && (
                  <span className={styles.sortingIndicator}> ▲</span>
                )}
                {sorting.key == subcol.sortKey && !sorting.asc && (
                  <span className={styles.sortingIndicator}> ▼</span>
                )}
              </>
            )}
          </div>
        ))}
      </div>
      <div
        className={styles.columnGrip}
        onMouseDown={onmousedown}
        onDoubleClick={onAutoSize}
      ></div>
    </>
  );
}
