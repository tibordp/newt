import { Fragment, useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { PreferencesState } from "../../lib/preferences";
import styles from "./SettingsEditor.module.scss";
import { invoke } from "@tauri-apps/api/core";

type SettingsEditorProps = {
  open: boolean;
  preferences: PreferencesState | null;
  onClose: () => void;
  onCloseAutoFocus: (e: Event) => void;
};

type SettingDef = {
  key: string;
  title: string;
  description: string;
  category: string;
  type: "boolean" | "string" | "number";
  value: any;
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

function extractSettings(
  preferences: PreferencesState,
): SettingDef[] {
  const settings: SettingDef[] = [];
  const schema = preferences.schema;
  const values = preferences.settings;

  if (!schema?.properties) return settings;

  // Walk the schema properties (top-level = categories)
  for (const [category, rawCatSchema] of Object.entries(schema.properties) as [string, any][]) {
    const catSchema = resolveSchema(schema, rawCatSchema);
    if (catSchema?.type !== "object" || !catSchema.properties) continue;

    for (const [prop, propSchema] of Object.entries(catSchema.properties) as [string, any][]) {
      const key = `${category}.${prop}`;
      const title = propSchema.title || prop.replace(/_/g, " ");
      const description = propSchema.description || "";
      const type = propSchema.type === "boolean" ? "boolean"
        : propSchema.type === "integer" || propSchema.type === "number" ? "number"
        : "string";

      const value = (values as any)?.[category]?.[prop];

      settings.push({
        key,
        title,
        description,
        category,
        type,
        value,
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

type Tab = "settings" | "keybindings";

export default function SettingsEditor({
  open,
  preferences,
  onClose,
  onCloseAutoFocus,
}: SettingsEditorProps) {
  const [filter, setFilter] = useState("");
  const [activeCategory, setActiveCategory] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<Tab>("settings");

  const allSettings = useMemo(
    () => (preferences ? extractSettings(preferences) : []),
    [preferences],
  );

  const categories = useMemo(() => {
    const cats = new Set<string>();
    for (const s of allSettings) cats.add(s.category);
    return Array.from(cats);
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

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Content className={styles.content} onCloseAutoFocus={onCloseAutoFocus}>
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
              className={activeTab === "keybindings" ? styles.tabActive : styles.tab}
              onClick={() => setActiveTab("keybindings")}
            >
              Keybindings
            </button>
          </div>
          <div className={styles.body}>
            {activeTab === "settings" && (
              <>
                <div className={styles.sidebar}>
                  <div
                    className={activeCategory === null ? styles.sidebarItemActive : styles.sidebarItem}
                    onClick={() => setActiveCategory(null)}
                  >
                    All
                  </div>
                  {categories.map((cat) => (
                    <div
                      key={cat}
                      className={activeCategory === cat ? styles.sidebarItemActive : styles.sidebarItem}
                      onClick={() => setActiveCategory(cat)}
                    >
                      {cat}
                    </div>
                  ))}
                </div>
                <div className={styles.settingsList}>
                  {filteredSettings.length === 0 && (
                    <div style={{ color: "var(--color-fg-muted)", padding: "var(--space-4)" }}>
                      No settings found
                    </div>
                  )}
                  {filteredSettings.map((setting) => (
                    <div key={setting.key} className={styles.settingRow}>
                      <div className={styles.settingInfo}>
                        <div className={styles.settingLabel}>{setting.title}</div>
                        {setting.description && (
                          <div className={styles.settingDescription}>{setting.description}</div>
                        )}
                      </div>
                      <div className={styles.settingControl}>
                        <SettingControl setting={setting} onUpdate={onUpdate} />
                      </div>
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
                              {when ? when.replace(/_/g, " ").replace(/\b\w/g, c => c.toUpperCase()) : "Global"}
                            </span>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </div>
          <div className={styles.footer}>
            <button onClick={() => safeCommand("open_config_file")}>
              Open Settings File
            </button>
            <button onClick={onClose}>Close</button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
