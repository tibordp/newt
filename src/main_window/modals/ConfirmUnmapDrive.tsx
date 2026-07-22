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
        Disconnect {drive}
        {target ? ` (${target})` : ""}?
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
