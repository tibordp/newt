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

export function CheckboxField({
  label,
  checked,
  onChange,
  disabled,
  hint,
}: {
  label: React.ReactNode;
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  hint?: React.ReactNode;
}) {
  return (
    <div>
      <label className={styles.checkboxField}>
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
