import styles from "./DialogTabs.module.scss";

export function DialogTabs<T extends string>({
  tabs,
  value,
  onChange,
  stretch,
}: {
  tabs: readonly { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
  // Tabs share the full width equally (settings-editor style).
  stretch?: boolean;
}) {
  return (
    <div
      role="tablist"
      className={stretch ? styles.tabBarStretch : styles.tabBar}
    >
      {tabs.map((tab) => (
        <button
          key={tab.value}
          type="button"
          role="tab"
          aria-selected={tab.value === value}
          className={tab.value === value ? styles.tabActive : styles.tab}
          onClick={() => onChange(tab.value)}
        >
          {tab.label}
        </button>
      ))}
    </div>
  );
}
