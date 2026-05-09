import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";

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
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Rename file
        </Dialog.Title>
        <label htmlFor="path">
          New name for <b>{name}</b>
        </label>
        <input
          type="text"
          name="path"
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          size={40}
          ref={inputRef}
          autoFocus
        />
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button type="submit" className="suggested" disabled={!newName}>
          Rename
        </button>
      </div>
    </form>
  );
}
