import * as Dialog from "@radix-ui/react-dialog";
import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";

type ConfirmProps = CommonDialogProps & ModalDataOf<"confirm">;

export default function Confirm({ message, cancel }: ConfirmProps) {
  function onConfirm() {
    safe(commands.confirmAction());
  }

  return (
    <div>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Confirm
        </Dialog.Title>
        <p className={dialogStyles.dialogSummary}>{message}</p>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button
          type="button"
          className="suggested"
          onClick={onConfirm}
          autoFocus
        >
          OK
        </button>
      </div>
    </div>
  );
}
