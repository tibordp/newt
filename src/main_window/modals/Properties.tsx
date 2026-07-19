import { useState, useMemo } from "react";
import {
  commands,
  type PropertyPatchOp,
  type UserGroup,
} from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { formatDateTime } from "../../lib/datetime";
import { usePreferences } from "../../lib/preferences";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import { PropertySheetSection } from "./PropertySheetSection";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./primitives";
import styles from "./Properties.module.scss";

type PropertiesProps = CommonDialogProps & ModalDataOf<"properties">;

function formatUserGroup(
  ug: UserGroup | null,
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

function formatTimestamp(
  ms: number | null,
  dateFmt?: string,
  timeFmt?: string,
): string {
  if (ms == null) return "-";
  return formatDateTime(Number(ms), dateFmt, timeFmt);
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
  // Cycle: checked → unchecked → indeterminate → checked.
  const state = getBitState(modeSet, modeClear, bit);
  if (state === "checked") {
    return [modeSet & ~bit, modeClear | bit];
  } else if (state === "unchecked") {
    return [modeSet & ~bit, modeClear & ~bit];
  } else {
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
  allocated_size,
  hard_links,
  inode,
  device_id,
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
  sheet,
  cancel,
  context,
}: PropertiesProps) {
  const [modeSet, setModeSet] = useState(initialModeSet);
  const [modeClear, setModeClear] = useState(initialModeClear);
  const [recursive, setRecursive] = useState(false);
  const [sheetOps, setSheetOps] = useState<PropertyPatchOp[]>([]);
  const [ownerEdit, setOwnerEdit] = useState<OwnerEditState>({
    enabled: false,
    value: "",
  });
  const [groupEdit, setGroupEdit] = useState<OwnerEditState>({
    enabled: false,
    value: "",
  });
  const preferences = usePreferences();
  const dateFormat = preferences?.settings?.appearance?.date_format;
  const timeFormat = preferences?.settings?.appearance?.time_format;
  const isSingle = paths.length === 1;
  const hasDirs = is_dir || paths.length > 1;

  const octalDisplay = useMemo(() => {
    // Show definite bits; indeterminate bits shown as "?"
    const chars = [];
    for (const shift of [9, 6, 3, 0]) {
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
  const metaDirty = modeChanged || ownerChanged || groupChanged;
  const isDirty = metaDirty || sheetOps.length > 0;

  function onApply() {
    // The two editors apply through separate operations; fire only the
    // ones with actual changes (recursive-only counts for the
    // permission editor, matching its historical behavior).
    if (canEdit && (metaDirty || (recursive && sheetOps.length === 0))) {
      safe(
        commands.setMetadata(
          context?.pane_handle ?? null,
          paths,
          modeSet,
          modeClear,
          ownerChanged ? parseOwnerId(ownerEdit.value) : null,
          groupChanged ? parseOwnerId(groupEdit.value) : null,
          recursive,
        ),
      );
    }
    if (sheetOps.length > 0) {
      safe(
        commands.applyProperties(
          context?.pane_handle ?? null,
          paths,
          { ops: sheetOps },
          recursive,
        ),
      );
    }
  }

  const typeLabel = is_symlink
    ? "Symbolic link"
    : is_dir
      ? "Directory"
      : "File";

  const canEdit = can_set_metadata && has_mode;
  const sheetEditable =
    sheet.status === "loaded" &&
    sheet.sheet.groups.some((g) => g.fields.some((f) => f.editable));
  const applyHint =
    sheet.status === "loaded" && sheetOps.length > 0
      ? sheet.sheet.apply_hint
      : null;

  return (
    <DialogShell>
      <DialogHeader
        title={isSingle ? "Properties" : `Properties \u2014 ${name}`}
      />
      <DialogBody>
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
              {allocated_size != null && (
                <InfoRow
                  label="Size on disk"
                  value={formatSize(allocated_size)}
                />
              )}
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
                      value={formatTimestamp(modified, dateFormat, timeFormat)}
                    />
                  )}
                  {accessed != null && (
                    <InfoRow
                      label="Accessed"
                      value={formatTimestamp(accessed, dateFormat, timeFormat)}
                    />
                  )}
                  {created != null && (
                    <InfoRow
                      label="Created"
                      value={formatTimestamp(created, dateFormat, timeFormat)}
                    />
                  )}
                </dl>
              )}
            {(hard_links != null || inode != null || device_id != null) && (
              <dl className={styles.infoList}>
                {hard_links != null && (
                  <InfoRow label="Hard links" value={String(hard_links)} />
                )}
                {inode != null && (
                  <InfoRow label="Inode" value={String(inode)} />
                )}
                {device_id != null && (
                  <InfoRow label="Device" value={String(device_id)} />
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
            </div>
          )}
        </div>

        <PropertySheetSection state={sheet} onOpsChange={setSheetOps} />
      </DialogBody>
      <DialogFooter
        onCancel={cancel}
        cancelLabel={canEdit || sheetEditable ? "Cancel" : "Close"}
        start={
          <>
            {hasDirs && (canEdit || sheetEditable) && (
              <label className={styles.recursiveLabel}>
                <input
                  type="checkbox"
                  checked={recursive}
                  onChange={(e) => setRecursive(e.target.checked)}
                />
                Apply recursively
              </label>
            )}
            {applyHint && <span className={styles.sheetHint}>{applyHint}</span>}
          </>
        }
      >
        {(canEdit || sheetEditable) && (
          <button
            type="button"
            className="suggested"
            onClick={onApply}
            disabled={!isDirty && !(recursive && canEdit)}
            autoFocus
          >
            Apply
          </button>
        )}
      </DialogFooter>
    </DialogShell>
  );
}
