import { useState } from "react";
import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  Field,
} from "./primitives";

type CreateFileProps = CommonDialogProps & ModalDataOf<"create_file">;

export default function CreateFile({
  path,
  open_editor,
  cancel,
  context,
}: CreateFileProps) {
  const [name, setName] = useState("");

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    safe(
      commands.touchFile(context?.pane_handle ?? null, path, name, open_editor),
    );
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Create New File (Touch)" />
      <DialogBody>
        <Field label="File Name" htmlFor="path">
          <input
            type="text"
            id="path"
            value={name}
            onChange={(e) => setName(e.target.value)}
            size={40}
            autoFocus
          />
        </Field>
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        <DialogSubmitButton disabled={!name}>Create</DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
