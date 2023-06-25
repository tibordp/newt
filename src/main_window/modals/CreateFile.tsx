import { useState } from "react";
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
    <>
      <form onSubmit={onSubmit}>
        <div className="dialog-contents">
          <h2>Create New File (Touch)</h2>
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
          <input type="submit" value="Create" disabled={!name} />
          <input type="button" value="Cancel" onClick={cancel} />
        </div>
      </form>
    </>
  );
}
