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
      onKeyDown={(e) => {
        // Left/Right (wrapping) and Home/End move to a tab and select it,
        // while a tab has focus.
        if (
          disabled ||
          (e.target as HTMLElement).getAttribute("role") !== "tab"
        )
          return;
        const at = tabs.findIndex((tab) => tab.value === value);
        const next =
          e.key === "ArrowRight"
            ? (at + 1) % tabs.length
            : e.key === "ArrowLeft"
              ? (at - 1 + tabs.length) % tabs.length
              : e.key === "Home"
                ? 0
                : e.key === "End"
                  ? tabs.length - 1
                  : null;
        if (next === null) return;
        e.preventDefault();
        onChange(tabs[next].value);
        e.currentTarget
          .querySelectorAll<HTMLElement>('[role="tab"]')
          [next]?.focus();
      }}
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
