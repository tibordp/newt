import { useEffect, useRef, useState } from "react";
import { commands } from "../../lib/bindings";
import { tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  DialogError,
  Field,
  useAsyncAction,
} from "./primitives";

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
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Mount Kubernetes" />
      <DialogBody>
        <Field label="Context (leave empty for current)" htmlFor="k8s_context">
          <input
            ref={inputRef}
            type="text"
            id="k8s_context"
            value={k8sContext}
            onChange={(e) => setK8sContext(e.target.value)}
            size={40}
            autoFocus
            autoComplete="off"
            placeholder="default: current context"
            disabled={pending}
          />
        </Field>
        <DialogError error={error} />
      </DialogBody>
      <DialogFooter onCancel={cancel} cancelDisabled={pending}>
        <DialogSubmitButton pending={pending} pendingLabel="Connecting…">
          Mount
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
