import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { commands } from "../../lib/bindings";
import { safe, safeCommand } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";
import styles from "./CopyMove.module.scss";

type CopyMoveProps = CommonDialogProps & ModalDataOf<"copy_move">;

export default function CopyMove({
  kind,
  sources,
  destination,
  display_destination,
  summary: itemSummary,
  cancel,
  context,
}: CopyMoveProps) {
  const [preserveTimestamps, setPreserveTimestamps] = useState(false);
  const [preserveOwner, setPreserveOwner] = useState(false);
  const [preserveGroup, setPreserveGroup] = useState(false);
  const [createSymlink, setCreateSymlink] = useState(false);

  const isCopy = kind === "copy";
  const title = isCopy ? "Copy" : "Move";
  const isSingleFile = sources.length === 1;

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();

    safe(
      commands.startCopyMove(kind, sources, destination, {
        preserve_timestamps: preserveTimestamps,
        preserve_owner: preserveOwner,
        preserve_group: preserveGroup,
        create_symlink: createSymlink,
      }),
    );
  }

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          {title}
        </Dialog.Title>
        <p className={dialogStyles.dialogSummary}>
          {title} <b>{itemSummary}</b> to:
        </p>
        <input type="text" value={display_destination} readOnly size={50} />
        <div className={styles.copyOptions}>
          {isCopy && isSingleFile && (
            <label className={styles.optionLabel}>
              <input
                type="checkbox"
                checked={createSymlink}
                onChange={(e) => setCreateSymlink(e.target.checked)}
              />
              Create symbolic link
            </label>
          )}
          <label className={styles.optionLabel}>
            <input
              type="checkbox"
              checked={preserveTimestamps}
              onChange={(e) => setPreserveTimestamps(e.target.checked)}
              disabled={createSymlink}
            />
            Preserve timestamps
          </label>
          <label className={styles.optionLabel}>
            <input
              type="checkbox"
              checked={preserveOwner}
              onChange={(e) => setPreserveOwner(e.target.checked)}
              disabled={createSymlink}
            />
            Preserve owner
          </label>
          <label className={styles.optionLabel}>
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
      <div className={dialogStyles.dialogButtons}>
        {isCopy && context?.pane_handle != null && (
          // Swaps this modal for the Pack to Archive dialog over the same
          // selection (the cmd_ middleware closes this one).
          <button
            type="button"
            style={{ marginRight: "auto" }}
            onClick={() =>
              safeCommand("cmd_create_archive", {
                paneHandle: context.pane_handle,
              })
            }
          >
            Pack into archive…
          </button>
        )}
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button type="submit" className="suggested" autoFocus>
          {title}
        </button>
      </div>
    </form>
  );
}
