import { useEffect, useRef, useState } from "react";
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
    });
  }

  useEffect(() => {
    inputRef.current?.select();
  }, []);

  return (
    <>
      <form onSubmit={onSubmit}>
        <div className="dialog-contents">
          <h2>Navigate to</h2>
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
          <input type="submit" value="Navigate" disabled={!newPath} />
          <input type="button" value="Cancel" onClick={cancel} />
        </div>
      </form>
    </>
  );
}
