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
  mode: number | null;
  owner: { name: string } | { id: number } | null;
  group: { name: string } | { id: number } | null;
  modified: number | null;
  accessed: number | null;
  created: number | null;
};

function formatUserGroup(ug: { name: string } | { id: number } | null): string {
  if (!ug) return "(mixed)";
  if ("name" in ug) return ug.name;
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
  // Timestamps come as milliseconds since epoch from Rust's ToUnix trait
  return new Date(Number(ms)).toLocaleString();
}

// Permission bit positions
const PERM_BITS = [
  { label: "Read",    row: 0, bits: [0o400, 0o040, 0o004] },
  { label: "Write",   row: 1, bits: [0o200, 0o020, 0o002] },
  { label: "Execute", row: 2, bits: [0o100, 0o010, 0o001] },
];

const SPECIAL_BITS = [
  { label: "Set UID", bit: 0o4000 },
  { label: "Set GID", bit: 0o2000 },
  { label: "Sticky",  bit: 0o1000 },
];

function PermissionEditor({ mode, onChange }: { mode: number; onChange: (m: number) => void }) {
  const toggle = (bit: number) => {
    onChange(mode ^ bit);
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
              <input
                type="checkbox"
                checked={(mode & bit) !== 0}
                onChange={() => toggle(bit)}
              />
            </div>
          ))}
        </div>
      ))}
      <div className={styles.specialBits}>
        {SPECIAL_BITS.map(({ label, bit }) => (
          <label key={bit} className={styles.specialBitLabel}>
            <input
              type="checkbox"
              checked={(mode & bit) !== 0}
              onChange={() => toggle(bit)}
            />
            {label}
          </label>
        ))}
      </div>
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
  mode: initialMode,
  owner,
  group,
  modified,
  accessed,
  created,
  cancel,
  context,
}: PropertiesProps) {
  const [mode, setMode] = useState(initialMode ?? 0);
  const [recursive, setRecursive] = useState(false);
  const isSingle = paths.length === 1;
  const hasMode = initialMode != null;
  const hasDirs = is_dir || paths.length > 1;

  const octalMode = useMemo(() => {
    return "0" + mode.toString(8).padStart(4, "0");
  }, [mode]);

  const isDirty = hasMode && mode !== initialMode;

  function onApply() {
    safeCommand("set_permissions", {
      paneHandle: context?.pane_handle,
      paths,
      mode,
      recursive,
    });
  }

  return (
    <div>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          {isSingle ? "Properties" : `Properties — ${name}`}
        </Dialog.Title>

        <table className={styles.propsTable}>
          <tbody>
            {isSingle && (
              <tr>
                <td className={styles.propLabel}>Name</td>
                <td>{name}</td>
              </tr>
            )}
            {isSingle && (
              <tr>
                <td className={styles.propLabel}>Type</td>
                <td>
                  {is_symlink ? "Symbolic link" : is_dir ? "Directory" : "File"}
                  {symlink_target && ` → ${symlink_target}`}
                </td>
              </tr>
            )}
            <tr>
              <td className={styles.propLabel}>Size</td>
              <td>{formatSize(size)}</td>
            </tr>
            {isSingle && modified != null && (
              <tr>
                <td className={styles.propLabel}>Modified</td>
                <td>{formatTimestamp(modified)}</td>
              </tr>
            )}
            {isSingle && accessed != null && (
              <tr>
                <td className={styles.propLabel}>Accessed</td>
                <td>{formatTimestamp(accessed)}</td>
              </tr>
            )}
            {isSingle && created != null && (
              <tr>
                <td className={styles.propLabel}>Created</td>
                <td>{formatTimestamp(created)}</td>
              </tr>
            )}
            <tr>
              <td className={styles.propLabel}>Owner</td>
              <td>{formatUserGroup(owner)}</td>
            </tr>
            <tr>
              <td className={styles.propLabel}>Group</td>
              <td>{formatUserGroup(group)}</td>
            </tr>
          </tbody>
        </table>

        {hasMode && (
          <>
            <h3 className={styles.sectionTitle}>Permissions</h3>
            <PermissionEditor mode={mode} onChange={setMode} />
            <div className={styles.octalDisplay}>
              Octal: <code>{octalMode}</code>
            </div>
            {hasDirs && (
              <label className={styles.recursiveLabel}>
                <input
                  type="checkbox"
                  checked={recursive}
                  onChange={(e) => setRecursive(e.target.checked)}
                />
                Apply recursively to directories
              </label>
            )}
          </>
        )}
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          {hasMode ? "Cancel" : "Close"}
        </button>
        {hasMode && (
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
