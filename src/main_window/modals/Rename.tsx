import { useEffect, useRef, useState } from "react";
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
    <>
      <form onSubmit={onSubmit}>
        <div className="dialog-contents">
          <h2>Rename file</h2>
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
          <input type="submit" value="Create" disabled={!newName} />
          <input type="button" value="Cancel" onClick={cancel} />
        </div>
      </form>
    </>
  );
}
