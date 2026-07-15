import { useEffect, useRef } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safe } from "../../lib/ipc";
import type { MainWindowState } from "../types";
import styles from "./ConnectionLogDialog.module.scss";
import { commands } from "../../lib/bindings";
import { DialogShell, DialogHeader, DialogFooter } from "./primitives";

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
      className={styles.content}
      onCloseAutoFocus={preventAutoFocus}
    >
      <DialogShell>
        <DialogHeader title="Connection Log" />
        <pre className={styles.log} ref={ref}>
          {log.length > 0 ? log.join("\n") : "(no log entries)"}
        </pre>
        <DialogFooter>
          <button
            type="button"
            onClick={() => safe(commands.closeModal())}
            autoFocus
          >
            Close
          </button>
        </DialogFooter>
      </DialogShell>
    </Dialog.Content>
  );
}
