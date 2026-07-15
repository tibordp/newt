import { useState } from "react";
import { type ArchiveFormat, commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  DialogTabs,
  FieldGroup,
  FieldRow,
  CheckboxField,
} from "./primitives";
import styles from "./CreateArchive.module.scss";

type CreateArchiveProps = CommonDialogProps & ModalDataOf<"create_archive">;

const FORMATS: { tag: ArchiveFormat; ext: string }[] = [
  { tag: "zip", ext: "zip" },
  { tag: "tar", ext: "tar" },
  { tag: "tar_gz", ext: "tar.gz" },
  { tag: "tar_xz", ext: "tar.xz" },
  { tag: "tar_zst", ext: "tar.zst" },
];

const LEVEL_RANGE: Record<ArchiveFormat, [number, number] | null> = {
  zip: [0, 9],
  tar: null,
  tar_gz: [0, 9],
  tar_xz: [0, 9],
  tar_zst: [1, 22],
};

function extFor(format: ArchiveFormat): string {
  return FORMATS.find((f) => f.tag === format)!.ext;
}

function swapExtension(
  name: string,
  from: ArchiveFormat,
  to: ArchiveFormat,
): string {
  const oldExt = "." + extFor(from);
  const base = name.endsWith(oldExt) ? name.slice(0, -oldExt.length) : name;
  return base + "." + extFor(to);
}

export default function CreateArchive({
  sources,
  destination,
  display_destination,
  summary,
  default_name,
  defaults,
  cancel,
}: CreateArchiveProps) {
  const [format, setFormat] = useState<ArchiveFormat>(defaults.format);
  const [name, setName] = useState(
    `${default_name}.${extFor(defaults.format)}`,
  );
  // One remembered level per format, so tab-switching doesn't lose edits.
  const [levels, setLevels] = useState<Record<ArchiveFormat, number>>({
    zip: defaults.zip_level,
    tar: 0,
    tar_gz: defaults.gzip_level,
    tar_xz: defaults.xz_level,
    tar_zst: defaults.zstd_level,
  });
  const [preserveSymlinks, setPreserveSymlinks] = useState(
    defaults.preserve_symlinks,
  );
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");

  const range = LEVEL_RANGE[format];
  const passwordMismatch =
    format === "zip" && password !== "" && password !== confirmPassword;
  const nameInvalid = name.trim() === "" || name.includes("/");

  function switchFormat(next: ArchiveFormat) {
    setName((n) => swapExtension(n, format, next));
    setFormat(next);
  }

  function selectStem(e: React.FocusEvent<HTMLInputElement>) {
    const stem = name.length - extFor(format).length - 1;
    if (stem > 0) e.currentTarget.setSelectionRange(0, stem);
  }

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (nameInvalid || passwordMismatch) return;

    const dir = destination.path;
    const path = dir === "/" ? `/${name}` : `${dir}/${name}`;
    safe(
      commands.startCreateArchive(
        sources,
        { vfs_id: destination.vfs_id, path },
        {
          format,
          level: range ? levels[format] : null,
          preserve_symlinks: preserveSymlinks,
          password: format === "zip" && password !== "" ? password : null,
        },
      ),
    );
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Pack to Archive" />
      <DialogBody>
        <DialogTabs
          tabs={FORMATS.map((f) => ({ value: f.tag, label: f.ext }))}
          value={format}
          onChange={switchFormat}
        />
        <p className={styles.hint}>
          Pack <b>{summary}</b> into:
        </p>
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onFocus={selectStem}
          autoFocus
          size={50}
        />
        <p className={styles.hint}>
          in <b>{display_destination}</b>
        </p>
        <FieldGroup>
          {range && (
            <FieldRow label="Compression level">
              <input
                type="number"
                className={styles.levelInput}
                min={range[0]}
                max={range[1]}
                value={levels[format]}
                onChange={(e) => {
                  const value = Number(e.target.value);
                  if (Number.isInteger(value)) {
                    setLevels({ ...levels, [format]: value });
                  }
                }}
              />
              {format === "zip" && (
                <span className={styles.hint}>0 = store</span>
              )}
            </FieldRow>
          )}
          <CheckboxField
            label="Preserve symlinks"
            checked={preserveSymlinks}
            onChange={setPreserveSymlinks}
          />
          {format === "zip" && (
            <>
              <FieldRow label="Password">
                <input
                  type="password"
                  className={styles.passwordInput}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder="no encryption"
                />
              </FieldRow>
              {password !== "" && (
                <>
                  <FieldRow label="Confirm password">
                    <input
                      type="password"
                      className={styles.passwordInput}
                      value={confirmPassword}
                      onChange={(e) => setConfirmPassword(e.target.value)}
                    />
                  </FieldRow>
                  <p className={styles.hint}>
                    AES-256 — opens in 7-Zip, WinRAR, or Keka; not in Windows
                    Explorer.
                  </p>
                </>
              )}
            </>
          )}
        </FieldGroup>
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        <DialogSubmitButton disabled={nameInvalid || passwordMismatch}>
          Pack
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
