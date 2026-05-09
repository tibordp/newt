import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safe } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import { VfsPath } from "../../lib/types";
import dialogStyles from "./Dialog.module.scss";
import { commands } from "../../lib/bindings";

type CreateFileProps = CommonDialogProps & {
  path: VfsPath;
  open_editor: boolean;
};

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
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Create New File (Touch)
        </Dialog.Title>
        <label htmlFor="path">File Name</label>
        <input
          type="text"
          name="path"
          value={name}
          onChange={(e) => setName(e.target.value)}
          size={40}
          autoFocus
        />
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button type="submit" className="suggested" disabled={!name}>
          Create
        </button>
      </div>
    </form>
  );
}
