import { Fragment, useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { PreferencesState, UserCommandEntry } from "../../lib/preferences";
import styles from "./SettingsEditor.module.scss";
import { invoke } from "@tauri-apps/api/core";

type SettingDef = {
  key: string;
  title: string;
  description: string;
  category: string;
  categoryTitle: string;
  type: "boolean" | "string" | "number" | "enum" | "custom";
  enumValues?: string[];
  customWidget?: string;
  value: any;
  modified: boolean;
};

function resolveRef(schema: any, refPath: string): any {
  // Resolve "#/definitions/Foo" style $ref pointers
  const parts = refPath.replace(/^#\//, "").split("/");
  let node = schema;
  for (const part of parts) {
    node = node?.[part];
  }
  return node;
}

function resolveSchema(root: any, node: any): any {
  if (!node) return node;
  // Direct $ref
  if (node.$ref) return resolveRef(root, node.$ref);
  // allOf with a single $ref (schemars pattern)
  if (node.allOf?.length === 1 && node.allOf[0].$ref) {
    return resolveRef(root, node.allOf[0].$ref);
  }
  return node;
}

function extractSettings(preferences: PreferencesState): SettingDef[] {
  const settings: SettingDef[] = [];
  const schema = preferences.schema;
  const values = preferences.settings;

  if (!schema?.properties) return settings;

  // Walk the schema properties (top-level = categories)
  for (const [category, rawCatSchema] of Object.entries(schema.properties) as [
    string,
    any,
  ][]) {
    const catSchema = resolveSchema(schema, rawCatSchema);
    if (catSchema?.type !== "object" || !catSchema.properties) continue;
    const categoryTitle =
      rawCatSchema.title || catSchema.title || category.replace(/_/g, " ");

    for (const [prop, rawPropSchema] of Object.entries(
      catSchema.properties,
    ) as [string, any][]) {
      const propSchema = resolveSchema(schema, rawPropSchema);
      const key = `${category}.${prop}`;
      const title =
        rawPropSchema.title || propSchema.title || prop.replace(/_/g, " ");
      const description =
        rawPropSchema.description || propSchema.description || "";

      // Detect string enums (schemars emits { type: "string", enum: [...] })
      const enumValues: string[] | undefined =
        propSchema.type === "string" && Array.isArray(propSchema.enum)
          ? propSchema.enum
          : undefined;

      // Custom widget registry: keys that get special UI instead of generic controls
      const customWidgets: Record<string, string> = {
        "appearance.columns": "columns",
        "behavior.default_sort": "default_sort",
      };
      const customWidget = customWidgets[key];

      const type: SettingDef["type"] = customWidget
        ? "custom"
        : enumValues
          ? "enum"
          : propSchema.type === "boolean"
            ? "boolean"
            : propSchema.type === "integer" || propSchema.type === "number"
              ? "number"
              : "string";

      const value = (values as any)?.[category]?.[prop];

      settings.push({
        key,
        title,
        description,
        category,
        categoryTitle,
        type,
        enumValues,
        customWidget,
        value,
        modified: preferences.modified_keys.includes(key),
      });
    }
  }

  return settings;
}

function SettingControl({
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

function CustomWidget({
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

type Tab = "settings" | "keybindings" | "commands";

function emptyCommand(): UserCommandEntry {
  return { title: "", run: "", terminal: false };
}

function CommandsEditor({ commands }: { commands: UserCommandEntry[] }) {
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [editForm, setEditForm] = useState<UserCommandEntry>(emptyCommand());
  const [isAdding, setIsAdding] = useState(false);

  const startEdit = (index: number) => {
    setEditingIndex(index);
    setEditForm({ ...commands[index] });
    setIsAdding(false);
  };

  const startAdd = () => {
    setEditingIndex(null);
    setEditForm(emptyCommand());
    setIsAdding(true);
  };

  const cancelEdit = () => {
    setEditingIndex(null);
    setIsAdding(false);
  };

  const saveEdit = async () => {
    try {
      if (isAdding) {
        await invoke("add_user_command_entry", { entry: editForm });
      } else if (editingIndex !== null) {
        await invoke("update_user_command_entry", {
          index: editingIndex,
          entry: editForm,
        });
      }
      setEditingIndex(null);
      setIsAdding(false);
    } catch (e) {
      console.error("Failed to save command:", e);
    }
  };

  const removeCommand = async (index: number) => {
    try {
      await invoke("remove_user_command_entry", { index });
      if (editingIndex === index) {
        setEditingIndex(null);
      }
    } catch (e) {
      console.error("Failed to remove command:", e);
    }
  };

  const renderForm = () => (
    <div className={styles.commandForm}>
      <label>
        Title
        <input
          type="text"
          value={editForm.title}
          onChange={(e) => setEditForm({ ...editForm, title: e.target.value })}
          autoFocus
        />
      </label>
      <label>
        Run
        <textarea
          value={editForm.run}
          onChange={(e) => setEditForm({ ...editForm, run: e.target.value })}
          rows={3}
          style={{ fontFamily: "monospace" }}
        />
      </label>
      <div className={styles.commandFormRow}>
        <label>
          Key
          <input
            type="text"
            value={editForm.key ?? ""}
            onChange={(e) =>
              setEditForm({
                ...editForm,
                key: e.target.value || undefined,
              })
            }
            placeholder="e.g. alt+z"
            style={{ width: "120px" }}
          />
        </label>
        <label>
          When
          <select
            value={editForm.when ?? "any"}
            onChange={(e) =>
              setEditForm({
                ...editForm,
                when: e.target.value === "any" ? undefined : e.target.value,
              })
            }
          >
            <option value="any">Any</option>
            <option value="file">File</option>
            <option value="directory">Directory</option>
            <option value="selection">Selection</option>
          </select>
        </label>
        <label className={styles.checkboxLabel}>
          <input
            type="checkbox"
            checked={editForm.terminal}
            onChange={(e) =>
              setEditForm({ ...editForm, terminal: e.target.checked })
            }
          />
          Terminal
        </label>
      </div>
      <div className={styles.commandFormActions}>
        <button onClick={saveEdit}>Save</button>
        <button onClick={cancelEdit}>Cancel</button>
      </div>
    </div>
  );

  return (
    <div className={styles.settingsList}>
      {commands.length === 0 && !isAdding && (
        <div
          style={{ color: "var(--color-fg-muted)", padding: "var(--space-4)" }}
        >
          No user commands configured
        </div>
      )}
      {commands.map((cmd, i) => (
        <div key={i}>
          <div className={styles.settingRow}>
            <div className={styles.settingInfo}>
              <div className={styles.settingLabel}>
                {cmd.title || "(untitled)"}
              </div>
              <div className={styles.settingDescription}>
                <code>{cmd.run}</code>
                {cmd.key && <> &middot; {cmd.key}</>}
                {cmd.when && <> &middot; when: {cmd.when}</>}
                {cmd.terminal && <> &middot; terminal</>}
              </div>
            </div>
            <div className={styles.settingControl}>
              <button onClick={() => startEdit(i)}>Edit</button>
              <button onClick={() => removeCommand(i)}>Delete</button>
            </div>
          </div>
          {editingIndex === i && renderForm()}
        </div>
      ))}
      {isAdding && renderForm()}
      {!isAdding && editingIndex === null && (
        <div style={{ padding: "var(--space-4) 0" }}>
          <button onClick={startAdd}>Add Command</button>
        </div>
      )}
      <div className={styles.templateHelp}>
        <div className={styles.templateHelpTitle}>Template Reference</div>
        <details>
          <summary>Details</summary>
          <div className={styles.templateHelpBody}>
            <p>
              The <b>Run</b> field uses{" "}
              <a
                href="https://docs.rs/minijinja"
                target="_blank"
                rel="noreferrer"
              >
                Jinja2
              </a>{" "}
              templates.
            </p>

            <h4>Variables</h4>
            <table>
              <tbody>
                <tr>
                  <td>
                    <code>{"{{ dir }}"}</code>
                  </td>
                  <td>Current pane directory</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ other_dir }}"}</code>
                  </td>
                  <td>Other pane directory</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ hostname }}"}</code>
                  </td>
                  <td>Machine hostname</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ env.HOME }}"}</code>
                  </td>
                  <td>Environment variable (any name)</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.name }}"}</code>
                  </td>
                  <td>Focused file name</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.path }}"}</code>
                  </td>
                  <td>Focused file full path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.stem }}"}</code>
                  </td>
                  <td>Filename without extension</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.ext }}"}</code>
                  </td>
                  <td>File extension</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.size }}"}</code>
                  </td>
                  <td>File size in bytes</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.modified }}"}</code>
                  </td>
                  <td>Last modified (Unix timestamp)</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.is_dir }}"}</code>
                  </td>
                  <td>Whether focused item is a directory</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ files }}"}</code>
                  </td>
                  <td>Selected files (or focused file)</td>
                </tr>
              </tbody>
            </table>

            <h4>Filters</h4>
            <table>
              <tbody>
                <tr>
                  <td>
                    <code>{"shell_quote"}</code>
                  </td>
                  <td>Shell-escape a string</td>
                </tr>
                <tr>
                  <td>
                    <code>{"basename"}</code>
                  </td>
                  <td>Extract filename from path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"dirname"}</code>
                  </td>
                  <td>Extract directory from path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"stem"}</code>
                  </td>
                  <td>Filename without extension</td>
                </tr>
                <tr>
                  <td>
                    <code>{"ext"}</code>
                  </td>
                  <td>Extract extension from path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"regex_replace(pattern, replacement)"}</code>
                  </td>
                  <td>Regex substitution</td>
                </tr>
                <tr>
                  <td>
                    <code>{"join_path"}</code>
                  </td>
                  <td>Join path segments</td>
                </tr>
              </tbody>
            </table>

            <h4>Functions</h4>
            <table>
              <tbody>
                <tr>
                  <td>
                    <code>{'prompt("Label", default="")'}</code>
                  </td>
                  <td>Show input dialog before running</td>
                </tr>
                <tr>
                  <td>
                    <code>{'confirm("Are you sure?")'}</code>
                  </td>
                  <td>Show confirmation — aborts if declined</td>
                </tr>
              </tbody>
            </table>

            <h4>Examples</h4>
            <p>
              <code>
                {
                  "tar czf {{ file.stem }}.tar.gz {{ files | map(attribute='name') | shell_quote | join(' ') }}"
                }
              </code>
            </p>
            <p>
              <code>
                {
                  'mv {{ file.name | shell_quote }} {{ prompt("New name", file.name) | shell_quote }}'
                }
              </code>
            </p>
            <p>
              <code>
                {
                  '{% do confirm("Play " ~ file.name ~ "?" ) %} paplay {{ file.path | shell_quote }}'
                }
              </code>
            </p>
          </div>
        </details>
      </div>
    </div>
  );
}

const preventAutoFocus = (e: Event) => e.preventDefault();

export default function SettingsEditor({
  preferences,
}: {
  preferences: PreferencesState | null;
}) {
  const [filter, setFilter] = useState("");
  const [activeCategory, setActiveCategory] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<Tab>("settings");

  const allSettings = useMemo(
    () => (preferences ? extractSettings(preferences) : []),
    [preferences],
  );

  const categories = useMemo(() => {
    const cats = new Map<string, string>();
    for (const s of allSettings) cats.set(s.category, s.categoryTitle);
    return Array.from(cats.entries());
  }, [allSettings]);

  const filteredSettings = useMemo(() => {
    let result = allSettings;
    if (activeCategory) {
      result = result.filter((s) => s.category === activeCategory);
    }
    if (filter) {
      const lower = filter.toLowerCase();
      result = result.filter(
        (s) =>
          s.title.toLowerCase().includes(lower) ||
          s.description.toLowerCase().includes(lower) ||
          s.key.toLowerCase().includes(lower),
      );
    }
    return result;
  }, [allSettings, activeCategory, filter]);

  const filteredBindings = useMemo(() => {
    if (!preferences) return [];
    const commands = preferences.commands;
    if (!filter) return commands;
    const lower = filter.toLowerCase();
    return commands.filter(
      (c) =>
        c.name.toLowerCase().includes(lower) ||
        c.id.toLowerCase().includes(lower) ||
        (c.shortcut && c.shortcut.toLowerCase().includes(lower)),
    );
  }, [preferences, filter]);

  const onUpdate = async (key: string, value: any) => {
    try {
      await invoke("update_preference", { key, value });
    } catch (e) {
      console.error("Failed to update preference:", e);
    }
  };

  const onReset = async (key: string) => {
    try {
      await invoke("reset_preference", { key });
    } catch (e) {
      console.error("Failed to reset preference:", e);
    }
  };

  return (
    <Dialog.Content
      className={styles.content}
      onCloseAutoFocus={preventAutoFocus}
    >
      <Dialog.Title className="sr-only">Settings</Dialog.Title>
      <div className={styles.header}>
        <input
          className={styles.searchBox}
          type="text"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Search settings..."
          autoFocus
        />
      </div>
      <div className={styles.tabBar}>
        <button
          className={activeTab === "settings" ? styles.tabActive : styles.tab}
          onClick={() => setActiveTab("settings")}
        >
          Preferences
        </button>
        <button
          className={
            activeTab === "keybindings" ? styles.tabActive : styles.tab
          }
          onClick={() => setActiveTab("keybindings")}
        >
          Keybindings
        </button>
        <button
          className={activeTab === "commands" ? styles.tabActive : styles.tab}
          onClick={() => setActiveTab("commands")}
        >
          Commands
        </button>
      </div>
      <div className={styles.body}>
        {activeTab === "settings" && (
          <>
            <div className={styles.sidebar}>
              <div
                className={
                  activeCategory === null
                    ? styles.sidebarItemActive
                    : styles.sidebarItem
                }
                onClick={() => setActiveCategory(null)}
              >
                All
              </div>
              {categories.map(([key, title]) => (
                <div
                  key={key}
                  className={
                    activeCategory === key
                      ? styles.sidebarItemActive
                      : styles.sidebarItem
                  }
                  onClick={() => setActiveCategory(key)}
                >
                  {title}
                </div>
              ))}
            </div>
            <div className={styles.settingsList}>
              {filteredSettings.length === 0 && (
                <div
                  style={{
                    color: "var(--color-fg-muted)",
                    padding: "var(--space-4)",
                  }}
                >
                  No settings found
                </div>
              )}
              {filteredSettings.map((setting) => (
                <div
                  key={setting.key}
                  className={
                    setting.customWidget === "columns"
                      ? styles.settingRowFull
                      : styles.settingRow
                  }
                >
                  <div className={styles.settingInfo}>
                    <div className={styles.settingLabel}>
                      {setting.title}
                      {setting.modified && (
                        <button
                          type="button"
                          className={styles.resetButton}
                          onClick={() => onReset(setting.key)}
                          title="Reset to default"
                        >
                          Reset
                        </button>
                      )}
                    </div>
                    {setting.description && (
                      <div className={styles.settingDescription}>
                        {setting.description}
                      </div>
                    )}
                  </div>
                  {setting.customWidget === "columns" ? (
                    <CustomWidget setting={setting} onUpdate={onUpdate} />
                  ) : (
                    <div className={styles.settingControl}>
                      {setting.type === "custom" ? (
                        <CustomWidget setting={setting} onUpdate={onUpdate} />
                      ) : (
                        <SettingControl setting={setting} onUpdate={onUpdate} />
                      )}
                    </div>
                  )}
                </div>
              ))}
            </div>
          </>
        )}
        {activeTab === "keybindings" && (
          <div className={styles.settingsList}>
            <table className={styles.keybindingsTable}>
              <thead>
                <tr>
                  <th>Command</th>
                  <th>Shortcut</th>
                  <th>When</th>
                </tr>
              </thead>
              <tbody>
                {filteredBindings.map((cmd) => {
                  const binding = preferences?.bindings.find(
                    (b) => b.command === cmd.id,
                  );
                  const when = binding?.when;
                  return (
                    <tr key={cmd.id}>
                      <td>{cmd.name}</td>
                      <td>
                        {cmd.shortcut_display.length > 0 ? (
                          <span className={styles.shortcutKbd}>
                            {cmd.shortcut_display.map((part, i) => (
                              <Fragment key={i}>
                                {i !== 0 ? " + " : ""}
                                <kbd>{part}</kbd>
                              </Fragment>
                            ))}
                          </span>
                        ) : (
                          <span className={styles.noShortcut}>&mdash;</span>
                        )}
                      </td>
                      <td>
                        <span className={styles.whenLabel}>
                          {when
                            ? when
                                .replace(/_/g, " ")
                                .replace(/\b\w/g, (c) => c.toUpperCase())
                            : "Global"}
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
        {activeTab === "commands" && (
          <CommandsEditor commands={preferences?.user_commands ?? []} />
        )}
      </div>
      <div className={styles.footer}>
        <button onClick={() => safeCommand("open_config_file")}>
          Open Settings File
        </button>
        <button onClick={() => safeCommand("close_modal")}>Close</button>
      </div>
    </Dialog.Content>
  );
}
