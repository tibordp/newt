import styles from "./ProfileNameField.module.scss";

/// The name a connection dialog's Save action persists under. Hidden until
/// save-intent (first press of Save… / Mod+S) unless provenance pins it
/// open (editing a saved profile). Enter saves — the field's purpose — and
/// Escape backs out of an intent-triggered reveal instead of closing the
/// dialog.
export function ProfileNameField({
  value,
  onChange,
  visible,
  onSave,
  onDismiss,
  onFocusChange,
  disabled,
  inputRef,
}: {
  value: string;
  onChange: (value: string) => void;
  visible: boolean;
  onSave: () => void;
  /// Absent when provenance pins the field open (Escape then bubbles to
  /// the dialog as usual).
  onDismiss?: () => void;
  /// While the field is focused Enter means Save; dialogs use this to move
  /// the primary-button emphasis from Connect to Save.
  onFocusChange?: (focused: boolean) => void;
  disabled?: boolean;
  inputRef?: React.RefObject<HTMLInputElement | null>;
}) {
  if (!visible) return null;
  return (
    <div className={styles.profileRow}>
      <label className={styles.profileLabel} htmlFor="profile-name">
        Save as profile
      </label>
      <input
        id="profile-name"
        ref={inputRef}
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            onSave();
          } else if (e.key === "Escape" && onDismiss) {
            e.preventDefault();
            e.stopPropagation();
            onDismiss();
          }
        }}
        onFocus={() => onFocusChange?.(true)}
        onBlur={() => onFocusChange?.(false)}
        placeholder="Profile name"
        autoComplete="off"
        disabled={disabled}
      />
    </div>
  );
}
