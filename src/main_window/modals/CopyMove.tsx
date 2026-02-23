import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";

type CopyMoveProps = CommonDialogProps & {
  kind: string;
  sources: string[];
  destination: string;
};

export default function CopyMove({
  kind,
  sources,
  destination: initialDestination,
  cancel,
}: CopyMoveProps) {
  const [destination, setDestination] = useState(initialDestination);
  const [preserveTimestamps, setPreserveTimestamps] = useState(false);
  const [preserveOwner, setPreserveOwner] = useState(false);
  const [preserveGroup, setPreserveGroup] = useState(false);
  const [createSymlink, setCreateSymlink] = useState(false);

  const isCopy = kind === "copy";
  const title = isCopy ? "Copy" : "Move";
  const isSingleFile = sources.length === 1;

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();

    const options = {
      preserve_timestamps: preserveTimestamps,
      preserve_owner: preserveOwner,
      preserve_group: preserveGroup,
      create_symlink: createSymlink,
    };

    const request = isCopy
      ? { Copy: { sources, destination, options } }
      : { Move: { sources, destination, options } };

    safeCommand("start_operation", { request });
    safeCommand("close_modal");
  }

  const itemCount = sources.length;
  const summary =
    itemCount === 1
      ? sources[0].split("/").pop()
      : `${itemCount} items`;

  return (
    <form onSubmit={onSubmit}>
      <div className="dialog-contents">
        <Dialog.Title className="dialog-title">
          {title}
        </Dialog.Title>
        <p className="dialog-summary">
          {title} <b>{summary}</b> to:
        </p>
        <input
          type="text"
          value={destination}
          onChange={(e) => setDestination(e.target.value)}
          size={50}
          autoFocus
        />
        <div className="copy-options">
          {isCopy && isSingleFile && (
            <label className="option-label">
              <input
                type="checkbox"
                checked={createSymlink}
                onChange={(e) => setCreateSymlink(e.target.checked)}
              />
              Create symbolic link
            </label>
          )}
          <label className="option-label">
            <input
              type="checkbox"
              checked={preserveTimestamps}
              onChange={(e) => setPreserveTimestamps(e.target.checked)}
              disabled={createSymlink}
            />
            Preserve timestamps
          </label>
          <label className="option-label">
            <input
              type="checkbox"
              checked={preserveOwner}
              onChange={(e) => setPreserveOwner(e.target.checked)}
              disabled={createSymlink}
            />
            Preserve owner
          </label>
          <label className="option-label">
            <input
              type="checkbox"
              checked={preserveGroup}
              onChange={(e) => setPreserveGroup(e.target.checked)}
              disabled={createSymlink}
            />
            Preserve group
          </label>
        </div>
      </div>
      <div className="dialog-buttons">
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button type="submit" className="suggested" disabled={!destination}>
          {title}
        </button>
      </div>
    </form>
  );
}
