import styles from "./Field.module.scss";

// Stacked label-above-control field.
export function Field({
  label,
  htmlFor,
  hint,
  children,
}: {
  label: React.ReactNode;
  htmlFor?: string;
  hint?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className={styles.field}>
      <label className={styles.fieldLabel} htmlFor={htmlFor}>
        {label}
      </label>
      {children}
      {hint != null && <div className={styles.hint}>{hint}</div>}
    </div>
  );
}

// Tight vertical cluster for related options (checkbox groups).
export function FieldGroup({ children }: { children: React.ReactNode }) {
  return <div className={styles.group}>{children}</div>;
}

// Collapsed section for the options most runs leave alone.
export function FieldFold({
  summary,
  open,
  onToggle,
  children,
}: {
  summary: React.ReactNode;
  open?: boolean;
  onToggle?: (open: boolean) => void;
  children: React.ReactNode;
}) {
  return (
    <details
      className={styles.fold}
      open={open}
      onToggle={(e) => onToggle?.(e.currentTarget.open)}
    >
      <summary>{summary}</summary>
      {children}
    </details>
  );
}

export function CheckboxField({
  label,
  checked,
  onChange,
  disabled,
  hint,
  title,
}: {
  label: React.ReactNode;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  hint?: React.ReactNode;
  // Hover explainer; the tooltip idiom for text that would otherwise
  // take a line of its own.
  title?: string;
}) {
  return (
    <div>
      <label className={styles.checkboxField} title={title}>
        <input
          type="checkbox"
          checked={checked}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked)}
        />
        {label}
      </label>
      {hint != null && <div className={styles.checkboxHint}>{hint}</div>}
    </div>
  );
}

// Inline label + control on one row (compact selects, level spinners).
export function FieldRow({
  label,
  children,
}: {
  label: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <label className={styles.fieldRow}>
      <span className={styles.fieldRowLabel}>{label}</span>
      {children}
    </label>
  );
}
