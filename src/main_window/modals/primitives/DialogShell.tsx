import * as Dialog from "@radix-ui/react-dialog";
import styles from "./DialogShell.module.scss";

// Structural skeleton for dialog content: a flex column of
// DialogHeader / DialogBody / DialogFooter where the body scrolls and the
// header/footer stay pinned when content exceeds the dialog's max height.

export function DialogShell({
  onSubmit,
  children,
}: {
  // Renders a <form> when given, a plain <div> otherwise.
  onSubmit?: React.FormEventHandler<HTMLFormElement>;
  children: React.ReactNode;
}) {
  if (onSubmit) {
    return (
      <form className={styles.shell} onSubmit={onSubmit}>
        {children}
      </form>
    );
  }
  return <div className={styles.shell}>{children}</div>;
}

export function DialogHeader({
  title,
  summary,
  srOnlyTitle,
}: {
  title: React.ReactNode;
  summary?: React.ReactNode;
  // Radix requires a Dialog.Title for a11y even when the design has no
  // visible one.
  srOnlyTitle?: boolean;
}) {
  if (srOnlyTitle) {
    return <Dialog.Title className="sr-only">{title}</Dialog.Title>;
  }
  return (
    <header className={styles.header}>
      <Dialog.Title className={styles.title}>{title}</Dialog.Title>
      {summary != null && <p className={styles.summary}>{summary}</p>}
    </header>
  );
}

export function DialogBody({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={className ? `${styles.body} ${className}` : styles.body}>
      {children}
    </div>
  );
}

export function DialogFooter({
  start,
  onCancel,
  cancelLabel = "Cancel",
  cancelDisabled,
  children,
}: {
  // Left-aligned slot (secondary actions, scoped options).
  start?: React.ReactNode;
  onCancel?: () => void;
  cancelLabel?: string;
  cancelDisabled?: boolean;
  children?: React.ReactNode;
}) {
  return (
    <footer className={styles.footer}>
      {start != null && <div className={styles.footerStart}>{start}</div>}
      {onCancel && (
        <button type="button" onClick={onCancel} disabled={cancelDisabled}>
          {cancelLabel}
        </button>
      )}
      {children}
    </footer>
  );
}
