import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import { VfsPath } from "../../lib/types";
import dialogStyles from "./Dialog.module.scss";

type NavigateProps = CommonDialogProps & {
  path: VfsPath;
  display_path: string;
};

export default function Navigate({ path, display_path, cancel, context }: NavigateProps) {
  const [newPath, setNewPath] = useState(display_path);
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
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>Navigate to</Dialog.Title>
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
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>Cancel</button>
        <button type="submit" className="suggested" disabled={!newPath}>Navigate</button>
      </div>
    </form>
  );
}
