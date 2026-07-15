import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./primitives";

type ConfirmProps = CommonDialogProps & ModalDataOf<"confirm">;

export default function Confirm({ message, action, cancel }: ConfirmProps) {
  const isDestructive = action.type === "delete_selected";

  function onConfirm() {
    safe(commands.confirmAction());
  }

  return (
    <DialogShell>
      <DialogHeader title={isDestructive ? "Delete" : "Confirm"} />
      <DialogBody>{message}</DialogBody>
      <DialogFooter onCancel={cancel}>
        <button
          type="button"
          className={isDestructive ? "destructive" : "suggested"}
          onClick={onConfirm}
          autoFocus
        >
          {isDestructive ? "Delete" : "OK"}
        </button>
      </DialogFooter>
    </DialogShell>
  );
}
