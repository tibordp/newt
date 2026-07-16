import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./primitives";

type ConfirmDeleteProps = CommonDialogProps & ModalDataOf<"confirm_delete">;

export default function ConfirmDelete({
  message,
  mode,
  cancel,
}: ConfirmDeleteProps) {
  function onConfirm(toTrash: boolean) {
    safe(commands.confirmDelete(toTrash));
  }

  return (
    <DialogShell>
      <DialogHeader title="Delete" />
      <DialogBody>{message}</DialogBody>
      <DialogFooter onCancel={cancel}>
        {mode === "trash" ? (
          <>
            <button
              type="button"
              className="destructive"
              onClick={() => onConfirm(false)}
            >
              Delete Permanently
            </button>
            <button
              type="button"
              className="suggested"
              onClick={() => onConfirm(true)}
              autoFocus
            >
              Move to Trash
            </button>
          </>
        ) : (
          <button
            type="button"
            className="destructive"
            onClick={() => onConfirm(false)}
            autoFocus
          >
            {mode === "trash_unavailable" ? "Delete Permanently" : "Delete"}
          </button>
        )}
      </DialogFooter>
    </DialogShell>
  );
}
