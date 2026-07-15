import { useEffect, useRef, useState } from "react";
import { commands } from "../../lib/bindings";
import { safeSilent, tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  DialogError,
  Field,
  FieldGroup,
  CheckboxField,
  useAsyncAction,
} from "./primitives";

type MountSftpProps = CommonDialogProps & ModalDataOf<"mount_sftp">;

export default function MountSftp({ host, cancel, context }: MountSftpProps) {
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
            type: "sftp",
            host: newHost,
          },
          null,
        ),
      );
    }
    return tryRun(commands.mountSftp(context?.pane_handle ?? 0, newHost));
  });

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Mount SFTP" />
      <DialogBody>
        <Field label="Host (e.g., user@host)" htmlFor="host">
          <input
            ref={inputRef}
            type="text"
            id="host"
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
        </Field>
        <FieldGroup>
          <CheckboxField
            label="Save as connection profile"
            checked={saveProfile}
            onChange={setSaveProfile}
            disabled={pending}
          />
          {saveProfile && (
            <input
              type="text"
              value={connectionName}
              onChange={(e) => setConnectionName(e.target.value)}
              placeholder="Connection name"
              size={30}
              autoComplete="off"
              disabled={pending}
            />
          )}
        </FieldGroup>
        <DialogError error={error} />
      </DialogBody>
      <DialogFooter onCancel={cancel} cancelDisabled={pending}>
        <DialogSubmitButton
          pending={pending}
          pendingLabel="Connecting…"
          disabled={!newHost}
        >
          Mount
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
