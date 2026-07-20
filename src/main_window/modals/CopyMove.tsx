import { useState } from "react";
import { commands } from "../../lib/bindings";
import { safe, safeCommand } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  FieldGroup,
  CheckboxField,
} from "./primitives";
import styles from "./CopyMove.module.scss";

type CopyMoveProps = CommonDialogProps & ModalDataOf<"copy_move">;

export default function CopyMove({
  kind,
  sources,
  destination,
  display_destination,
  summary: itemSummary,
  default_name,
  cancel,
  context,
}: CopyMoveProps) {
  const [preserveTimestamps, setPreserveTimestamps] = useState(false);
  const [preserveOwner, setPreserveOwner] = useState(false);
  const [preserveGroup, setPreserveGroup] = useState(false);
  const [createSymlink, setCreateSymlink] = useState(false);
  const [name, setName] = useState(default_name ?? "");

  const isCopy = kind === "copy";
  const title = isCopy ? "Copy" : "Move";
  const isSingleFile = sources.length === 1;
  const nameInvalid =
    default_name != null && (name.trim() === "" || /[/\\]/.test(name));

  function selectStem(e: React.FocusEvent<HTMLInputElement>) {
    const dot = name.lastIndexOf(".");
    e.currentTarget.setSelectionRange(0, dot > 0 ? dot : name.length);
  }

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (nameInvalid) return;

    const renameTo =
      default_name != null && name !== default_name ? name : null;
    safe(
      commands.startCopyMove(
        kind,
        sources,
        destination,
        {
          preserve_timestamps: preserveTimestamps,
          preserve_owner: preserveOwner,
          preserve_group: preserveGroup,
          create_symlink: createSymlink,
        },
        renameTo,
      ),
    );
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title={title} />
      <DialogBody>
        <p className={styles.hint}>
          {title} <b>{itemSummary}</b> {default_name != null ? "as:" : "into:"}
        </p>
        {default_name != null && (
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onFocus={selectStem}
            autoFocus
            size={50}
          />
        )}
        <p className={styles.hint}>
          {default_name != null ? (
            <>
              in <b>{display_destination}</b>
            </>
          ) : (
            <b>{display_destination}</b>
          )}
        </p>
        <FieldGroup>
          {isCopy && isSingleFile && (
            <CheckboxField
              label="Create symbolic link"
              checked={createSymlink}
              onChange={setCreateSymlink}
            />
          )}
          <CheckboxField
            label="Preserve timestamps"
            checked={preserveTimestamps}
            onChange={setPreserveTimestamps}
            disabled={createSymlink}
          />
          <CheckboxField
            label="Preserve owner"
            checked={preserveOwner}
            onChange={setPreserveOwner}
            disabled={createSymlink}
          />
          <CheckboxField
            label="Preserve group"
            checked={preserveGroup}
            onChange={setPreserveGroup}
            disabled={createSymlink}
          />
        </FieldGroup>
      </DialogBody>
      <DialogFooter
        onCancel={cancel}
        start={
          isCopy &&
          context?.pane_handle != null && (
            // Swaps this modal for the Pack to Archive dialog over the same
            // selection (the cmd_ middleware closes this one).
            <button
              type="button"
              onClick={() =>
                safeCommand("cmd_create_archive", {
                  paneHandle: context.pane_handle,
                })
              }
            >
              Pack into archive…
            </button>
          )
        }
      >
        <DialogSubmitButton
          autoFocus={default_name == null}
          disabled={nameInvalid}
        >
          {title}
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
