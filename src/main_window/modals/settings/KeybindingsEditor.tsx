import { Fragment, useMemo, useState } from "react";

import { commands as ipc } from "../../../lib/bindings";
import { unwrap, safeSilent } from "../../../lib/ipc";
import { CommandInfo, ResolvedBinding } from "../../../lib/preferences";
import styles from "../SettingsEditor.module.scss";
import {
  Conflict,
  ConflictMark,
  detectConflicts,
  findBindingOverlaps,
  isCompleteKey,
  KeyCaptureInput,
  shortcutChips,
  whenLabel,
} from "./keybindingHelpers";

type EditState = {
  commandId: string;
  /// Working key list; entries are "" (empty capture row) or complete keys —
  /// KeyCaptureInput only ever commits complete combinations.
  keys: string[];
};

function sameKeySet(a: string[], b: string[]): boolean {
  const sa = [...a].sort();
  const sb = [...b].sort();
  return sa.length === sb.length && sa.every((k, i) => k === sb[i]);
}

export function KeybindingsEditor({
  commands,
  bindings,
  filter,
}: {
  commands: CommandInfo[];
  bindings: ResolvedBinding[];
  filter: string;
}) {
  const [edit, setEdit] = useState<EditState | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Tracks the (keys, when) the user has explicitly acknowledged as a
  // conflict. Save remains gated until ack matches the current draft, and
  // changing any key invalidates the ack.
  const [acked, setAcked] = useState<{ keys: string; when: string } | null>(
    null,
  );

  const commandsById = useMemo(() => {
    const m = new Map<string, CommandInfo>();
    for (const c of commands) m.set(c.id, c);
    return m;
  }, [commands]);

  const overlaps = useMemo(
    () =>
      findBindingOverlaps(bindings, (id) => commandsById.get(id)?.name ?? id),
    [bindings, commandsById],
  );

  const filtered = useMemo(() => {
    if (!filter) return commands;
    const lower = filter.toLowerCase();
    return commands.filter(
      (c) =>
        c.name.toLowerCase().includes(lower) ||
        c.id.toLowerCase().includes(lower) ||
        c.shortcuts.some((k) => k.toLowerCase().includes(lower)) ||
        (c.when && c.when.toLowerCase().includes(lower)),
    );
  }, [commands, filter]);

  const startEdit = (cmd: CommandInfo) => {
    setError(null);
    setAcked(null);
    setEdit({
      commandId: cmd.id,
      keys: cmd.shortcuts.length > 0 ? [...cmd.shortcuts] : [""],
    });
  };

  const cancelEdit = () => {
    setEdit(null);
    setError(null);
    setAcked(null);
  };

  const save = async (cmd: CommandInfo, keysToSave: string[]) => {
    if (!edit) return;
    try {
      // The when clause is a property of the command, not the user's choice —
      // keep whatever the command currently uses (its default for built-ins).
      // We do NOT pre-clear conflicting bindings: the new binding wins by
      // resolution order and the loser's row visibly shows as shadowed. To
      // reclaim, the user can Reset either side — Reset is symmetric.
      await unwrap(
        ipc.setCommandKeybindings(
          edit.commandId,
          keysToSave,
          cmd.default_when ?? cmd.when ?? null,
        ),
      );
      setEdit(null);
      setError(null);
    } catch (e: any) {
      setError(typeof e === "string" ? e : (e?.message ?? String(e)));
    }
  };

  const reset = async (cmd: CommandInfo) => {
    await safeSilent(ipc.resetCommandKeybinding(cmd.id));
  };

  return (
    <div className={styles.settingsList}>
      <table className={styles.keybindingsTable}>
        <thead>
          <tr>
            <th>Command</th>
            <th>Shortcut</th>
            <th>When</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {filtered.map((cmd) => {
            const isEditing = edit?.commandId === cmd.id;
            const candidateWhen = cmd.default_when ?? cmd.when ?? "";
            // User commands carry at most one key (it lives on their
            // [[command]] entry, not in [[bind]]).
            const singleKey = cmd.id.startsWith("user_command_");

            // Conflict / validation state — only computed in edit mode.
            const keysToSave = isEditing
              ? [...new Set(edit!.keys.filter((k) => k && isCompleteKey(k)))]
              : [];
            const conflicts: Conflict[] = isEditing
              ? keysToSave.flatMap((k) =>
                  detectConflicts(
                    k,
                    candidateWhen,
                    edit!.commandId,
                    bindings,
                    commandsById,
                  ),
                )
              : [];
            const hardConflicts = conflicts.filter((c) => c.kind === "hard");
            const softConflicts = conflicts.filter((c) => c.kind === "soft");
            const ackKeys = keysToSave.join(",");
            const ackMatches =
              !!acked && acked.keys === ackKeys && acked.when === candidateWhen;
            const canSave = hardConflicts.length === 0 || ackMatches;
            const showBanner =
              isEditing &&
              (hardConflicts.length > 0 || softConflicts.length > 0 || !!error);

            return (
              <Fragment key={cmd.id}>
                <tr
                  className={[
                    cmd.user_overridden ? styles.kbRowModified : "",
                    isEditing ? styles.kbRowEditing : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  onDoubleClick={() => !isEditing && startEdit(cmd)}
                >
                  <td>
                    {cmd.name}
                    {cmd.user_overridden && !isEditing && (
                      <span className={styles.kbModifiedDot} title="Modified">
                        •
                      </span>
                    )}
                  </td>
                  <td>
                    {isEditing && edit ? (
                      <div className={styles.kbKeyList}>
                        {edit.keys.map((key, i) => (
                          <div className={styles.kbKeyRow} key={i}>
                            <KeyCaptureInput
                              value={key}
                              onChange={(k) => {
                                const keys = [...edit.keys];
                                keys[i] = k;
                                setEdit({ ...edit, keys });
                                setAcked(null);
                              }}
                              autoFocus={i === 0}
                            />
                            {edit.keys.length > 1 && (
                              <button
                                type="button"
                                className={styles.kbKeyRemove}
                                title="Remove this binding"
                                onClick={() => {
                                  setEdit({
                                    ...edit,
                                    keys: edit.keys.filter((_, j) => j !== i),
                                  });
                                  setAcked(null);
                                }}
                              >
                                ×
                              </button>
                            )}
                          </div>
                        ))}
                        {!singleKey && (
                          <button
                            type="button"
                            className={styles.kbKeyAdd}
                            onClick={() =>
                              setEdit({ ...edit, keys: [...edit.keys, ""] })
                            }
                            disabled={edit.keys.some((k) => !k)}
                          >
                            + Add key
                          </button>
                        )}
                      </div>
                    ) : cmd.shortcuts.length > 0 ? (
                      <span className={styles.kbShortcutList}>
                        {cmd.shortcuts.map((k, i) => {
                          const warn = overlaps.get(cmd.id)?.get(k);
                          return (
                            <span className={styles.kbChipLine} key={i}>
                              {shortcutChips(k)}
                              {warn && <ConflictMark title={warn} />}
                            </span>
                          );
                        })}
                      </span>
                    ) : (
                      <span className={styles.noShortcut}>&mdash;</span>
                    )}
                  </td>
                  <td>
                    <span className={styles.whenLabel}>
                      {whenLabel(cmd.when ?? cmd.default_when)}
                    </span>
                  </td>
                  <td className={styles.kbRowActions}>
                    {!isEditing && (
                      <>
                        <button onClick={() => startEdit(cmd)}>Edit</button>
                        {cmd.user_overridden && (
                          <button
                            onClick={() => reset(cmd)}
                            title="Reset to default"
                          >
                            Reset
                          </button>
                        )}
                      </>
                    )}
                    {isEditing && edit && (
                      <>
                        <button
                          className="suggested"
                          onClick={() => save(cmd, keysToSave)}
                          disabled={!canSave}
                        >
                          Save
                        </button>
                        <button onClick={cancelEdit}>Cancel</button>
                        {cmd.default_keys.length > 0 && (
                          <button
                            onClick={() => {
                              reset(cmd);
                              cancelEdit();
                            }}
                            disabled={
                              !cmd.user_overridden &&
                              sameKeySet(keysToSave, cmd.default_keys)
                            }
                            title="Restore the built-in defaults"
                          >
                            Reset
                          </button>
                        )}
                      </>
                    )}
                  </td>
                </tr>

                {showBanner && (
                  <tr className={styles.kbDetailRow}>
                    <td></td>
                    <td colSpan={3}>
                      {hardConflicts.length > 0 && (
                        <div className={styles.kbBannerError}>
                          <span>
                            Already used by{" "}
                            {hardConflicts
                              .map(
                                (c) =>
                                  `${c.commandName} (${whenLabel(c.binding.when)})`,
                              )
                              .join(", ")}
                            .
                          </span>
                          {!ackMatches && (
                            <button
                              onClick={() =>
                                setAcked({
                                  keys: ackKeys,
                                  when: candidateWhen,
                                })
                              }
                              title="Acknowledge the conflict — Save will then overwrite the existing binding"
                            >
                              Override
                            </button>
                          )}
                        </div>
                      )}

                      {hardConflicts.length === 0 &&
                        softConflicts.length > 0 && (
                          <div className={styles.kbBannerWarn}>
                            Also used by{" "}
                            {softConflicts
                              .map(
                                (c) =>
                                  `${c.commandName} (${whenLabel(c.binding.when)})`,
                              )
                              .join(", ")}
                          </div>
                        )}

                      {error && (
                        <div className={styles.kbBannerError}>{error}</div>
                      )}
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
