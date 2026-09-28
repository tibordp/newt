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

type ConfirmUnmapDriveProps = CommonDialogProps &
  ModalDataOf<"confirm_unmap_drive">;

export default function ConfirmUnmapDrive({
  drive,
  target,
  cancel,
}: ConfirmUnmapDriveProps) {
  return (
    <DialogShell>
      <DialogHeader title="Unmap Network Drive" />
      <DialogBody>
        <Dialog.Description asChild>
          <span>
            Disconnect {drive}
            {target ? ` (${target})` : ""}?
          </span>
        </Dialog.Description>
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        <button
          type="button"
          className="destructive"
          onClick={() => safe(commands.confirmUnmapDrive())}
          autoFocus
        >
          Disconnect
        </button>
      </DialogFooter>
    </DialogShell>
  );
}
