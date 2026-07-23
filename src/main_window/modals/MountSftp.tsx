import { useEffect, useRef, useState } from "react";
import { commands } from "../../lib/bindings";
import { tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  DialogSaveButton,
  DialogError,
  Field,
  MountLogView,
  ProfileNameField,
  useAsyncAction,
  useSaveFlash,
} from "./primitives";

type MountSftpProps = CommonDialogProps &
  ModalDataOf<"mount_sftp"> & { mountLog?: string[] };

export default function MountSftp({
  host,
  edit,
  connect_on_open,
  cancel,
  context,
  mountLog,
}: MountSftpProps) {
  const [newHost, setNewHost] = useState(host);
  const [connectionName, setConnectionName] = useState(edit?.name ?? host);
  const [nameEdited, setNameEdited] = useState(!!edit);
  // Save updates in place once the form has an identity: the edited
  // profile's, or the one minted by the first Save.
  const [savedId, setSavedId] = useState<string | null>(edit?.id ?? null);
  const [savedFlash, flashSaved] = useSaveFlash();
  // The profile-name row is hidden until save-intent; editing a saved
  // profile pins it open.
  const [saveIntent, setSaveIntent] = useState(false);
  const nameRevealed = !!edit || saveIntent;
  // While the name row is focused, Enter means Save — the button emphasis
  // follows.
  const [nameFocused, setNameFocused] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const nameRef = useRef<HTMLInputElement>(null);

  const connecting = useAsyncAction(async () =>
    tryRun(commands.mountSftp(context?.pane_handle ?? 0, newHost)),
  );

  const saving = useAsyncAction(async () => {
    const name = connectionName.trim();
    if (!newHost) return "Host is required";
    if (!name) return "Profile name is required";
    const id = savedId ?? name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
    const err = await tryRun(
      commands.cmdSaveConnection(
        { id, name, type: "sftp", host: newHost },
        null,
      ),
    );
    if (err) return err;
    setSavedId(id);
    flashSaved();
    return null;
  });

  const pending = connecting.pending || saving.pending;

  // First Save… reveals the name row (focused, preselected); once visible,
  // Save persists.
  const onSaveAction = () => {
    if (!newHost) return;
    if (!nameRevealed) {
      setSaveIntent(true);
      return;
    }
    if (!pending) saving.run();
  };

  useEffect(() => {
    if (saveIntent) {
      nameRef.current?.focus();
      nameRef.current?.select();
    }
  }, [saveIntent]);

  const dismissSave = () => {
    setSaveIntent(false);
    inputRef.current?.focus();
  };

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    connecting.run();
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Activation flow: the dialog doubles as the connection-progress surface.
  // Submit immediately; on failure it stays open, prefilled, for a retry.
  useEffect(() => {
    if (connect_on_open) connecting.run();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <DialogShell onSubmit={onSubmit} onSave={onSaveAction}>
      <DialogHeader title={edit ? `Mount SFTP — ${edit.name}` : "Mount SFTP"} />
      <DialogBody>
        <Field label="Host (e.g., user@host)" htmlFor="host">
          <input
            ref={inputRef}
            type="text"
            id="host"
            value={newHost}
            onChange={(e) => {
              setNewHost(e.target.value);
              if (!nameEdited) setConnectionName(e.target.value);
            }}
            size={40}
            autoFocus
            autoComplete="off"
            disabled={pending}
          />
        </Field>
        <ProfileNameField
          value={connectionName}
          onChange={(v) => {
            setConnectionName(v);
            setNameEdited(true);
          }}
          visible={nameRevealed}
          onSave={onSaveAction}
          onDismiss={edit ? undefined : dismissSave}
          onFocusChange={setNameFocused}
          disabled={pending}
          inputRef={nameRef}
        />
        <MountLogView lines={mountLog} visible={connecting.pending} />
        <DialogError error={connecting.error ?? saving.error} />
      </DialogBody>
      <DialogFooter onCancel={cancel} cancelDisabled={pending}>
        <DialogSaveButton
          pending={saving.pending}
          saved={savedFlash}
          disabled={connecting.pending || !newHost}
          label={nameRevealed ? "Save" : "Save…"}
          variant={nameFocused ? "suggested" : "normal"}
          onClick={onSaveAction}
        />
        <DialogSubmitButton
          pending={connecting.pending}
          pendingLabel="Connecting…"
          disabled={!newHost || saving.pending}
          variant={nameFocused ? "normal" : "suggested"}
        >
          Mount
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
