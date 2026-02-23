import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";

type ConnectRemoteProps = CommonDialogProps & {
  host: string;
};

export default function ConnectRemote({ host, cancel }: ConnectRemoteProps) {
  const [newHost, setNewHost] = useState(host);
  const inputRef = useRef<HTMLInputElement>(null);

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    await safeCommand("connect_remote", { host: newHost });
    await safeCommand("close_modal");
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <form onSubmit={onSubmit}>
      <div className="dialog-contents">
        <Dialog.Title className="dialog-title">Connect to Remote Host</Dialog.Title>
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
      <div className="dialog-buttons">
        <button type="button" onClick={cancel}>Cancel</button>
        <button type="submit" className="suggested" disabled={!newHost}>Connect</button>
      </div>
    </form>
  );
}
