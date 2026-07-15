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

type CreateDirectoryProps = CommonDialogProps & ModalDataOf<"create_directory">;

export default function CreateDirectory({
  path,
  cancel,
  context,
}: CreateDirectoryProps) {
  const [name, setName] = useState("");

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    safe(commands.createDirectory(context?.pane_handle ?? null, path, name));
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Create Directory" />
      <DialogBody>
        <Field label="Directory name" htmlFor="path">
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
