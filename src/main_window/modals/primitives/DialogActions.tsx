import { useCallback, useEffect, useRef, useState } from "react";
import styles from "./DialogActions.module.scss";

type DialogSubmitButtonProps = {
  pending?: boolean;
  disabled?: boolean;
  pendingLabel?: string;
  // "normal" demotes the button to plain styling (while another action —
  // e.g. Save in the focused profile-name row — holds the emphasis).
  variant?: "suggested" | "destructive" | "normal";
  autoFocus?: boolean;
  children: React.ReactNode;
};

// Submit button that shows a spinner and swaps to `pendingLabel` while an
// async action is in flight. Stays disabled while pending so the user can't
// double-submit.
export function DialogSubmitButton({
  pending = false,
  disabled = false,
  pendingLabel,
  variant = "suggested",
  autoFocus,
  children,
}: DialogSubmitButtonProps) {
  return (
    <button
      type="submit"
      className={variant === "normal" ? undefined : variant}
      disabled={disabled || pending}
      aria-busy={pending}
      autoFocus={autoFocus}
    >
      {pending && <span className={styles.spinner} aria-hidden />}
      {pending && pendingLabel ? pendingLabel : children}
    </button>
  );
}

// Transient "Saved" acknowledgement for a save-in-place action that keeps
// the dialog open. Returns the flag and a trigger that raises it briefly.
export function useSaveFlash(): [boolean, () => void] {
  const [saved, setSaved] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const flash = useCallback(() => {
    setSaved(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setSaved(false), 1500);
  }, []);
  return [saved, flash];
}

type DialogSaveButtonProps = {
  pending?: boolean;
  saved?: boolean;
  disabled?: boolean;
  /// "Save…" while the profile-name row is still hidden (the press reveals
  /// it), plain "Save" once it's visible.
  label?: string;
  // "suggested" while the profile-name row is focused (Enter means Save).
  variant?: "suggested" | "normal";
  onClick: () => void;
};

// Secondary "Save" action for connect/mount dialogs: persists the form as a
// connection profile without connecting. Pair with useSaveFlash.
export function DialogSaveButton({
  pending = false,
  saved = false,
  disabled = false,
  label = "Save",
  variant = "normal",
  onClick,
}: DialogSaveButtonProps) {
  return (
    <button
      type="button"
      className={variant === "suggested" ? "suggested" : undefined}
      onClick={onClick}
      disabled={disabled || pending}
      aria-busy={pending}
      title="Save as connection profile (Mod+S)"
    >
      {saved ? "Saved" : label}
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
    // pre-wrap (in the module): mount failures carry a multi-line log.
    <div className={styles.error} role="alert">
      {error}
    </div>
  );
}
