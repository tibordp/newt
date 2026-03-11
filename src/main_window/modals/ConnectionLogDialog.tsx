import { useEffect, useRef } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import type { MainWindowState } from "../types";
import dialogStyles from "./Dialog.module.scss";
import styles from "./ConnectionLogDialog.module.scss";

const preventAutoFocus = (e: Event) => e.preventDefault();

export default function ConnectionLogContent({
  state,
}: {
  state: MainWindowState | null;
}) {
  const ref = useRef<HTMLPreElement>(null);
  const log = state?.connection_status?.log ?? [];

  useEffect(() => {
    if (ref.current) {
      ref.current.scrollTop = ref.current.scrollHeight;
    }
  }, [log.length]);

  return (
    <Dialog.Content
      className={dialogStyles.dialogContent}
      onCloseAutoFocus={preventAutoFocus}
      style={{
        width: 700,
        maxWidth: "90%",
        height: 500,
        maxHeight: "85%",
        display: "flex",
        flexDirection: "column",
      }}
    >
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Connection Log
        </Dialog.Title>
      </div>
      <pre className={styles.log} ref={ref}>
        {log.length > 0 ? log.join("\n") : "(no log entries)"}
      </pre>
      <div className={dialogStyles.dialogButtons}>
        <button
          type="button"
          onClick={() => safeCommand("close_modal")}
          autoFocus
        >
          Close
        </button>
      </div>
    </Dialog.Content>
  );
}
