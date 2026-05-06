import styles from "./DialogActions.module.scss";

type DialogSubmitButtonProps = {
  pending?: boolean;
  disabled?: boolean;
  pendingLabel?: string;
  children: React.ReactNode;
};

// Submit button that shows a spinner and swaps to `pendingLabel` while an
// async action is in flight. Stays disabled while pending so the user can't
// double-submit.
export function DialogSubmitButton({
  pending = false,
  disabled = false,
  pendingLabel,
  children,
}: DialogSubmitButtonProps) {
  return (
    <button
      type="submit"
      className="suggested"
      disabled={disabled || pending}
      aria-busy={pending}
    >
      {pending && <span className={styles.spinner} aria-hidden />}
      {pending && pendingLabel ? pendingLabel : children}
    </button>
  );
}

type DialogErrorProps = {
  error: string | null;
};

// Inline error banner for dialogs. Renders nothing when error is null, so it
// can sit unconditionally in the dialog body.
export function DialogError({ error }: DialogErrorProps) {
  if (!error) return null;
  return (
    <div className={styles.error} role="alert">
      {error}
    </div>
  );
}
