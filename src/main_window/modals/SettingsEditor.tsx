import {
  Fragment,
  useEffect,
  useMemo,
  useRef,
  useState,
  KeyboardEvent as ReactKeyboardEvent,
} from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import {
  CommandInfo,
  PreferencesState,
  ResolvedBinding,
  UserCommandEntry,
} from "../../lib/preferences";
import { normalizeKeyEvent } from "../../lib/commands";
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

function CommandsEditor({
  commands,
  bindings,
  allCommands,
}: {
  commands: UserCommandEntry[];
  bindings: ResolvedBinding[];
  allCommands: CommandInfo[];
}) {
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [editForm, setEditForm] = useState<UserCommandEntry>(emptyCommand());
  const [isAdding, setIsAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Tracks the (key, when) the user has explicitly acknowledged as a
  // conflict — same pattern as KeybindingsEditor.
  const [acked, setAcked] = useState<{ key: string; when: string } | null>(
    null,
  );

  const commandsById = useMemo(() => {
    const m = new Map<string, CommandInfo>();
    for (const c of allCommands) m.set(c.id, c);
    return m;
  }, [allCommands]);

  const startEdit = (index: number) => {
    setEditingIndex(index);
    setEditForm({ ...commands[index] });
    setIsAdding(false);
    setError(null);
    setAcked(null);
  };

  const startAdd = () => {
    setEditingIndex(null);
    setEditForm(emptyCommand());
    setIsAdding(true);
    setError(null);
    setAcked(null);
  };

  const cancelEdit = () => {
    setEditingIndex(null);
    setIsAdding(false);
    setError(null);
    setAcked(null);
  };

  // The keybinding for a user command always resolves with `pane_focused` —
  // see `resolve_bindings` in preferences/mod.rs. The editForm's `when` field
  // is a separate concept (file/directory/selection match for the command's
  // run condition), not the dispatch context.
  const KEYBINDING_WHEN = "pane_focused";

  // The "own" command id while editing/adding — used so conflict detection
  // doesn't flag the command's own current binding as a conflict with itself.
  const ownCommandId =
    editingIndex !== null ? `user_command_${editingIndex}` : "__new__";

  const saveEdit = async () => {
    try {
      // We do NOT pre-clear conflicting bindings: the new binding wins by
      // resolution order, and the user can later Reset either side to
      // reclaim — Reset is symmetric and explicit.
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
      setError(null);
    } catch (e: any) {
      setError(typeof e === "string" ? e : (e?.message ?? String(e)));
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

  const renderForm = () => {
    const candidateKey = editForm.key ?? "";
    const conflicts = candidateKey
      ? detectConflicts(
          candidateKey,
          KEYBINDING_WHEN,
          ownCommandId,
          bindings,
          commandsById,
        )
      : [];
    const hardConflicts = conflicts.filter((c) => c.kind === "hard");
    const softConflicts = conflicts.filter((c) => c.kind === "soft");
    const keyValid = !candidateKey || isCompleteKey(candidateKey);
    const ackMatches =
      !!acked && acked.key === candidateKey && acked.when === KEYBINDING_WHEN;
    const canSave = keyValid && (hardConflicts.length === 0 || ackMatches);

    return (
      <div className={styles.commandForm}>
        <label>
          Title
          <input
            type="text"
            value={editForm.title}
            onChange={(e) =>
              setEditForm({ ...editForm, title: e.target.value })
            }
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
            <KeyCaptureInput
              value={candidateKey}
              onChange={(k) => {
                setEditForm({ ...editForm, key: k || undefined });
                setAcked(null);
              }}
              size="regular"
            />
          </label>
          <label>
            Applies to
            <select
              value={editForm.applies_to ?? "any"}
              onChange={(e) =>
                setEditForm({
                  ...editForm,
                  applies_to:
                    e.target.value === "any" ? undefined : e.target.value,
                })
              }
            >
              <option value="any">Any focused item</option>
              <option value="file">Files only</option>
              <option value="directory">Directories only</option>
              <option value="selection">Selection</option>
            </select>
          </label>
          <label>
            <span className={styles.checkboxSpacer} aria-hidden="true">
              &nbsp;
            </span>
            <span className={styles.checkboxRow}>
              <input
                type="checkbox"
                checked={editForm.terminal}
                onChange={(e) =>
                  setEditForm({ ...editForm, terminal: e.target.checked })
                }
              />
              Run in terminal
            </span>
          </label>
        </div>

        {!keyValid && (
          <div className={styles.kbBannerWarn}>
            Press a non-modifier key (letter, number, function key, etc.).
          </div>
        )}

        {hardConflicts.length > 0 && (
          <div className={styles.kbBannerError}>
            <span>
              Already used by{" "}
              {hardConflicts
                .map((c) => `${c.commandName} (${whenLabel(c.binding.when)})`)
                .join(", ")}
              .
            </span>
            {!ackMatches && (
              <button
                onClick={() =>
                  setAcked({ key: candidateKey, when: KEYBINDING_WHEN })
                }
                disabled={!keyValid}
                title="Acknowledge the conflict — Save will then overwrite the existing binding"
              >
                Override
              </button>
            )}
          </div>
        )}

        {hardConflicts.length === 0 && softConflicts.length > 0 && (
          <div className={styles.kbBannerWarn}>
            Also used by{" "}
            {softConflicts
              .map((c) => `${c.commandName} (${whenLabel(c.binding.when)})`)
              .join(", ")}
            . Whichever context applies will win.
          </div>
        )}

        {error && <div className={styles.kbBannerError}>{error}</div>}

        <div className={styles.commandFormActions}>
          {!isAdding && editingIndex !== null && (
            <button onClick={() => removeCommand(editingIndex)}>Delete</button>
          )}
          <div className={styles.commandFormPrimary}>
            <button onClick={cancelEdit}>Cancel</button>
            <button
              className="suggested"
              onClick={() => saveEdit()}
              disabled={!canSave}
            >
              Save
            </button>
          </div>
        </div>
      </div>
    );
  };

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
        <div key={i} className={styles.userCmdEntry}>
          {editingIndex === i ? (
            renderForm()
          ) : (
            <div className={styles.userCmdRow}>
              <div className={styles.settingInfo}>
                <div className={styles.userCmdHeader}>
                  <span className={styles.settingLabel}>
                    {cmd.title || "(untitled)"}
                  </span>
                  {cmd.key && (
                    <span className={styles.userCmdShortcut}>
                      {shortcutChips(cmd.key)}
                    </span>
                  )}
                </div>
                {cmd.run.trim() ? (
                  <pre className={styles.userCmdCode}>{cmd.run}</pre>
                ) : (
                  <pre className={styles.userCmdCodeEmpty}>(no command)</pre>
                )}
                {(cmd.applies_to || cmd.terminal) && (
                  <div className={styles.userCmdTags}>
                    {cmd.applies_to && (
                      <span className={styles.userCmdTag}>
                        applies to {cmd.applies_to}
                      </span>
                    )}
                    {cmd.terminal && (
                      <span className={styles.userCmdTag}>terminal</span>
                    )}
                  </div>
                )}
              </div>
              <div className={styles.settingControl}>
                <button onClick={() => startEdit(i)}>Edit</button>
              </div>
            </div>
          )}
        </div>
      ))}
      {isAdding && <div className={styles.userCmdEntry}>{renderForm()}</div>}
      {!isAdding && (
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
            <pre className={styles.userCmdCode}>
              {
                "tar czf {{ file.stem }}.tar.gz {{ files | map(attribute='name') | shell_quote | join(' ') }}"
              }
            </pre>
            <pre className={styles.userCmdCode}>
              {
                'mv {{ file.name | shell_quote }} {{ prompt("New name", file.name) | shell_quote }}'
              }
            </pre>
            <pre className={styles.userCmdCode}>
              {
                '{% do confirm("Play " ~ file.name ~ "?" ) %} paplay {{ file.path | shell_quote }}'
              }
            </pre>
          </div>
        </details>
      </div>
    </div>
  );
}

// --- Keybindings editor ---

const IS_MAC =
  typeof navigator !== "undefined" && navigator.platform.startsWith("Mac");

/// Render a normalized key string ("ctrl+shift+f5") into display segments.
/// Mirrors the Rust `render_shortcut` so captured-but-unsaved keys can be
/// previewed before the round-trip through the backend.
function renderShortcut(key: string): string[] {
  return key.split("+").map((part) => {
    switch (part.toLowerCase()) {
      case "ctrl":
        return "Ctrl";
      case "meta":
        return IS_MAC ? "⌘" : "Super";
      case "shift":
        return "Shift";
      case "alt":
        return IS_MAC ? "⌥" : "Alt";
      default:
        return part.length > 0 ? part[0].toUpperCase() + part.slice(1) : "";
    }
  });
}

/// Render a normalized key string as a row of <kbd> chips separated by " + ".
function shortcutChips(key: string) {
  return (
    <span className={styles.shortcutKbd}>
      {renderShortcut(key).map((part, i) => (
        <Fragment key={i}>
          {i !== 0 ? " + " : ""}
          <kbd>{part}</kbd>
        </Fragment>
      ))}
    </span>
  );
}

/// True if the key has at least one non-modifier component.
function isCompleteKey(key: string): boolean {
  if (!key) return false;
  const parts = key.split("+");
  const NON_MODIFIERS = parts.filter(
    (p) => !["ctrl", "meta", "shift", "alt"].includes(p.toLowerCase()),
  );
  return NON_MODIFIERS.length === 1 && NON_MODIFIERS[0].length > 0;
}

function whenLabel(when: string | undefined | null): string {
  if (!when) return "Global";
  switch (when) {
    case "pane_focused":
      return "Pane focused";
    case "terminal_focused":
      return "Terminal focused";
    default:
      return when.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  }
}

/// Are two `when` values considered the same dispatch context?
function whenEq(a: string | null | undefined, b: string | null | undefined) {
  return (a ?? "") === (b ?? "");
}

type Conflict = {
  kind: "hard" | "soft";
  binding: ResolvedBinding;
  commandName: string;
  commandId: string;
};

/// Detect conflicts for a candidate (key, when) being assigned to `commandId`.
/// - hard: another binding has the exact same (key, when) — they would collide
///   on dispatch.
/// - soft: same key in a different/overlapping when — one shadows the other in
///   that context but both exist.
function detectConflicts(
  candidateKey: string,
  candidateWhen: string,
  ownCommandId: string,
  bindings: ResolvedBinding[],
  commandsById: Map<string, CommandInfo>,
): Conflict[] {
  const conflicts: Conflict[] = [];
  for (const b of bindings) {
    if (b.command === ownCommandId) continue;
    if (b.key !== candidateKey) continue;
    const sameWhen = whenEq(b.when, candidateWhen || null);
    const candidateGlobal = !candidateWhen;
    const otherGlobal = !b.when;
    const overlaps = sameWhen || candidateGlobal || otherGlobal;
    if (!overlaps) continue;
    const cmd = commandsById.get(b.command);
    conflicts.push({
      kind: sameWhen ? "hard" : "soft",
      binding: b,
      commandName: cmd?.name ?? b.command,
      commandId: b.command,
    });
  }
  return conflicts;
}

function KeyCaptureInput({
  value,
  onChange,
  autoFocus,
  size = "compact",
}: {
  value: string;
  onChange: (key: string) => void;
  autoFocus?: boolean;
  /// "compact" matches the keybindings table row height. "regular" matches
  /// the surrounding text inputs in standard forms (CommandsEditor).
  size?: "compact" | "regular";
}) {
  const [recording, setRecording] = useState(!!autoFocus);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (autoFocus && ref.current) ref.current.focus();
  }, [autoFocus]);

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    // Tab and Escape have higher-priority semantics: Escape exits recording,
    // Tab lets the user move on to the action buttons without trapping focus.
    if (e.key === "Escape") {
      setRecording(false);
      ref.current?.blur();
      e.preventDefault();
      return;
    }
    if (e.key === "Tab") {
      // allow default focus traversal
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    const k = normalizeKeyEvent(e.nativeEvent);
    if (!k) return;
    if (isCompleteKey(k)) {
      onChange(k);
    }
  };

  const segments = value ? renderShortcut(value) : [];

  return (
    <div
      ref={ref}
      tabIndex={0}
      role="textbox"
      aria-label="Press key combination"
      className={[
        recording ? styles.keyCaptureActive : styles.keyCapture,
        size === "regular" ? styles.keyCaptureRegular : "",
      ]
        .filter(Boolean)
        .join(" ")}
      onFocus={() => setRecording(true)}
      onBlur={() => setRecording(false)}
      onKeyDown={onKeyDown}
      onClick={() => ref.current?.focus()}
    >
      {segments.length > 0 ? (
        <span className={styles.shortcutKbd}>
          {segments.map((part, i) => (
            <Fragment key={i}>
              {i !== 0 ? " + " : ""}
              <kbd>{part}</kbd>
            </Fragment>
          ))}
        </span>
      ) : (
        <span className={styles.keyCapturePlaceholder}>
          {recording ? "Press keys…" : "Click and press keys"}
        </span>
      )}
      {value && (
        <button
          type="button"
          className={styles.keyCaptureClear}
          onMouseDown={(e) => {
            // mousedown so the parent doesn't lose focus first
            e.preventDefault();
            onChange("");
            ref.current?.focus();
          }}
          title="Clear"
        >
          ×
        </button>
      )}
    </div>
  );
}

type EditState = {
  commandId: string;
  key: string;
};

function KeybindingsEditor({
  commands,
  bindings,
  filter,
}: {
  commands: CommandInfo[];
  bindings: ResolvedBinding[];
  filter: string;
}) {
  const [edit, setEdit] = useState<EditState | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Tracks the (key, when) the user has explicitly acknowledged as a
  // conflict. Save remains gated until ack matches the current draft, and
  // changing the key invalidates the ack.
  const [acked, setAcked] = useState<{ key: string; when: string } | null>(
    null,
  );

  const commandsById = useMemo(() => {
    const m = new Map<string, CommandInfo>();
    for (const c of commands) m.set(c.id, c);
    return m;
  }, [commands]);

  const filtered = useMemo(() => {
    if (!filter) return commands;
    const lower = filter.toLowerCase();
    return commands.filter(
      (c) =>
        c.name.toLowerCase().includes(lower) ||
        c.id.toLowerCase().includes(lower) ||
        (c.shortcut && c.shortcut.toLowerCase().includes(lower)) ||
        (c.when && c.when.toLowerCase().includes(lower)),
    );
  }, [commands, filter]);

  const startEdit = (cmd: CommandInfo) => {
    setError(null);
    setAcked(null);
    setEdit({
      commandId: cmd.id,
      key: cmd.shortcut ?? "",
    });
  };

  const cancelEdit = () => {
    setEdit(null);
    setError(null);
    setAcked(null);
  };

  const save = async (cmd: CommandInfo) => {
    if (!edit) return;
    try {
      // The when clause is a property of the command, not the user's choice —
      // keep whatever the command currently uses (its default for built-ins).
      // We do NOT pre-clear conflicting bindings: the new binding wins by
      // resolution order and the loser's row visibly shows as shadowed. To
      // reclaim, the user can Reset either side — Reset is symmetric.
      await invoke("set_command_keybinding", {
        commandId: edit.commandId,
        newKey: edit.key || null,
        newWhen: cmd.default_when ?? cmd.when ?? null,
      });
      setEdit(null);
      setError(null);
    } catch (e: any) {
      setError(typeof e === "string" ? e : (e?.message ?? String(e)));
    }
  };

  const reset = async (cmd: CommandInfo) => {
    try {
      await invoke("reset_command_keybinding", { commandId: cmd.id });
    } catch (e) {
      console.error("Failed to reset keybinding:", e);
    }
  };

  return (
    <div className={styles.settingsList}>
      <table className={styles.keybindingsTable}>
        <thead>
          <tr>
            <th>Command</th>
            <th>Shortcut</th>
            <th>When</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {filtered.map((cmd) => {
            const isEditing = edit?.commandId === cmd.id;
            const candidateWhen = cmd.default_when ?? cmd.when ?? "";

            // Conflict / validation state — only computed in edit mode.
            const conflicts =
              isEditing && edit && edit.key
                ? detectConflicts(
                    edit.key,
                    candidateWhen,
                    edit.commandId,
                    bindings,
                    commandsById,
                  )
                : [];
            const hardConflicts = conflicts.filter((c) => c.kind === "hard");
            const softConflicts = conflicts.filter((c) => c.kind === "soft");
            const valid = !isEditing || !edit?.key || isCompleteKey(edit.key);
            const ackMatches =
              !!acked &&
              !!edit &&
              acked.key === edit.key &&
              acked.when === candidateWhen;
            const canSave = valid && (hardConflicts.length === 0 || ackMatches);
            const showBanner =
              isEditing &&
              (!valid ||
                hardConflicts.length > 0 ||
                softConflicts.length > 0 ||
                !!error);

            return (
              <Fragment key={cmd.id}>
                <tr
                  className={[
                    cmd.user_overridden ? styles.kbRowModified : "",
                    isEditing ? styles.kbRowEditing : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  onDoubleClick={() => !isEditing && startEdit(cmd)}
                >
                  <td>
                    {cmd.name}
                    {cmd.user_overridden && !isEditing && (
                      <span className={styles.kbModifiedDot} title="Modified">
                        •
                      </span>
                    )}
                  </td>
                  <td>
                    {isEditing && edit ? (
                      <KeyCaptureInput
                        value={edit.key}
                        onChange={(k) => {
                          setEdit({ ...edit, key: k });
                          setAcked(null);
                        }}
                        autoFocus
                      />
                    ) : cmd.shortcut_display.length > 0 ? (
                      shortcutChips(cmd.shortcut!)
                    ) : (
                      <span className={styles.noShortcut}>&mdash;</span>
                    )}
                  </td>
                  <td>
                    <span className={styles.whenLabel}>
                      {whenLabel(cmd.when ?? cmd.default_when)}
                    </span>
                  </td>
                  <td className={styles.kbRowActions}>
                    {!isEditing && (
                      <>
                        <button onClick={() => startEdit(cmd)}>Edit</button>
                        {cmd.user_overridden && (
                          <button
                            onClick={() => reset(cmd)}
                            title="Reset to default"
                          >
                            Reset
                          </button>
                        )}
                      </>
                    )}
                    {isEditing && edit && (
                      <>
                        <button
                          className="suggested"
                          onClick={() => save(cmd)}
                          disabled={!canSave}
                        >
                          Save
                        </button>
                        <button onClick={cancelEdit}>Cancel</button>
                        {cmd.default_key && (
                          <button
                            onClick={() => {
                              reset(cmd);
                              cancelEdit();
                            }}
                            disabled={
                              !cmd.user_overridden &&
                              edit.key === cmd.default_key
                            }
                            title="Restore the built-in default"
                          >
                            Reset
                          </button>
                        )}
                      </>
                    )}
                  </td>
                </tr>

                {showBanner && (
                  <tr className={styles.kbDetailRow}>
                    <td></td>
                    <td colSpan={3}>
                      {!valid && (
                        <div className={styles.kbBannerWarn}>
                          Press a non-modifier key (letter, number, function
                          key, etc.).
                        </div>
                      )}

                      {hardConflicts.length > 0 && (
                        <div className={styles.kbBannerError}>
                          <span>
                            Already used by{" "}
                            {hardConflicts
                              .map(
                                (c) =>
                                  `${c.commandName} (${whenLabel(c.binding.when)})`,
                              )
                              .join(", ")}
                            .
                          </span>
                          {!ackMatches && (
                            <button
                              onClick={() =>
                                setAcked({
                                  key: edit!.key,
                                  when: candidateWhen,
                                })
                              }
                              disabled={!valid}
                              title="Acknowledge the conflict — Save will then overwrite the existing binding"
                            >
                              Override
                            </button>
                          )}
                        </div>
                      )}

                      {showBanner &&
                        hardConflicts.length === 0 &&
                        softConflicts.length > 0 && (
                          <div className={styles.kbBannerWarn}>
                            Also used by{" "}
                            {softConflicts
                              .map(
                                (c) =>
                                  `${c.commandName} (${whenLabel(c.binding.when)})`,
                              )
                              .join(", ")}
                            . Whichever context applies will win.
                          </div>
                        )}

                      {error && (
                        <div className={styles.kbBannerError}>{error}</div>
                      )}
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
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
          <KeybindingsEditor
            commands={preferences?.commands ?? []}
            bindings={preferences?.bindings ?? []}
            filter={filter}
          />
        )}
        {activeTab === "commands" && (
          <CommandsEditor
            commands={preferences?.user_commands ?? []}
            bindings={preferences?.bindings ?? []}
            allCommands={preferences?.commands ?? []}
          />
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
