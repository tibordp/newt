import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { commands } from "../../lib/bindings";
import { tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import { useAsyncAction } from "./useAsyncAction";
import { DialogError, DialogSubmitButton } from "./DialogActions";
import dialogStyles from "./Dialog.module.scss";

type MountK8sProps = CommonDialogProps & ModalDataOf<"mount_k8s">;

export default function MountK8s({
  k8s_context,
  cancel,
  context,
}: MountK8sProps) {
  const [k8sContext, setK8sContext] = useState(k8s_context);
  const inputRef = useRef<HTMLInputElement>(null);

  const { pending, error, run } = useAsyncAction(() =>
    tryRun(commands.mountK8s(context?.pane_handle ?? 0, k8sContext)),
  );

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
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
              disabled={pending}
            />
          </div>
          <DialogError error={error} />
        </div>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel} disabled={pending}>
          Cancel
        </button>
        <DialogSubmitButton pending={pending} pendingLabel="Connecting…">
          Mount
        </DialogSubmitButton>
      </div>
    </form>
  );
}
