import { useEffect, useRef, useState } from "react";
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
import { commands } from "../../lib/bindings";

type CredentialMode = "default" | "iam_user" | "assume_role" | "profile";

import type { S3Credentials } from "../../lib/bindings";

type MountS3Props = CommonDialogProps &
  ModalDataOf<"mount_s3"> & { mountLog?: string[] };

export default function MountS3({
  initial,
  edit,
  connect_on_open,
  cancel,
  context,
  mountLog,
}: MountS3Props) {
  const s3 = initial?.type === "s3" ? initial : null;
  const [region, setRegion] = useState(s3?.region ?? "");
  const [bucket, setBucket] = useState(s3?.bucket ?? "");
  const [credentialMode, setCredentialMode] = useState<CredentialMode>(
    (s3?.credential_mode as CredentialMode) ?? "default",
  );
  const [accessKeyId, setAccessKeyId] = useState("");
  const [secretAccessKey, setSecretAccessKey] = useState("");
  const [awsProfileName, setProfileName] = useState(s3?.profile ?? "");
  const [endpointUrl, setEndpointUrl] = useState(s3?.endpoint_url ?? "");
  const [roleArn, setRoleArn] = useState(s3?.role_arn ?? "");
  const [externalId, setExternalId] = useState(s3?.external_id ?? "");
  const [connectionNameEdited, setConnectionNameEdited] = useState(!!edit);
  const [connectionName, setConnectionName] = useState(edit?.name ?? "");
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
  const firstInputRef = useRef<HTMLInputElement>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  // Activation flow: submit on open, deferred one render so keychain keys
  // land in state before the submit closure reads them. If IAM-user keys
  // are missing, the dialog simply stays open for re-entry.
  const [autoConnect, setAutoConnect] = useState(false);

  // Editing/activating an IAM-user profile: prefill the key pair from the
  // keychain so saving without touching the fields keeps the stored secret.
  useEffect(() => {
    if (edit && s3?.credential_mode === "iam_user") {
      (async () => {
        const r = await commands.cmdGetConnectionSecret(edit.id);
        if (r.status !== "ok" || !r.data) return;
        try {
          const parsed = JSON.parse(r.data);
          setAccessKeyId(parsed.access_key_id ?? "");
          setSecretAccessKey(parsed.secret_access_key ?? "");
          if (
            connect_on_open &&
            parsed.access_key_id &&
            parsed.secret_access_key
          ) {
            setAutoConnect(true);
          }
        } catch {
          // Unparseable secret — leave the fields empty for re-entry.
        }
      })();
    } else if (connect_on_open) {
      setAutoConnect(true);
    }
    // Runs once per dialog open; `edit`/`s3` never change while mounted.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (autoConnect) {
      setAutoConnect(false);
      connecting.run();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoConnect]);

  // Auto-generate connection name from fields until user manually edits it
  const suggestedName = bucket
    ? endpointUrl
      ? `${bucket} (${endpointUrl})`
      : bucket
    : endpointUrl
      ? endpointUrl
      : region || "S3";
  const displayedConnectionName = connectionNameEdited
    ? connectionName
    : suggestedName;

  function buildCredentials(): S3Credentials {
    const base: S3Credentials = {
      access_key_id: null,
      secret_access_key: null,
      session_token: null,
      profile: null,
      endpoint_url: endpointUrl || null,
      role_arn: null,
      external_id: null,
    };
    switch (credentialMode) {
      case "iam_user":
        return {
          ...base,
          access_key_id: accessKeyId || null,
          secret_access_key: secretAccessKey || null,
        };
      case "assume_role":
        return {
          ...base,
          role_arn: roleArn || null,
          external_id: externalId || null,
        };
      case "profile":
        return {
          ...base,
          profile: awsProfileName || null,
        };
      default:
        return base;
    }
  }

  const connecting = useAsyncAction(async () =>
    tryRun(
      commands.mountS3(
        context?.pane_handle ?? 0,
        region || null,
        bucket || null,
        buildCredentials(),
      ),
    ),
  );

  const saving = useAsyncAction(async () => {
    const name = displayedConnectionName.trim();
    if (!name) return "Profile name is required";
    const id = savedId ?? name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
    const secret =
      credentialMode === "iam_user" && accessKeyId && secretAccessKey
        ? JSON.stringify({
            access_key_id: accessKeyId,
            secret_access_key: secretAccessKey,
          })
        : null;
    const err = await tryRun(
      commands.cmdSaveConnection(
        {
          id,
          name,
          type: "s3",
          region: region || null,
          bucket: bucket || null,
          endpoint_url: endpointUrl || null,
          credential_mode: credentialMode,
          profile: awsProfileName || null,
          role_arn: roleArn || null,
          external_id: externalId || null,
        },
        secret,
      ),
    );
    if (err) return err;
    setSavedId(id);
    flashSaved();
    return null;
  });

  const pending = connecting.pending || saving.pending;

  const canSubmit =
    credentialMode === "default" ||
    credentialMode === "profile" ||
    (credentialMode === "iam_user" && accessKeyId && secretAccessKey) ||
    (credentialMode === "assume_role" && !!roleArn);

  // First Save… reveals the name row (focused, preselected); once visible,
  // Save persists.
  const onSaveAction = () => {
    if (!canSubmit) return;
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
    firstInputRef.current?.focus();
  };

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    connecting.run();
  }

  return (
    <DialogShell onSubmit={onSubmit} onSave={onSaveAction}>
      <DialogHeader title={edit ? `Mount S3 — ${edit.name}` : "Mount S3"} />
      <DialogBody>
        <Field label="Region (optional)" htmlFor="s3-region">
          <input
            ref={firstInputRef}
            id="s3-region"
            type="text"
            value={region}
            onChange={(e) => setRegion(e.target.value)}
            placeholder="us-east-1"
            autoFocus
            autoComplete="off"
          />
        </Field>

        <Field label="Bucket (optional)" htmlFor="s3-bucket">
          <input
            id="s3-bucket"
            type="text"
            value={bucket}
            onChange={(e) => setBucket(e.target.value)}
            placeholder="Scope mount to a specific bucket"
            autoComplete="off"
            spellCheck={false}
          />
        </Field>

        <Field label="Endpoint URL (optional)" htmlFor="s3-endpoint">
          <input
            id="s3-endpoint"
            type="text"
            value={endpointUrl}
            onChange={(e) => setEndpointUrl(e.target.value)}
            placeholder="https://s3.amazonaws.com"
            autoComplete="off"
            spellCheck={false}
          />
        </Field>

        <Field label="Credentials" htmlFor="s3-cred-mode">
          <select
            id="s3-cred-mode"
            value={credentialMode}
            onChange={(e) =>
              setCredentialMode(e.target.value as CredentialMode)
            }
          >
            <option value="default">
              Default (environment / instance metadata)
            </option>
            <option value="profile">AWS Profile</option>
            <option value="iam_user">IAM User (access key)</option>
            <option value="assume_role">Assume Role</option>
          </select>
        </Field>

        {credentialMode === "profile" && (
          <Field label="Profile name" htmlFor="s3-profile">
            <input
              id="s3-profile"
              type="text"
              value={awsProfileName}
              onChange={(e) => setProfileName(e.target.value)}
              placeholder="default"
              autoComplete="off"
            />
          </Field>
        )}

        {credentialMode === "iam_user" && (
          <>
            <Field label="Access Key ID" htmlFor="s3-access-key">
              <input
                id="s3-access-key"
                type="text"
                value={accessKeyId}
                onChange={(e) => setAccessKeyId(e.target.value)}
                autoComplete="off"
                spellCheck={false}
              />
            </Field>
            <Field label="Secret Access Key" htmlFor="s3-secret-key">
              <input
                id="s3-secret-key"
                type="password"
                value={secretAccessKey}
                onChange={(e) => setSecretAccessKey(e.target.value)}
                autoComplete="off"
              />
            </Field>
          </>
        )}

        {credentialMode === "assume_role" && (
          <>
            <Field label="Role ARN" htmlFor="s3-role-arn">
              <input
                id="s3-role-arn"
                type="text"
                value={roleArn}
                onChange={(e) => setRoleArn(e.target.value)}
                placeholder="arn:aws:iam::123456789012:role/MyRole"
                autoComplete="off"
                spellCheck={false}
              />
            </Field>
            <Field label="External ID (optional)" htmlFor="s3-external-id">
              <input
                id="s3-external-id"
                type="text"
                value={externalId}
                onChange={(e) => setExternalId(e.target.value)}
                autoComplete="off"
              />
            </Field>
          </>
        )}

        <ProfileNameField
          value={displayedConnectionName}
          onChange={(v) => {
            setConnectionName(v);
            setConnectionNameEdited(true);
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
          disabled={connecting.pending || !canSubmit}
          label={nameRevealed ? "Save" : "Save…"}
          variant={nameFocused ? "suggested" : "normal"}
          onClick={onSaveAction}
        />
        <DialogSubmitButton
          pending={connecting.pending}
          pendingLabel="Connecting…"
          disabled={!canSubmit || saving.pending}
          variant={nameFocused ? "normal" : "suggested"}
        >
          Mount
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
