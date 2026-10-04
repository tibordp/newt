import * as Dialog from "@radix-ui/react-dialog";
import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./primitives";

type ConfirmCloseProps = CommonDialogProps & ModalDataOf<"confirm_close">;

export default function ConfirmClose({
  running,
  stopping,
  cancel,
}: ConfirmCloseProps) {
  const what = running === 1 ? "An operation is" : `${running} operations are`;
  return (
    <DialogShell>
      <DialogHeader title="Close Window" />
      <DialogBody>
        <Dialog.Description asChild>
          <span>
            {stopping
              ? "Stopping the running operations…"
              : `${what} still running. Closing the window stops ${running === 1 ? "it" : "them"}.`}
          </span>
        </Dialog.Description>
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        {stopping ? (
          <button
            type="button"
            className="destructive"
            onClick={() => safe(commands.closeWithRunningOperations(true))}
            autoFocus
          >
            Close Now
          </button>
        ) : (
          <button
            type="button"
            className="destructive"
            onClick={() => safe(commands.closeWithRunningOperations(false))}
            autoFocus
          >
            Stop and Close
          </button>
        )}
      </DialogFooter>
    </DialogShell>
  );
}
