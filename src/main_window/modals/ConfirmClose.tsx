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
  terminals,
  stopping,
  cancel,
}: ConfirmCloseProps) {
  const parts = [
    running > 0 &&
      (running === 1
        ? "an operation is running"
        : `${running} operations are running`),
    terminals > 0 &&
      (terminals === 1
        ? "a terminal is open"
        : `${terminals} terminals are open`),
  ].filter(Boolean);
  const what = parts.join(" and ");
  const ends = running + terminals === 1 ? "it" : "them";
  return (
    <DialogShell>
      <DialogHeader title="Close Window" />
      <DialogBody>
        <Dialog.Description asChild>
          <span>
            {stopping
              ? "Stopping the running operations…"
              : `${what.charAt(0).toUpperCase()}${what.slice(1)}. Closing the window ${running > 0 ? "stops" : "ends"} ${ends}.`}
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
            {running > 0 ? "Stop and Close" : "Close"}
          </button>
        )}
      </DialogFooter>
    </DialogShell>
  );
}
