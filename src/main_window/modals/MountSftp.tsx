import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";

type MountSftpProps = CommonDialogProps & {
  host: string;
};

export default function MountSftp({ host, cancel, context }: MountSftpProps) {
  const [newHost, setNewHost] = useState(host);
  const inputRef = useRef<HTMLInputElement>(null);

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    await safeCommand("mount_sftp", {
      paneHandle: context?.pane_handle,
      host: newHost,
    });
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Mount SFTP
        </Dialog.Title>
        <label htmlFor="host">Host (e.g., user@host)</label>
        <input
          ref={inputRef}
          type="text"
          name="host"
          value={newHost}
          onChange={(e) => setNewHost(e.target.value)}
          size={40}
          autoFocus
        />
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button type="submit" className="suggested" disabled={!newHost}>
          Mount
        </button>
      </div>
    </form>
  );
}
