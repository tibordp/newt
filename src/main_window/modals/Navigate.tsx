import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";

type NavigateProps = CommonDialogProps & {
  path: string;
};

export default function Navigate({ path, cancel, context }: NavigateProps) {
  const [newPath, setNewPath] = useState(path);
  const inputRef = useRef<HTMLInputElement>(null);

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    safeCommand("navigate", {
      paneHandle: context?.pane_handle,
      path: newPath,
      exact: false
    });
  }

  useEffect(() => {
    inputRef.current?.select();
  }, []);

  return (
    <form onSubmit={onSubmit}>
      <div className="dialog-contents">
        <Dialog.Title className="dialog-title">Navigate to</Dialog.Title>
        <label htmlFor="path">Path</label>
        <input
          ref={inputRef}
          type="text"
          name="path"
          value={newPath}
          onChange={(e) => setNewPath(e.target.value)}
          size={40}
          autoFocus
        />
      </div>
      <div className="dialog-buttons">
        <button type="button" onClick={cancel}>Cancel</button>
        <button type="submit" className="suggested" disabled={!newPath}>Navigate</button>
      </div>
    </form>
  );
}
