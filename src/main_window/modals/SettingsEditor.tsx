import * as Dialog from "@radix-ui/react-dialog";
import { Fragment, useId, useMemo, useState } from "react";

import { safe, unwrap } from "../../lib/ipc";
import { PreferencesState } from "../../lib/preferences";
import { useCommandShortcuts } from "../../lib/scopedBindings";
import styles from "./SettingsEditor.module.scss";
import { AssociationsEditor } from "./settings/AssociationsEditor";
import { CommandsEditor } from "./settings/CommandsEditor";
import { KeybindingsEditor } from "./settings/KeybindingsEditor";
import { CustomWidget, SettingControl } from "./settings/SettingControls";
import { extractSettings } from "./settings/schema";
import {
  commands,
  type AssociationFocus,
  type PaneHandle,
} from "../../lib/bindings";
import {
  DialogTabs,
  dialogTabId,
  IconOpenExternal,
  IconRevealInPane,
} from "./primitives";

type Tab = "settings" | "keybindings" | "commands" | "associations";

const preventAutoFocus = (e: Event) => e.preventDefault();

export default function SettingsEditor({
  preferences,
  canReveal,
  association,
  paneHandle,
}: {
  preferences: PreferencesState | null;
  canReveal: boolean;
  /// Open on the Associations tab, at this row.
  association: AssociationFocus | null;
  paneHandle: PaneHandle | null;
}) {
  const [filter, setFilter] = useState("");
  const [activeCategory, setActiveCategory] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<Tab>(
    association ? "associations" : "settings",
  );
  const idBase = useId();
  const shortcuts = useCommandShortcuts();
  const categoryTabId = (key: string | null) =>
    `${idBase}-category-${key ?? "all"}`;

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
          onKeyDown={(e) => {
            // From a typed pattern straight into its row's cells.
            if (activeTab === "associations" && e.key === "ArrowDown") {
              const cell = document
                .getElementById(`${idBase}-panel`)
                ?.querySelector<HTMLElement>("[data-cell]");
              if (cell) {
                e.preventDefault();
                cell.focus();
              }
            }
          }}
          placeholder={
            activeTab === "associations"
              ? "Search patterns, or try a file name..."
              : "Search settings..."
          }
          aria-label={
            activeTab === "associations"
              ? "Search associations"
              : "Search settings"
          }
          // Change Association puts focus on its row instead.
          autoFocus={!association}
        />
      </div>
      <div className={styles.tabStrip}>
        <DialogTabs
          tabs={[
            { value: "settings", label: "Preferences" },
            { value: "keybindings", label: "Keybindings" },
            { value: "commands", label: "Commands" },
            { value: "associations", label: "Associations" },
          ]}
          value={activeTab}
          onChange={setActiveTab}
          stretch
          label="Settings sections"
          panelId={`${idBase}-panel`}
        />
      </div>
      <div
        className={styles.body}
        id={`${idBase}-panel`}
        role="tabpanel"
        aria-labelledby={dialogTabId(`${idBase}-panel`, activeTab)}
      >
        {activeTab === "settings" && (
          <>
            <div
              className={styles.sidebar}
              role="tablist"
              aria-orientation="vertical"
              aria-label="Categories"
            >
              <div
                id={categoryTabId(null)}
                role="tab"
                aria-selected={activeCategory === null}
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
                  id={categoryTabId(key)}
                  role="tab"
                  aria-selected={activeCategory === key}
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
            <div
              className={styles.settingsList}
              role="tabpanel"
              aria-labelledby={categoryTabId(activeCategory)}
            >
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
              {filteredSettings.map((setting, i) => {
                const labelId = `${idBase}-${setting.key}-label`;
                const descId = setting.description
                  ? `${idBase}-${setting.key}-desc`
                  : undefined;
                return (
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
                          <span id={labelId}>{setting.title}</span>
                          {/* Always render so the row's height stays
                            constant when modified flips on/off — visibility
                            rather than display preserves the slot. */}
                          <button
                            type="button"
                            className={styles.resetButton}
                            onClick={() => onReset(setting.key)}
                            title="Reset to default"
                            aria-label={`Reset ${setting.title}`}
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
                          <div
                            id={descId}
                            className={styles.settingDescription}
                          >
                            {setting.description}
                          </div>
                        )}
                      </div>
                      {setting.customWidget === "columns" ? (
                        <CustomWidget
                          setting={setting}
                          onUpdate={onUpdate}
                          labelledBy={labelId}
                          describedBy={descId}
                        />
                      ) : (
                        <div className={styles.settingControl}>
                          {setting.type === "custom" ? (
                            <CustomWidget
                              setting={setting}
                              onUpdate={onUpdate}
                              labelledBy={labelId}
                              describedBy={descId}
                            />
                          ) : (
                            <SettingControl
                              setting={setting}
                              onUpdate={onUpdate}
                              labelledBy={labelId}
                              describedBy={descId}
                            />
                          )}
                        </div>
                      )}
                    </div>
                  </Fragment>
                );
              })}
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
        {activeTab === "associations" && (
          <AssociationsEditor
            filter={filter}
            onClearFilter={() => setFilter("")}
            preferences={preferences}
            userCommands={preferences?.user_commands ?? []}
            focus={association}
          />
        )}
      </div>
      <div className={styles.footer}>
        <div className={styles.fileActions}>
          <span
            id={`${idBase}-file-actions`}
            className={styles.fileActionsLabel}
          >
            Settings file
          </span>
          <div
            className={styles.fileActionsGroup}
            role="group"
            aria-labelledby={`${idBase}-file-actions`}
          >
            {canReveal && paneHandle !== null && (
              <button
                type="button"
                className={styles.iconButton}
                onClick={() => safe(commands.revealConfigFile(paneHandle))}
                title="Reveal in pane"
                aria-label="Reveal in pane"
              >
                <IconRevealInPane />
              </button>
            )}
            <button
              type="button"
              className={styles.iconButton}
              onClick={() => safe(commands.openConfigFile())}
              title={shortcuts.label(
                "Open in external editor",
                "open_config_file",
              )}
              aria-label="Open in external editor"
            >
              <IconOpenExternal />
            </button>
          </div>
        </div>
        <button onClick={() => safe(commands.closeModal())}>Close</button>
      </div>
    </Dialog.Content>
  );
}
