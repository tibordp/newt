import * as Dialog from "@radix-ui/react-dialog";
import { safe } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";
import { commands } from "../../lib/bindings";

type ConfirmProps = CommonDialogProps & {
  message: string;
};

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
