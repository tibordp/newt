import * as Dialog from "@radix-ui/react-dialog";
import { useMemo, useState } from "react";

import { safe, unwrap } from "../../lib/ipc";
import { PreferencesState } from "../../lib/preferences";
import styles from "./SettingsEditor.module.scss";
import { CommandsEditor } from "./settings/CommandsEditor";
import { KeybindingsEditor } from "./settings/KeybindingsEditor";
import { CustomWidget, SettingControl } from "./settings/SettingControls";
import { extractSettings } from "./settings/schema";
import { commands } from "../../lib/bindings";

type Tab = "settings" | "keybindings" | "commands";

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
      await unwrap(commands.updatePreference(key, value));
    } catch (e) {
      console.error("Failed to update preference:", e);
    }
  };

  const onReset = async (key: string) => {
    try {
      await unwrap(commands.resetPreference(key));
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
        <button onClick={() => safe(commands.openConfigFile())}>
          Open Settings File
        </button>
        <button onClick={() => safe(commands.closeModal())}>Close</button>
      </div>
    </Dialog.Content>
  );
}
