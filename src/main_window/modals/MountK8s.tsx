import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";

type MountK8sProps = CommonDialogProps & {
  k8s_context: string;
};

export default function MountK8s({
  k8s_context,
  cancel,
  context,
}: MountK8sProps) {
  const [k8sContext, setK8sContext] = useState(k8s_context);
  const inputRef = useRef<HTMLInputElement>(null);

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    await safeCommand("mount_k8s", {
      paneHandle: context?.pane_handle,
      context: k8sContext,
    });
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Mount Kubernetes
        </Dialog.Title>
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-4)",
          }}
        >
          <div>
            <label htmlFor="k8s_context">
              Context (leave empty for current)
            </label>
            <input
              ref={inputRef}
              type="text"
              name="k8s_context"
              value={k8sContext}
              onChange={(e) => setK8sContext(e.target.value)}
              size={40}
              autoFocus
              autoComplete="off"
              placeholder="default: current context"
            />
          </div>
        </div>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button type="submit" className="suggested">
          Mount
        </button>
      </div>
    </form>
  );
}
