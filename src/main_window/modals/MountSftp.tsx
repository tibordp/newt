import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { invoke } from "@tauri-apps/api/core";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";

type MountSftpProps = CommonDialogProps & {
  host: string;
};

export default function MountSftp({ host, cancel, context }: MountSftpProps) {
  const [newHost, setNewHost] = useState(host);
  const [saveProfile, setSaveProfile] = useState(false);
  const [connectionName, setConnectionName] = useState(host);
  const inputRef = useRef<HTMLInputElement>(null);

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();

    if (saveProfile && connectionName) {
      try {
        const id = connectionName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
        await invoke("cmd_save_connection", {
          profile: {
            id,
            name: connectionName,
            type: "sftp",
            host: newHost,
          },
          secret: null,
        });
      } catch (err) {
        console.error("Failed to save connection profile:", err);
      }
    }

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
              />
            )}
          </div>
        </div>
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
