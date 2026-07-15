import { useState } from "react";
import { commands } from "../../lib/bindings";
import { safe, safeCommand } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  FieldGroup,
  CheckboxField,
} from "./primitives";

type CopyMoveProps = CommonDialogProps & ModalDataOf<"copy_move">;

export default function CopyMove({
  kind,
  sources,
  destination,
  display_destination,
  summary: itemSummary,
  cancel,
  context,
}: CopyMoveProps) {
  const [preserveTimestamps, setPreserveTimestamps] = useState(false);
  const [preserveOwner, setPreserveOwner] = useState(false);
  const [preserveGroup, setPreserveGroup] = useState(false);
  const [createSymlink, setCreateSymlink] = useState(false);

  const isCopy = kind === "copy";
  const title = isCopy ? "Copy" : "Move";
  const isSingleFile = sources.length === 1;

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();

    safe(
      commands.startCopyMove(kind, sources, destination, {
        preserve_timestamps: preserveTimestamps,
        preserve_owner: preserveOwner,
        preserve_group: preserveGroup,
        create_symlink: createSymlink,
      }),
    );
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader
        title={title}
        summary={
          <>
            {title} <b>{itemSummary}</b> to:
          </>
        }
      />
      <DialogBody>
        <input type="text" value={display_destination} readOnly size={50} />
        <FieldGroup>
          {isCopy && isSingleFile && (
            <CheckboxField
              label="Create symbolic link"
              checked={createSymlink}
              onChange={setCreateSymlink}
            />
          )}
          <CheckboxField
            label="Preserve timestamps"
            checked={preserveTimestamps}
            onChange={setPreserveTimestamps}
            disabled={createSymlink}
          />
          <CheckboxField
            label="Preserve owner"
            checked={preserveOwner}
            onChange={setPreserveOwner}
            disabled={createSymlink}
          />
          <CheckboxField
            label="Preserve group"
            checked={preserveGroup}
            onChange={setPreserveGroup}
            disabled={createSymlink}
          />
        </FieldGroup>
      </DialogBody>
      <DialogFooter
        onCancel={cancel}
        start={
          isCopy &&
          context?.pane_handle != null && (
            // Swaps this modal for the Pack to Archive dialog over the same
            // selection (the cmd_ middleware closes this one).
            <button
              type="button"
              onClick={() =>
                safeCommand("cmd_create_archive", {
                  paneHandle: context.pane_handle,
                })
              }
            >
              Pack into archive…
            </button>
          )
        }
      >
        <DialogSubmitButton autoFocus>{title}</DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
