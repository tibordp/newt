import { useState, useMemo } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";
import styles from "./Properties.module.scss";

type PropertiesProps = CommonDialogProps & {
  paths: { vfs_id: number; path: string }[];
  name: string;
  size: number | null;
  is_dir: boolean;
  is_symlink: boolean;
  symlink_target: string | null;
  can_set_metadata: boolean;
  mode_set: number;
  mode_clear: number;
  has_mode: boolean;
  owner: { name: string } | { id: number } | null;
  group: { name: string } | { id: number } | null;
  owner_id: number | null;
  group_id: number | null;
  modified: number | null;
  accessed: number | null;
  created: number | null;
};

function formatUserGroup(
  ug: { name: string } | { id: number } | null,
  id: number | null,
  isSingle: boolean,
): string {
  if (!ug) return isSingle ? "-" : "(mixed)";
  if ("name" in ug) {
    return id != null ? `${ug.name} (${id})` : ug.name;
  }
  return String(ug.id);
}

function formatSize(bytes: number | null): string {
  if (bytes == null) return "-";
  if (bytes < 1024) return `${bytes} bytes`;
  const units = ["KB", "MB", "GB", "TB"];
  let val = bytes;
  let unit = "bytes";
  for (const u of units) {
    if (val < 1024) break;
    val /= 1024;
    unit = u;
  }
  return `${val.toFixed(1)} ${unit} (${bytes.toLocaleString()} bytes)`;
}

function formatTimestamp(ms: number | null): string {
  if (ms == null) return "-";
  return new Date(Number(ms)).toLocaleString();
}

// Permission bit positions
const PERM_BITS = [
  { label: "Read", bits: [0o400, 0o040, 0o004] },
  { label: "Write", bits: [0o200, 0o020, 0o002] },
  { label: "Execute", bits: [0o100, 0o010, 0o001] },
];

const SPECIAL_BITS = [
  { label: "Set UID", bit: 0o4000 },
  { label: "Set GID", bit: 0o2000 },
  { label: "Sticky", bit: 0o1000 },
];

// Tri-state: a bit can be "set", "clear", or "indeterminate" (mixed).
// We track this with two masks: modeSet (bits forced ON) and modeClear (bits forced OFF).
// A bit in neither mask is indeterminate.
type TriState = "checked" | "unchecked" | "indeterminate";

function getBitState(
  modeSet: number,
  modeClear: number,
  bit: number,
): TriState {
  if (modeSet & bit) return "checked";
  if (modeClear & bit) return "unchecked";
  return "indeterminate";
}

function cycleBit(
  modeSet: number,
  modeClear: number,
  bit: number,
): [number, number] {
  const state = getBitState(modeSet, modeClear, bit);
  if (state === "checked") {
    // checked → unchecked
    return [modeSet & ~bit, modeClear | bit];
  } else if (state === "unchecked") {
    // unchecked → indeterminate (leave unchanged)
    return [modeSet & ~bit, modeClear & ~bit];
  } else {
    // indeterminate → checked
    return [modeSet | bit, modeClear & ~bit];
  }
}

function TriStateCheckbox({
  state,
  onChange,
}: {
  state: TriState;
  onChange: () => void;
}) {
  return (
    <input
      type="checkbox"
      checked={state === "checked"}
      ref={(el) => {
        if (el) el.indeterminate = state === "indeterminate";
      }}
      onChange={onChange}
    />
  );
}

function PermissionEditor({
  modeSet,
  modeClear,
  onChange,
}: {
  modeSet: number;
  modeClear: number;
  onChange: (modeSet: number, modeClear: number) => void;
}) {
  const toggle = (bit: number) => {
    const [s, c] = cycleBit(modeSet, modeClear, bit);
    onChange(s, c);
  };

  return (
    <div className={styles.permGrid}>
      <div className={styles.permHeader}></div>
      <div className={styles.permHeader}>Owner</div>
      <div className={styles.permHeader}>Group</div>
      <div className={styles.permHeader}>Other</div>
      {PERM_BITS.map(({ label, bits }) => (
        <div key={label} className={styles.permRow}>
          <div className={styles.permLabel}>{label}</div>
          {bits.map((bit) => (
            <div key={bit} className={styles.permCell}>
              <TriStateCheckbox
                state={getBitState(modeSet, modeClear, bit)}
                onChange={() => toggle(bit)}
              />
            </div>
          ))}
        </div>
      ))}
      <div className={styles.specialBits}>
        {SPECIAL_BITS.map(({ label, bit }) => (
          <label key={bit} className={styles.specialBitLabel}>
            <TriStateCheckbox
              state={getBitState(modeSet, modeClear, bit)}
              onChange={() => toggle(bit)}
            />
            {label}
          </label>
        ))}
      </div>
    </div>
  );
}

// Owner/group editor: "leave unchanged" | enter name or numeric ID
type OwnerEditState = {
  enabled: boolean;
  value: string;
};

function OwnerEditor({
  label,
  state,
  onChange,
}: {
  label: string;
  state: OwnerEditState;
  onChange: (s: OwnerEditState) => void;
}) {
  return (
    <div className={styles.ownerEditor}>
      <label className={styles.ownerLabel}>
        <input
          type="checkbox"
          checked={state.enabled}
          onChange={(e) => onChange({ ...state, enabled: e.target.checked })}
        />
        {label}
      </label>
      {state.enabled && (
        <input
          type="text"
          className={styles.ownerInput}
          placeholder="name or numeric ID"
          value={state.value}
          autoComplete="off"
          autoCorrect="off"
          onChange={(e) => onChange({ ...state, value: e.target.value })}
        />
      )}
    </div>
  );
}

function parseOwnerId(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const num = Number(trimmed);
  if (Number.isInteger(num) && num >= 0) return num;
  // For now, only numeric IDs are supported for setting.
  // Name resolution would require a Rust-side lookup.
  return null;
}

function InfoRow({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className={styles.infoRow}>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </div>
  );
}

export default function Properties({
  paths,
  name,
  size,
  is_dir,
  is_symlink,
  symlink_target,
  can_set_metadata,
  mode_set: initialModeSet,
  mode_clear: initialModeClear,
  has_mode,
  owner,
  group,
  owner_id,
  group_id,
  modified,
  accessed,
  created,
  cancel,
  context,
}: PropertiesProps) {
  const [modeSet, setModeSet] = useState(initialModeSet);
  const [modeClear, setModeClear] = useState(initialModeClear);
  const [recursive, setRecursive] = useState(false);
  const [ownerEdit, setOwnerEdit] = useState<OwnerEditState>({
    enabled: false,
    value: "",
  });
  const [groupEdit, setGroupEdit] = useState<OwnerEditState>({
    enabled: false,
    value: "",
  });
  const isSingle = paths.length === 1;
  const hasDirs = is_dir || paths.length > 1;

  const octalDisplay = useMemo(() => {
    // Show definite bits; indeterminate bits shown as "?"
    const chars = [];
    for (const shift of [9, 6, 3, 0]) {
      const mask = 0o7 << shift;
      const set = (modeSet >> shift) & 0o7;
      const clear = (modeClear >> shift) & 0o7;
      if ((set | clear) === 0o7) {
        // All bits determined
        chars.push(set.toString(8));
      } else {
        chars.push("?");
      }
    }
    return chars.join("");
  }, [modeSet, modeClear]);

  const modeChanged =
    modeSet !== initialModeSet || modeClear !== initialModeClear;
  const ownerChanged = ownerEdit.enabled && ownerEdit.value.trim() !== "";
  const groupChanged = groupEdit.enabled && groupEdit.value.trim() !== "";
  const isDirty = modeChanged || ownerChanged || groupChanged;

  function onApply() {
    safeCommand("set_metadata", {
      paneHandle: context?.pane_handle,
      paths,
      modeSet,
      modeClear,
      uid: ownerChanged ? parseOwnerId(ownerEdit.value) : null,
      gid: groupChanged ? parseOwnerId(groupEdit.value) : null,
      recursive,
    });
  }

  const typeLabel = is_symlink
    ? "Symbolic link"
    : is_dir
      ? "Directory"
      : "File";

  const canEdit = can_set_metadata && has_mode;

  return (
    <div>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          {isSingle ? "Properties" : `Properties \u2014 ${name}`}
        </Dialog.Title>

        <div className={canEdit ? styles.columns : undefined}>
          <div className={styles.infoSection}>
            <dl className={styles.infoList}>
              {isSingle && <InfoRow label="Name" value={name} />}
              {isSingle && (
                <InfoRow
                  label="Type"
                  value={
                    <>
                      {typeLabel}
                      {symlink_target && (
                        <span className={styles.symlinkTarget}>
                          {" "}
                          &rarr; {symlink_target}
                        </span>
                      )}
                    </>
                  }
                />
              )}
              <InfoRow label="Size" value={formatSize(size)} />
              {(owner != null || !isSingle) && (
                <InfoRow
                  label="Owner"
                  value={formatUserGroup(owner, owner_id, isSingle)}
                />
              )}
              {(group != null || !isSingle) && (
                <InfoRow
                  label="Group"
                  value={formatUserGroup(group, group_id, isSingle)}
                />
              )}
            </dl>
            {isSingle &&
              (modified != null || accessed != null || created != null) && (
                <dl className={styles.infoList}>
                  {modified != null && (
                    <InfoRow
                      label="Modified"
                      value={formatTimestamp(modified)}
                    />
                  )}
                  {accessed != null && (
                    <InfoRow
                      label="Accessed"
                      value={formatTimestamp(accessed)}
                    />
                  )}
                  {created != null && (
                    <InfoRow label="Created" value={formatTimestamp(created)} />
                  )}
                </dl>
              )}
          </div>

          {canEdit && (
            <div className={styles.permSection}>
              <div className={styles.permSectionHeader}>Permissions</div>
              <PermissionEditor
                modeSet={modeSet}
                modeClear={modeClear}
                onChange={(s, c) => {
                  setModeSet(s);
                  setModeClear(c);
                }}
              />
              <div className={styles.octalDisplay}>
                <code>{octalDisplay}</code>
              </div>

              <div className={styles.permSectionHeader}>Ownership</div>
              <OwnerEditor
                label="Set owner"
                state={ownerEdit}
                onChange={setOwnerEdit}
              />
              <OwnerEditor
                label="Set group"
                state={groupEdit}
                onChange={setGroupEdit}
              />

              {hasDirs && (
                <label className={styles.recursiveLabel}>
                  <input
                    type="checkbox"
                    checked={recursive}
                    onChange={(e) => setRecursive(e.target.checked)}
                  />
                  Apply recursively
                </label>
              )}
            </div>
          )}
        </div>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          {canEdit ? "Cancel" : "Close"}
        </button>
        {canEdit && (
          <button
            type="button"
            className="suggested"
            onClick={onApply}
            disabled={!isDirty && !recursive}
            autoFocus
          >
            Apply
          </button>
        )}
      </div>
    </div>
  );
}
