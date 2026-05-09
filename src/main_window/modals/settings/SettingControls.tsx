import { useState } from "react";

import styles from "../SettingsEditor.module.scss";
import { SettingDef } from "./schema";

export function SettingControl({
  setting,
  onUpdate,
}: {
  setting: SettingDef;
  onUpdate: (key: string, value: any) => void;
}) {
  switch (setting.type) {
    case "boolean":
      return (
        <input
          type="checkbox"
          checked={setting.value ?? false}
          onChange={(e) => onUpdate(setting.key, e.target.checked)}
        />
      );
    case "number":
      return (
        <input
          type="number"
          value={setting.value ?? 0}
          onChange={(e) => onUpdate(setting.key, Number(e.target.value))}
          style={{ width: "80px" }}
        />
      );
    case "enum":
      return (
        <select
          value={setting.value ?? setting.enumValues?.[0] ?? ""}
          onChange={(e) => onUpdate(setting.key, e.target.value)}
        >
          {setting.enumValues?.map((v) => (
            <option key={v} value={v}>
              {v.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase())}
            </option>
          ))}
        </select>
      );
    case "string":
      return (
        <input
          type="text"
          value={setting.value ?? ""}
          onChange={(e) => onUpdate(setting.key, e.target.value)}
          style={{ width: "150px" }}
        />
      );
    default:
      return null;
  }
}

const ALL_COLUMN_KEYS = [
  { key: "name", label: "Name" },
  { key: "size", label: "Size" },
  { key: "extension", label: "Extension" },
  { key: "modified_date", label: "Modified Date" },
  { key: "modified_time", label: "Modified Time" },
  { key: "accessed_date", label: "Accessed Date" },
  { key: "accessed_time", label: "Accessed Time" },
  { key: "created_date", label: "Created Date" },
  { key: "created_time", label: "Created Time" },
  { key: "user", label: "User" },
  { key: "group", label: "Group" },
  { key: "mode", label: "Mode" },
  { key: "symlink_target", label: "Link Target" },
];

function TransferPanel({
  items,
  selected,
  onSelect,
  onAction,
  emptyLabel,
  label,
}: {
  items: { key: string; label: string; note?: string }[];
  selected: string | null;
  onSelect: (key: string) => void;
  onAction: (key: string) => void;
  emptyLabel: string;
  label: string;
}) {
  const onKeyDown = (e: React.KeyboardEvent) => {
    const idx = items.findIndex((c) => c.key === selected);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (idx < items.length - 1) onSelect(items[idx + 1].key);
      else if (idx < 0 && items.length > 0) onSelect(items[0].key);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (idx > 0) onSelect(items[idx - 1].key);
    } else if (e.key === "Enter" && selected) {
      e.preventDefault();
      onAction(selected);
    }
  };

  return (
    <div className={styles.transferPanel}>
      <div className={styles.transferHeader}>{label}</div>
      <div className={styles.transferItems} tabIndex={0} onKeyDown={onKeyDown}>
        {items.length === 0 && (
          <div className={styles.transferEmpty}>{emptyLabel}</div>
        )}
        {items.map((col) => (
          <div
            key={col.key}
            className={
              selected === col.key
                ? styles.transferItemSelected
                : styles.transferItem
            }
            onClick={() => onSelect(col.key)}
            onDoubleClick={() => onAction(col.key)}
          >
            {col.label}
            {col.note && (
              <span className={styles.transferRequired}> {col.note}</span>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function ColumnsEditor({
  value,
  onUpdate,
  settingKey,
}: {
  value: string[];
  onUpdate: (key: string, value: any) => void;
  settingKey: string;
}) {
  const current = value ?? ALL_COLUMN_KEYS.map((c) => c.key);
  const [selectedVisible, setSelectedVisible] = useState<string | null>(null);
  const [selectedAvailable, setSelectedAvailable] = useState<string | null>(
    null,
  );

  const visible = current
    .map((key) => {
      const col = ALL_COLUMN_KEYS.find((c) => c.key === key);
      if (!col) return null;
      return { ...col, note: col.key === "name" ? "(required)" : undefined };
    })
    .filter(Boolean) as { key: string; label: string; note?: string }[];
  const available = ALL_COLUMN_KEYS.filter((c) => !current.includes(c.key));

  const add = (key: string) => {
    onUpdate(settingKey, [...current, key]);
    setSelectedAvailable(null);
  };

  const remove = (key: string) => {
    if (key === "name") return;
    onUpdate(
      settingKey,
      current.filter((k) => k !== key),
    );
    setSelectedVisible(null);
  };

  const visibleIdx = selectedVisible ? current.indexOf(selectedVisible) : -1;

  const moveUp = () => {
    if (visibleIdx <= 0) return;
    const next = [...current];
    [next[visibleIdx - 1], next[visibleIdx]] = [
      next[visibleIdx],
      next[visibleIdx - 1],
    ];
    onUpdate(settingKey, next);
  };

  const moveDown = () => {
    if (visibleIdx < 0 || visibleIdx >= current.length - 1) return;
    const next = [...current];
    [next[visibleIdx], next[visibleIdx + 1]] = [
      next[visibleIdx + 1],
      next[visibleIdx],
    ];
    onUpdate(settingKey, next);
  };

  return (
    <div className={styles.transferList}>
      <TransferPanel
        label="Visible"
        items={visible}
        selected={selectedVisible}
        onSelect={setSelectedVisible}
        onAction={remove}
        emptyLabel="No columns"
      />

      <div className={styles.transferButtons}>
        <button
          type="button"
          disabled={visibleIdx <= 0}
          onClick={moveUp}
          title="Move up"
        >
          ▲
        </button>
        <button
          type="button"
          disabled={visibleIdx < 0 || visibleIdx >= current.length - 1}
          onClick={moveDown}
          title="Move down"
        >
          ▼
        </button>
        <div className={styles.transferSpacer} />
        <button
          type="button"
          disabled={!selectedVisible || selectedVisible === "name"}
          onClick={() => selectedVisible && remove(selectedVisible)}
          title="Remove column"
        >
          &rsaquo;
        </button>
        <button
          type="button"
          disabled={!selectedAvailable}
          onClick={() => selectedAvailable && add(selectedAvailable)}
          title="Add column"
        >
          &lsaquo;
        </button>
      </div>

      <TransferPanel
        label="Available"
        items={available}
        selected={selectedAvailable}
        onSelect={setSelectedAvailable}
        onAction={add}
        emptyLabel="All columns visible"
      />
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
}: {
  value: { key: string; ascending: boolean } | undefined;
  onUpdate: (key: string, value: any) => void;
  settingKey: string;
}) {
  const current = value ?? { key: "name", ascending: true };

  return (
    <div
      style={{ display: "flex", gap: "var(--space-3)", alignItems: "center" }}
    >
      <select
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
}: {
  setting: SettingDef;
  onUpdate: (key: string, value: any) => void;
}) {
  switch (setting.customWidget) {
    case "columns":
      return (
        <ColumnsEditor
          value={setting.value}
          onUpdate={onUpdate}
          settingKey={setting.key}
        />
      );
    case "default_sort":
      return (
        <DefaultSortEditor
          value={setting.value}
          onUpdate={onUpdate}
          settingKey={setting.key}
        />
      );
    default:
      return null;
  }
}
