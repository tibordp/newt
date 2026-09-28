import styles from "./DialogTabs.module.scss";

// Id of the tab for `value`, for the panel's aria-labelledby.
export function dialogTabId(panelId: string, value: string) {
  return `${panelId}-tab-${value}`;
}

export function DialogTabs<T extends string>({
  tabs,
  value,
  onChange,
  stretch,
  disabled,
  label,
  panelId,
}: {
  tabs: readonly { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
  // Tabs share the full width equally (settings-editor style).
  stretch?: boolean;
  disabled?: boolean;
  // Accessible name of the tab list.
  label?: string;
  // Id of the element holding the selected tab's content.
  panelId?: string;
}) {
  return (
    <div
      role="tablist"
      aria-label={label}
      className={stretch ? styles.tabBarStretch : styles.tabBar}
    >
      {tabs.map((tab) => (
        <button
          key={tab.value}
          id={panelId ? dialogTabId(panelId, tab.value) : undefined}
          type="button"
          role="tab"
          aria-selected={tab.value === value}
          aria-controls={panelId}
          className={tab.value === value ? styles.tabActive : styles.tab}
          onClick={() => onChange(tab.value)}
          disabled={disabled}
        >
          {tab.label}
        </button>
      ))}
    </div>
  );
}
