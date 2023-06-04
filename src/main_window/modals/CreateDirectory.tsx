import { useState } from "react";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";

type CreateDirectoryProps = CommonDialogProps & {
  path: string;
};

export default function CreateDirectory({
  path,
  cancel,
  context,
}: CreateDirectoryProps) {
  const [name, setName] = useState("");

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    console.log(context);
    safeCommand("create_directory", {
      paneHandle: context?.pane_handle,
      path,
      name,
    });
  }

  return (
    <>
      <form onSubmit={onSubmit}>
        <div className="dialog-contents">
          <h2>Create Directory</h2>
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
        <div className="dialog-buttons">
          <input type="submit" value="Create" disabled={!name} />
          <input type="button" value="Cancel" onClick={cancel} />
        </div>
      </form>
    </>
  );
}
