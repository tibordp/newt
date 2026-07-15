import { useState } from "react";
import { safeSilent, tryRun } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
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
import { commands } from "../../lib/bindings";

type CredentialMode = "default" | "iam_user" | "assume_role" | "profile";

import type { S3Credentials } from "../../lib/bindings";

export default function MountS3({ cancel, context }: CommonDialogProps) {
  const [region, setRegion] = useState("");
  const [bucket, setBucket] = useState("");
  const [credentialMode, setCredentialMode] =
    useState<CredentialMode>("default");
  const [accessKeyId, setAccessKeyId] = useState("");
  const [secretAccessKey, setSecretAccessKey] = useState("");
  const [awsProfileName, setProfileName] = useState("");
  const [endpointUrl, setEndpointUrl] = useState("");
  const [roleArn, setRoleArn] = useState("");
  const [externalId, setExternalId] = useState("");
  const [saveProfile, setSaveProfile] = useState(false);
  const [connectionNameEdited, setConnectionNameEdited] = useState(false);
  const [connectionName, setConnectionName] = useState("");

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

  const { pending, error, run } = useAsyncAction(async () => {
    const credentials = buildCredentials();

    const finalName = displayedConnectionName;
    if (saveProfile && finalName) {
      const id = finalName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
      const secret =
        credentialMode === "iam_user" && accessKeyId && secretAccessKey
          ? JSON.stringify({
              access_key_id: accessKeyId,
              secret_access_key: secretAccessKey,
            })
          : null;
      await safeSilent(
        commands.cmdSaveConnection(
          {
            id,
            name: finalName,
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
    }

    return tryRun(
      commands.mountS3(
        context?.pane_handle ?? 0,
        region || null,
        bucket || null,
        credentials,
      ),
    );
  });

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
  }

  const canSubmit =
    credentialMode === "default" ||
    credentialMode === "profile" ||
    (credentialMode === "iam_user" && accessKeyId && secretAccessKey) ||
    (credentialMode === "assume_role" && !!roleArn);

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Mount S3" />
      <DialogBody>
        <Field label="Region (optional)" htmlFor="s3-region">
          <input
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

        <FieldGroup>
          <CheckboxField
            label="Save as connection profile"
            checked={saveProfile}
            onChange={setSaveProfile}
          />
          {saveProfile && (
            <input
              type="text"
              value={displayedConnectionName}
              onChange={(e) => {
                setConnectionName(e.target.value);
                setConnectionNameEdited(true);
              }}
              placeholder="Connection name"
              size={30}
              autoComplete="off"
            />
          )}
        </FieldGroup>
        <DialogError error={error} />
      </DialogBody>
      <DialogFooter onCancel={cancel} cancelDisabled={pending}>
        <DialogSubmitButton
          pending={pending}
          pendingLabel="Connecting…"
          disabled={!canSubmit}
        >
          Mount
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
