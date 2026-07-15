import { useEffect, useRef, useState } from "react";
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

type RenameProps = CommonDialogProps & ModalDataOf<"rename">;

export default function Rename({
  base_path,
  name,
  cancel,
  context,
}: RenameProps) {
  const [newName, setNewName] = useState(name);
  const inputRef = useRef<HTMLInputElement>(null);

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    safe(
      commands.rename(context?.pane_handle ?? null, base_path, name, newName),
    );
  }

  useEffect(() => {
    inputRef.current?.select();
  }, []);

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Rename file" />
      <DialogBody>
        <Field
          label={
            <>
              New name for <b>{name}</b>
            </>
          }
          htmlFor="path"
        >
          <input
            type="text"
            id="path"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            size={40}
            ref={inputRef}
            autoFocus
          />
        </Field>
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        <DialogSubmitButton disabled={!newName}>Rename</DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
