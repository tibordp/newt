import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";

type RenameProps = CommonDialogProps & {
  base_path: string;
  name: string;
};

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
    safeCommand("rename", {
      paneHandle: context?.pane_handle,
      basePath: base_path,
      oldName: name,
      newName: newName,
    });
  }

  useEffect(() => {
    inputRef.current?.select();
  }, []);

  return (
    <form onSubmit={onSubmit}>
      <div className="dialog-contents">
        <Dialog.Title className="dialog-title">Rename file</Dialog.Title>
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
      <div className="dialog-buttons">
        <button type="button" onClick={cancel}>Cancel</button>
        <button type="submit" className="suggested" disabled={!newName}>Rename</button>
      </div>
    </form>
  );
}
