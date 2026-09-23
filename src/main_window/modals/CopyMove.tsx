import { useState } from "react";
import {
  commands,
  type CopyMoveDefaults,
  type CopyOptions,
} from "../../lib/bindings";
import { safe, safeCommand } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  FieldGroup,
  FieldFold,
  CheckboxField,
  FieldRow,
} from "./primitives";
import styles from "./CopyMove.module.scss";

type CopyMoveProps = CommonDialogProps & ModalDataOf<"copy_move">;

// Which toggles the runtime state remembers is decided by the Rust
// `CopyMoveDefaults` struct; the explicit object keeps this in step with it.
function stickyOf(options: CopyOptions): CopyMoveDefaults {
  return {
    preserve_timestamps: options.preserve_timestamps,
    preserve_permissions: options.preserve_permissions,
    ownership_by_name: options.ownership_by_name,
    preserve_owner: options.preserve_owner,
    preserve_group: options.preserve_group,
    preserve_xattrs: options.preserve_xattrs,
    preserve_acl: options.preserve_acl,
    preserve_streams: options.preserve_streams,
    preserve_hard_links: options.preserve_hard_links,
    preserve_sparse: options.preserve_sparse,
    preserve_object_metadata: options.preserve_object_metadata,
    preserve_object_tags: options.preserve_object_tags,
    preserve_object_access: options.preserve_object_access,
  };
}

/** Whether a remembered toggle that lives behind the fold is on. */
function advancedStickyOn(defaults: CopyOptions): boolean {
  const sticky = stickyOf(defaults);
  return (Object.keys(sticky) as (keyof CopyMoveDefaults)[])
    .filter(
      (key) => key !== "preserve_timestamps" && key !== "preserve_permissions",
    )
    .some((key) => sticky[key]);
}

export default function CopyMove({
  kind,
  object_destination,
  object_source,
  sources,
  destination,
  display_destination,
  summary: itemSummary,
  default_name,
  name_separators,
  defaults,
  cancel,
  context,
}: CopyMoveProps) {
  const [options, setOptions] = useState<CopyOptions>({
    ...defaults,
    create_symlink: false,
    follow_symlinks: false,
    object_storage_class: object_destination
      ? defaults.object_storage_class
      : null,
    object_canned_acl: null,
  });
  const createSymlink = options.create_symlink;
  function toggle(key: keyof CopyOptions, checked: boolean) {
    setOptions((current) => ({ ...current, [key]: checked }));
  }
  // A remembered advanced toggle must not hide behind the fold.
  const [advancedOpen, setAdvancedOpen] = useState(advancedStickyOn(defaults));
  const [name, setName] = useState(default_name ?? "");

  const isCopy = kind === "copy";
  const title = isCopy ? "Copy" : "Move";
  const isSingleFile = sources.length === 1;
  // The value becomes a single leaf under the destination, so a separator
  // in it would silently build a subpath. Which characters those are comes
  // from the destination filesystem, not from here: `\` is a legal
  // filename character on Unix, where a directory may be called `\`.
  const nameInvalid =
    default_name != null &&
    (name === "" || [...name_separators].some((sep) => name.includes(sep)));

  function selectStem(e: React.FocusEvent<HTMLInputElement>) {
    const dot = name.lastIndexOf(".");
    e.currentTarget.setSelectionRange(0, dot > 0 ? dot : name.length);
  }

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (nameInvalid) return;

    const renameTo =
      default_name != null && name !== default_name ? name : null;
    safe(commands.updateRuntimeState("copy_move", stickyOf(options)));
    safe(commands.startCopyMove(kind, sources, destination, options, renameTo));
  }

  function checkbox(key: keyof CopyOptions, label: string, title?: string) {
    return (
      <CheckboxField
        key={key}
        label={label}
        title={title}
        checked={Boolean(options[key])}
        onChange={(checked) => toggle(key, checked)}
        disabled={createSymlink}
      />
    );
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title={title} />
      <DialogBody>
        <p className={styles.hint}>
          {title} <b>{itemSummary}</b> {default_name != null ? "as:" : "into:"}
        </p>
        {default_name != null && (
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onFocus={selectStem}
            autoFocus
            size={50}
          />
        )}
        <p className={styles.hint}>
          {default_name != null ? (
            <>
              in <b>{display_destination}</b>
            </>
          ) : (
            <b>{display_destination}</b>
          )}
        </p>
        <FieldGroup>
          {isCopy && isSingleFile && (
            <CheckboxField
              label="Create symbolic link"
              checked={createSymlink}
              onChange={(checked) => toggle("create_symlink", checked)}
            />
          )}
          {checkbox("preserve_timestamps", "Preserve timestamps")}
          {checkbox("preserve_permissions", "Preserve permissions")}
        </FieldGroup>
        <FieldFold
          summary={`More ${isCopy ? "copy" : "move"} options`}
          open={advancedOpen}
          onToggle={setAdvancedOpen}
        >
          <FieldGroup>
            {checkbox("preserve_owner", "Preserve owner")}
            {checkbox("preserve_group", "Preserve group")}
            {(options.preserve_owner || options.preserve_group) && (
              <FieldRow label="Match accounts by">
                <select
                  disabled={createSymlink}
                  value={options.ownership_by_name ? "name" : "id"}
                  onChange={(e) =>
                    toggle("ownership_by_name", e.target.value === "name")
                  }
                >
                  <option value="id">Numeric ID / native identity</option>
                  <option value="name">Account name on destination</option>
                </select>
              </FieldRow>
            )}
            {checkbox("preserve_xattrs", "Preserve extended attributes")}
            {checkbox("preserve_acl", "Preserve access control lists")}
            {checkbox(
              "preserve_streams",
              "Preserve alternate streams and resource forks",
            )}
            {checkbox(
              "preserve_hard_links",
              "Preserve hard links within the selection",
            )}
            {checkbox("preserve_sparse", "Preserve sparse files")}
            {checkbox(
              "preserve_merged_directories",
              "Apply source attributes to existing directories",
            )}
            {checkbox(
              "one_file_system",
              "Stay on one filesystem",
              "Mount points under the selection are copied as empty directories.",
            )}
            {isCopy && (
              <FieldRow label="Symbolic links">
                <select
                  value={options.follow_symlinks ? "follow" : "preserve"}
                  disabled={createSymlink}
                  onChange={(e) =>
                    toggle("follow_symlinks", e.target.value === "follow")
                  }
                >
                  <option value="preserve">Copy the links</option>
                  <option value="follow">Copy their targets</option>
                </select>
              </FieldRow>
            )}
          </FieldGroup>
          {(object_source || object_destination) && (
            <FieldGroup>
              {checkbox(
                "preserve_object_metadata",
                "Preserve object metadata and content headers",
              )}
              {checkbox("preserve_object_tags", "Preserve object tags")}
              {options.object_canned_acl === null &&
                checkbox(
                  "preserve_object_access",
                  "Preserve object access grants",
                )}
              {object_destination && (
                <>
                  <FieldRow label="Storage class">
                    <select
                      disabled={createSymlink}
                      value={options.object_storage_class ?? ""}
                      onChange={(e) =>
                        setOptions((o) => ({
                          ...o,
                          object_storage_class: e.target.value || null,
                        }))
                      }
                    >
                      <option value="">Backend default</option>
                      {[
                        "STANDARD",
                        "STANDARD_IA",
                        "ONEZONE_IA",
                        "INTELLIGENT_TIERING",
                        "GLACIER_IR",
                        "GLACIER",
                        "DEEP_ARCHIVE",
                      ].map((value) => (
                        <option key={value}>{value}</option>
                      ))}
                    </select>
                  </FieldRow>
                  <FieldRow label="Object access">
                    <select
                      disabled={createSymlink}
                      value={options.object_canned_acl ?? ""}
                      onChange={(e) =>
                        setOptions((o) => ({
                          ...o,
                          object_canned_acl: e.target.value || null,
                        }))
                      }
                    >
                      <option value="">
                        Destination default / preserved grants
                      </option>
                      {[
                        "private",
                        "bucket-owner-full-control",
                        "public-read",
                        "public-read-write",
                        "authenticated-read",
                      ].map((value) => (
                        <option key={value}>{value}</option>
                      ))}
                    </select>
                  </FieldRow>
                </>
              )}
            </FieldGroup>
          )}
          <p className={styles.hint}>
            If a requested property cannot be preserved, choose Retry, Skip, or
            Cancel in the operation dialog.
          </p>
        </FieldFold>
      </DialogBody>
      <DialogFooter
        onCancel={cancel}
        start={
          isCopy &&
          context?.pane_handle != null && (
            // Swaps this modal for the Pack to Archive dialog over the same
            // selection (the cmd_ middleware closes this one).
            <button
              type="button"
              onClick={() =>
                safeCommand("cmd_create_archive", {
                  paneHandle: context.pane_handle,
                })
              }
            >
              Pack into archive…
            </button>
          )
        }
      >
        <DialogSubmitButton
          autoFocus={default_name == null}
          disabled={nameInvalid}
        >
          {title}
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
