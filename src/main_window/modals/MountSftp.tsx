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

export default function MountSftp({
  host,
  edit,
  cancel,
  context,
}: MountSftpProps) {
  const [newHost, setNewHost] = useState(host);
  const [saveProfile, setSaveProfile] = useState(!!edit);
  const [connectionName, setConnectionName] = useState(edit?.name ?? host);
  const inputRef = useRef<HTMLInputElement>(null);

  const { pending, error, run } = useAsyncAction(async (connect: boolean) => {
    if (saveProfile && connectionName) {
      // Editing keeps the profile's id stable across renames.
      const id =
        edit?.id ?? connectionName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
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
    if (!connect) {
      // Save-only: back to Quick Connect, which re-reads the profiles.
      await safeSilent(
        commands.dialog("quick_connect", context?.pane_handle ?? null),
      );
      return null;
    }
    return tryRun(commands.mountSftp(context?.pane_handle ?? 0, newHost));
  });

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run(true);
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title={edit ? "Edit Connection" : "Mount SFTP"} />
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
            label={
              edit ? "Update connection profile" : "Save as connection profile"
            }
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
        {edit && saveProfile && (
          <button
            type="button"
            onClick={() => run(false)}
            disabled={pending || !newHost}
          >
            Save
          </button>
        )}
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
