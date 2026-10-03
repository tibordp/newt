import { useId, useState } from "react";

import styles from "../SettingsEditor.module.scss";
import {
  COLUMN_CHOICES,
  TIMESTAMP_BASES,
  TIMESTAMP_STATE_LABELS,
  TimestampColumnState,
  columnRowId,
  getTimestampState,
  insertColumnKey,
  isTimestampPart,
  setTimestampState,
} from "../../columns";
import { SettingDef } from "./schema";

type ControlLabelling = {
  labelledBy: string;
  describedBy?: string;
};

export function SettingControl({
  setting,
  onUpdate,
  labelledBy,
  describedBy,
}: {
  setting: SettingDef;
  onUpdate: (key: string, value: any) => void;
} & ControlLabelling) {
  const aria = {
    "aria-labelledby": labelledBy,
    "aria-describedby": describedBy,
  };
  switch (setting.type) {
    case "boolean":
      return (
        <input
          type="checkbox"
          {...aria}
          checked={setting.value ?? false}
          onChange={(e) => onUpdate(setting.key, e.target.checked)}
        />
      );
    case "number":
      return (
        <input
          type="number"
          {...aria}
          value={setting.value ?? 0}
          onChange={(e) => onUpdate(setting.key, Number(e.target.value))}
          style={{ width: "80px" }}
        />
      );
    case "enum":
      return (
        <select
          {...aria}
          value={setting.value ?? setting.enumValues?.[0] ?? ""}
          onChange={(e) => onUpdate(setting.key, e.target.value)}
        >
          {setting.enumValues?.map((v) => (
            <option key={v} value={v}>
              {setting.enumLabels?.[v] ??
                v.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase())}
            </option>
          ))}
        </select>
      );
    case "list":
      return (
        <StringListEditor
          value={setting.value ?? []}
          onUpdate={onUpdate}
          settingKey={setting.key}
          labelledBy={labelledBy}
          describedBy={describedBy}
        />
      );
    case "string":
      return (
        <input
          type="text"
          {...aria}
          value={setting.value ?? ""}
          onChange={(e) => onUpdate(setting.key, e.target.value)}
          style={{ width: "150px" }}
        />
      );
    default:
      return null;
  }
}

/// A list of strings: a row per item, and an empty row at the end that
/// adds what is typed into it.
function StringListEditor({
  value,
  onUpdate,
  settingKey,
  labelledBy,
  describedBy,
}: {
  value: string[];
  onUpdate: (key: string, value: any) => void;
  settingKey: string;
} & ControlLabelling) {
  const [draft, setDraft] = useState("");
  const set = (next: string[]) => onUpdate(settingKey, next);
  const add = () => {
    if (!draft.trim()) return;
    set([...value, draft.trim()]);
    setDraft("");
  };
  return (
    <div
      className={styles.stringList}
      role="group"
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
    >
      {value.map((item, i) => (
        <div key={i} className={styles.stringListRow}>
          <input
            type="text"
            value={item}
            aria-label={`Item ${i + 1}`}
            onChange={(e) =>
              set(value.map((x, j) => (j === i ? e.target.value : x)))
            }
            onBlur={() => {
              if (!item.trim()) set(value.filter((_, j) => j !== i));
            }}
          />
          <button
            type="button"
            className={styles.stringListRemove}
            aria-label={`Remove ${item}`}
            title="Remove"
            onClick={() => set(value.filter((_, j) => j !== i))}
          >
            ×
          </button>
        </div>
      ))}
      <div className={styles.stringListRow}>
        <input
          type="text"
          className={styles.stringListAdd}
          value={draft}
          placeholder="Add…"
          aria-label="Add an item"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add();
            }
          }}
          onBlur={add}
        />
        {/* Keeps the input as wide as the ones above. */}
        <span
          className={`${styles.stringListRemove} ${styles.stringListSpacer}`}
          aria-hidden
        >
          ×
        </span>
      </div>
    </div>
  );
}

/// Rows of the columns widget: the pickable columns with timestamp
/// date/time parts collapsed onto their base, like the header context menu.
const COLUMN_ROWS = COLUMN_CHOICES.filter((c) => !isTimestampPart(c.key));

function ColumnsEditor({
  value,
  onUpdate,
  settingKey,
  labelledBy,
  describedBy,
}: {
  value: string[];
  onUpdate: (key: string, value: any) => void;
  settingKey: string;
} & ControlLabelling) {
  const idBase = useId();
  const current = value ?? COLUMN_CHOICES.map((c) => c.key);
  const [drag, setDrag] = useState<{ id: string; order: string[] } | null>(
    null,
  );

  // Visible rows in configured order; hidden rows below in canonical order.
  const visibleIds: string[] = [];
  for (const k of current) {
    const id = columnRowId(k);
    if (COLUMN_ROWS.some((r) => r.key === id) && !visibleIds.includes(id)) {
      visibleIds.push(id);
    }
  }
  if (!visibleIds.includes("name")) visibleIds.unshift("name");
  const hiddenIds = COLUMN_ROWS.map((r) => r.key).filter(
    (id) => !visibleIds.includes(id),
  );
  const displayIds = drag?.order ?? visibleIds;

  /// Rebuild the config list from a visible-row order, expanding timestamp
  /// rows to their current presentation's keys.
  const configFor = (order: string[]): string[] =>
    order.flatMap((id) => {
      if (!TIMESTAMP_BASES.includes(id)) return [id];
      switch (getTimestampState(current, id)) {
        case "datetime":
          return [id];
        case "date":
          return [`${id}_date`];
        case "split":
          return [`${id}_date`, `${id}_time`];
        case "hidden":
          return [];
      }
    });

  const setSimple = (id: string, checked: boolean) => {
    onUpdate(
      settingKey,
      checked ? insertColumnKey(current, id) : current.filter((k) => k !== id),
    );
  };

  const setTimestamp = (id: string, state: TimestampColumnState) => {
    onUpdate(settingKey, setTimestampState(current, id, state));
  };

  const moveRow = (id: string, delta: number) => {
    const idx = visibleIds.indexOf(id);
    const to = idx + delta;
    if (idx < 0 || to < 0 || to >= visibleIds.length) return;
    const next = [...visibleIds];
    [next[idx], next[to]] = [next[to], next[idx]];
    onUpdate(settingKey, configFor(next));
  };

  const startDrag = (id: string) => (e: React.MouseEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    let order = visibleIds;
    setDrag({ id, order });
    const onMove = (ev: MouseEvent) => {
      const rows = document.querySelectorAll<HTMLElement>(`[data-column-row]`);
      for (const el of rows) {
        const rowId = el.dataset.columnRow!;
        if (rowId === id || !order.includes(rowId)) continue;
        const r = el.getBoundingClientRect();
        if (ev.clientY < r.top || ev.clientY > r.bottom) continue;
        const without = order.filter((k) => k !== id);
        const idx = without.indexOf(rowId);
        const insertAt = ev.clientY > r.top + r.height / 2 ? idx + 1 : idx;
        const next = [...without];
        next.splice(insertAt, 0, id);
        if (next.join() !== order.join()) {
          order = next;
          setDrag({ id, order });
        }
        break;
      }
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      setDrag(null);
      if (order.join() !== visibleIds.join()) {
        onUpdate(settingKey, configFor(order));
      }
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  };

  const renderRow = (id: string, visible: boolean) => {
    const isTs = TIMESTAMP_BASES.includes(id);
    const label = COLUMN_ROWS.find((r) => r.key === id)?.label ?? id;
    return (
      <div
        key={id}
        data-column-row={id}
        className={`${styles.columnRow} ${
          drag?.id === id ? styles.columnRowDragging : ""
        } ${visible ? "" : styles.columnRowHidden}`}
      >
        {visible && (
          <button
            type="button"
            className={styles.columnDragHandle}
            title="Drag to reorder (arrow keys move)"
            aria-label={`Reorder ${label}`}
            aria-keyshortcuts="ArrowUp ArrowDown"
            onMouseDown={startDrag(id)}
            onKeyDown={(e) => {
              if (e.key === "ArrowUp") {
                e.preventDefault();
                moveRow(id, -1);
              } else if (e.key === "ArrowDown") {
                e.preventDefault();
                moveRow(id, 1);
              }
            }}
          >
            ⠿
          </button>
        )}
        <label className={styles.columnRowLabel}>
          <input
            type="checkbox"
            checked={visible}
            disabled={id === "name"}
            onChange={(e) =>
              isTs
                ? setTimestamp(id, e.target.checked ? "datetime" : "hidden")
                : setSimple(id, e.target.checked)
            }
          />
          {label}
          {id === "name" && (
            <span className={styles.columnRequired}>(required)</span>
          )}
        </label>
        {isTs && visible && (
          <select
            value={getTimestampState(current, id)}
            onChange={(e) =>
              setTimestamp(id, e.target.value as TimestampColumnState)
            }
            aria-label={`${label} format`}
          >
            {(["datetime", "date", "split"] as const).map((s) => (
              <option key={s} value={s}>
                {TIMESTAMP_STATE_LABELS[s]}
              </option>
            ))}
          </select>
        )}
      </div>
    );
  };

  return (
    <div
      className={styles.columnPanels}
      role="group"
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
    >
      <div
        className={styles.columnList}
        role="group"
        aria-labelledby={`${idBase}-visible`}
      >
        <div id={`${idBase}-visible`} className={styles.columnListHeader}>
          Visible
        </div>
        {displayIds.map((id) => renderRow(id, true))}
      </div>
      <div
        className={styles.columnList}
        role="group"
        aria-labelledby={`${idBase}-hidden`}
      >
        <div id={`${idBase}-hidden`} className={styles.columnListHeader}>
          Hidden
        </div>
        {hiddenIds.map((id) => renderRow(id, false))}
        {hiddenIds.length === 0 && (
          <div className={styles.columnListEmpty}>All columns visible</div>
        )}
      </div>
    </div>
  );
}

const SORT_KEY_OPTIONS = [
  { value: "name", label: "Name" },
  { value: "extension", label: "Extension" },
  { value: "size", label: "Size" },
  { value: "modified", label: "Modified" },
  { value: "accessed", label: "Accessed" },
  { value: "created", label: "Created" },
];

function DefaultSortEditor({
  value,
  onUpdate,
  settingKey,
  labelledBy,
  describedBy,
}: {
  value: { key: string; ascending: boolean } | undefined;
  onUpdate: (key: string, value: any) => void;
  settingKey: string;
} & ControlLabelling) {
  const current = value ?? { key: "name", ascending: true };

  return (
    <div
      style={{ display: "flex", gap: "var(--space-3)", alignItems: "center" }}
      role="group"
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
    >
      <select
        aria-labelledby={labelledBy}
        value={current.key}
        onChange={(e) =>
          onUpdate(settingKey, { ...current, key: e.target.value })
        }
      >
        {SORT_KEY_OPTIONS.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>
      <label
        style={{
          display: "flex",
          alignItems: "center",
          gap: "var(--space-2)",
          fontSize: "0.9em",
        }}
      >
        <input
          type="checkbox"
          checked={current.ascending}
          onChange={(e) =>
            onUpdate(settingKey, { ...current, ascending: e.target.checked })
          }
        />
        Ascending
      </label>
    </div>
  );
}

export function CustomWidget({
  setting,
  onUpdate,
  labelledBy,
  describedBy,
}: {
  setting: SettingDef;
  onUpdate: (key: string, value: any) => void;
} & ControlLabelling) {
  switch (setting.customWidget) {
    case "columns":
      return (
        <ColumnsEditor
          value={setting.value}
          onUpdate={onUpdate}
          settingKey={setting.key}
          labelledBy={labelledBy}
          describedBy={describedBy}
        />
      );
    case "default_sort":
      return (
        <DefaultSortEditor
          value={setting.value}
          onUpdate={onUpdate}
          settingKey={setting.key}
          labelledBy={labelledBy}
          describedBy={describedBy}
        />
      );
    default:
      return null;
  }
}
