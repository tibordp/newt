import * as Dialog from "@radix-ui/react-dialog";
import { Fragment, useMemo, useState } from "react";

import { safe, unwrap } from "../../lib/ipc";
import { PreferencesState } from "../../lib/preferences";
import styles from "./SettingsEditor.module.scss";
import { CommandsEditor } from "./settings/CommandsEditor";
import { KeybindingsEditor } from "./settings/KeybindingsEditor";
import { CustomWidget, SettingControl } from "./settings/SettingControls";
import { extractSettings } from "./settings/schema";
import { commands } from "../../lib/bindings";
import { DialogTabs } from "./primitives";

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
      <div className={styles.tabStrip}>
        <DialogTabs
          tabs={[
            { value: "settings", label: "Preferences" },
            { value: "keybindings", label: "Keybindings" },
            { value: "commands", label: "Commands" },
          ]}
          value={activeTab}
          onChange={setActiveTab}
          stretch
        />
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
              {filteredSettings.map((setting, i) => (
                <Fragment key={setting.key}>
                  {/* With "All" selected the list spans every category, so
                      break it into labelled sections; settings arrive grouped
                      by category, so a header rides each category's first row. */}
                  {activeCategory === null &&
                    (i === 0 ||
                      filteredSettings[i - 1].category !==
                        setting.category) && (
                      <div className={styles.categoryHeader}>
                        {setting.categoryTitle}
                      </div>
                    )}
                  <div
                    className={
                      setting.customWidget === "columns"
                        ? styles.settingRowFull
                        : styles.settingRow
                    }
                  >
                    <div className={styles.settingInfo}>
                      <div className={styles.settingLabel}>
                        {setting.title}
                        {/* Always render so the row's height stays
                          constant when modified flips on/off — visibility
                          rather than display preserves the slot. */}
                        <button
                          type="button"
                          className={styles.resetButton}
                          onClick={() => onReset(setting.key)}
                          title="Reset to default"
                          style={
                            setting.modified
                              ? undefined
                              : { visibility: "hidden" }
                          }
                          tabIndex={setting.modified ? 0 : -1}
                          aria-hidden={!setting.modified}
                        >
                          Reset
                        </button>
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
                          <SettingControl
                            setting={setting}
                            onUpdate={onUpdate}
                          />
                        )}
                      </div>
                    )}
                  </div>
                </Fragment>
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
