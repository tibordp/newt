import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safe, safeSilent, tryRun } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import { useAsyncAction } from "./useAsyncAction";
import { DialogError, DialogSubmitButton } from "./DialogActions";
import dialogStyles from "./Dialog.module.scss";
import { commands } from "../../lib/bindings";

type ConnectRemoteProps = CommonDialogProps & {
  host: string;
};

export default function ConnectRemote({ host, cancel }: ConnectRemoteProps) {
  const [newHost, setNewHost] = useState(host);
  const [saveProfile, setSaveProfile] = useState(false);
  const [connectionName, setConnectionName] = useState(host);
  const inputRef = useRef<HTMLInputElement>(null);

  const { pending, error, run } = useAsyncAction(async () => {
    if (saveProfile && connectionName) {
      const id = connectionName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
      await safeSilent(
        commands.cmdSaveConnection(
          {
            id,
            name: connectionName,
            type: "remote",
            host: newHost,
          },
          null,
        ),
      );
    }
    const err = await tryRun(commands.connectRemote(newHost));
    if (err) return err;
    await safe(commands.closeModal());
    return null;
  });

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Connect to Remote Host
        </Dialog.Title>
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-4)",
          }}
        >
          <div>
            <label htmlFor="host">Host (e.g., user@host)</label>
            <input
              ref={inputRef}
              type="text"
              name="host"
              value={newHost}
              onChange={(e) => {
                setNewHost(e.target.value);
                if (!saveProfile) setConnectionName(e.target.value);
              }}
              size={40}
              autoFocus
              autoComplete="off"
              disabled={pending}
            />
          </div>
          <div>
            <label
              style={{
                display: "flex",
                alignItems: "center",
                gap: "var(--space-2)",
                fontSize: "0.9em",
              }}
            >
              <input
                type="checkbox"
                checked={saveProfile}
                onChange={(e) => setSaveProfile(e.target.checked)}
                disabled={pending}
              />
              Save as connection profile
            </label>
            {saveProfile && (
              <input
                type="text"
                value={connectionName}
                onChange={(e) => setConnectionName(e.target.value)}
                placeholder="Connection name"
                size={30}
                style={{ marginTop: "var(--space-2)" }}
                autoComplete="off"
                disabled={pending}
              />
            )}
          </div>
          <DialogError error={error} />
        </div>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel} disabled={pending}>
          Cancel
        </button>
        <DialogSubmitButton
          pending={pending}
          pendingLabel="Connecting…"
          disabled={!newHost}
        >
          Connect
        </DialogSubmitButton>
      </div>
    </form>
  );
}
