import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeSilent, tryRun } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import { useAsyncAction, DialogError, DialogSubmitButton } from "./primitives";
import dialogStyles from "./Dialog.module.scss";
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
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Mount S3
        </Dialog.Title>

        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-4)",
          }}
        >
          <div>
            <label htmlFor="s3-region">Region (optional)</label>
            <input
              id="s3-region"
              type="text"
              value={region}
              onChange={(e) => setRegion(e.target.value)}
              placeholder="us-east-1"
              autoFocus
              autoComplete="off"
            />
          </div>

          <div>
            <label htmlFor="s3-bucket">Bucket (optional)</label>
            <input
              id="s3-bucket"
              type="text"
              value={bucket}
              onChange={(e) => setBucket(e.target.value)}
              placeholder="Scope mount to a specific bucket"
              autoComplete="off"
              spellCheck={false}
            />
          </div>

          <div>
            <label htmlFor="s3-endpoint">Endpoint URL (optional)</label>
            <input
              id="s3-endpoint"
              type="text"
              value={endpointUrl}
              onChange={(e) => setEndpointUrl(e.target.value)}
              placeholder="https://s3.amazonaws.com"
              autoComplete="off"
              spellCheck={false}
            />
          </div>

          <div>
            <label htmlFor="s3-cred-mode">Credentials</label>
            <select
              id="s3-cred-mode"
              value={credentialMode}
              style={{ width: "100%" }}
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
          </div>

          {credentialMode === "profile" && (
            <div>
              <label htmlFor="s3-profile">Profile name</label>
              <input
                id="s3-profile"
                type="text"
                value={awsProfileName}
                onChange={(e) => setProfileName(e.target.value)}
                placeholder="default"
                autoComplete="off"
              />
            </div>
          )}

          {credentialMode === "iam_user" && (
            <>
              <div>
                <label htmlFor="s3-access-key">Access Key ID</label>
                <input
                  id="s3-access-key"
                  type="text"
                  value={accessKeyId}
                  onChange={(e) => setAccessKeyId(e.target.value)}
                  autoComplete="off"
                  spellCheck={false}
                />
              </div>
              <div>
                <label htmlFor="s3-secret-key">Secret Access Key</label>
                <input
                  id="s3-secret-key"
                  type="password"
                  value={secretAccessKey}
                  onChange={(e) => setSecretAccessKey(e.target.value)}
                  autoComplete="off"
                />
              </div>
            </>
          )}

          {credentialMode === "assume_role" && (
            <>
              <div>
                <label htmlFor="s3-role-arn">Role ARN</label>
                <input
                  id="s3-role-arn"
                  type="text"
                  value={roleArn}
                  onChange={(e) => setRoleArn(e.target.value)}
                  placeholder="arn:aws:iam::123456789012:role/MyRole"
                  autoComplete="off"
                  spellCheck={false}
                />
              </div>
              <div>
                <label htmlFor="s3-external-id">External ID (optional)</label>
                <input
                  id="s3-external-id"
                  type="text"
                  value={externalId}
                  onChange={(e) => setExternalId(e.target.value)}
                  autoComplete="off"
                />
              </div>
            </>
          )}

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
                value={displayedConnectionName}
                onChange={(e) => {
                  setConnectionName(e.target.value);
                  setConnectionNameEdited(true);
                }}
                placeholder="Connection name"
                size={30}
                style={{ marginTop: "var(--space-2)" }}
                autoComplete="off"
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
          disabled={!canSubmit}
        >
          Mount
        </DialogSubmitButton>
      </div>
    </form>
  );
}
