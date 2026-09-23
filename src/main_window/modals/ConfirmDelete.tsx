import { useState } from "react";
import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  FieldFold,
  FieldGroup,
  CheckboxField,
} from "./primitives";
import styles from "./ConfirmDelete.module.scss";

type ConfirmDeleteProps = CommonDialogProps & ModalDataOf<"confirm_delete">;

export default function ConfirmDelete({
  message,
  mode,
  cancel,
}: ConfirmDeleteProps) {
  // Deliberately forgotten between dialogs: crossing into a mounted
  // filesystem is a per-delete decision.
  const [crossMountPoints, setCrossMountPoints] = useState(false);
  function onConfirm(toTrash: boolean) {
    safe(commands.confirmDelete(toTrash, crossMountPoints));
  }

  return (
    <DialogShell>
      <DialogHeader title="Delete" />
      <DialogBody className={styles.body}>
        {message}
        {mode !== "trash" && (
          <FieldFold summary="More options">
            <FieldGroup>
              <CheckboxField
                label="Descend into mount points"
                title="Also delete the contents of filesystems mounted under the selection. The mount points themselves stay."
                checked={crossMountPoints}
                onChange={setCrossMountPoints}
              />
            </FieldGroup>
          </FieldFold>
        )}
      </DialogBody>
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
