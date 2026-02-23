import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";

type CreateFileProps = CommonDialogProps & {
  path: string;
};

export default function CreateFile({
  path,
  cancel,
  context,
}: CreateFileProps) {
  const [name, setName] = useState("");

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    safeCommand("touch_file", {
      paneHandle: context?.pane_handle,
      path,
      name,
    });
  }

  return (
    <form onSubmit={onSubmit}>
      <div className="dialog-contents">
        <Dialog.Title className="dialog-title">Create New File (Touch)</Dialog.Title>
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
      <div className="dialog-buttons">
        <button type="button" onClick={cancel}>Cancel</button>
        <button type="submit" className="suggested" disabled={!name}>Create</button>
      </div>
    </form>
  );
}
