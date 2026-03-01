import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import { VfsPath } from "../../lib/types";
import dialogStyles from "./Dialog.module.scss";

type CreateDirectoryProps = CommonDialogProps & {
  path: VfsPath;
};

export default function CreateDirectory({
  path,
  cancel,
  context,
}: CreateDirectoryProps) {
  const [name, setName] = useState("");

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    safeCommand("create_directory", {
      paneHandle: context?.pane_handle,
      path,
      name,
    });
  }

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Create Directory
        </Dialog.Title>
        <label htmlFor="path">Directory name</label>
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
