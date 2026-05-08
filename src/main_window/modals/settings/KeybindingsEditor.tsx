import { invoke } from "@tauri-apps/api/core";
import { Fragment, useMemo, useState } from "react";

import { CommandInfo, ResolvedBinding } from "../../../lib/preferences";
import styles from "../SettingsEditor.module.scss";
import {
  detectConflicts,
  isCompleteKey,
  KeyCaptureInput,
  shortcutChips,
  whenLabel,
} from "./keybindingHelpers";

type EditState = {
  commandId: string;
  key: string;
};

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
  // Tracks the (key, when) the user has explicitly acknowledged as a
  // conflict. Save remains gated until ack matches the current draft, and
  // changing the key invalidates the ack.
  const [acked, setAcked] = useState<{ key: string; when: string } | null>(
    null,
  );

  const commandsById = useMemo(() => {
    const m = new Map<string, CommandInfo>();
    for (const c of commands) m.set(c.id, c);
    return m;
  }, [commands]);

  const filtered = useMemo(() => {
    if (!filter) return commands;
    const lower = filter.toLowerCase();
    return commands.filter(
      (c) =>
        c.name.toLowerCase().includes(lower) ||
        c.id.toLowerCase().includes(lower) ||
        (c.shortcut && c.shortcut.toLowerCase().includes(lower)) ||
        (c.when && c.when.toLowerCase().includes(lower)),
    );
  }, [commands, filter]);

  const startEdit = (cmd: CommandInfo) => {
    setError(null);
    setAcked(null);
    setEdit({
      commandId: cmd.id,
      key: cmd.shortcut ?? "",
    });
  };

  const cancelEdit = () => {
    setEdit(null);
    setError(null);
    setAcked(null);
  };

  const save = async (cmd: CommandInfo) => {
    if (!edit) return;
    try {
      // The when clause is a property of the command, not the user's choice —
      // keep whatever the command currently uses (its default for built-ins).
      // We do NOT pre-clear conflicting bindings: the new binding wins by
      // resolution order and the loser's row visibly shows as shadowed. To
      // reclaim, the user can Reset either side — Reset is symmetric.
      await invoke("set_command_keybinding", {
        commandId: edit.commandId,
        newKey: edit.key || null,
        newWhen: cmd.default_when ?? cmd.when ?? null,
      });
      setEdit(null);
      setError(null);
    } catch (e: any) {
      setError(typeof e === "string" ? e : (e?.message ?? String(e)));
    }
  };

  const reset = async (cmd: CommandInfo) => {
    try {
      await invoke("reset_command_keybinding", { commandId: cmd.id });
    } catch (e) {
      console.error("Failed to reset keybinding:", e);
    }
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

            // Conflict / validation state — only computed in edit mode.
            const conflicts =
              isEditing && edit && edit.key
                ? detectConflicts(
                    edit.key,
                    candidateWhen,
                    edit.commandId,
                    bindings,
                    commandsById,
                  )
                : [];
            const hardConflicts = conflicts.filter((c) => c.kind === "hard");
            const softConflicts = conflicts.filter((c) => c.kind === "soft");
            const valid = !isEditing || !edit?.key || isCompleteKey(edit.key);
            const ackMatches =
              !!acked &&
              !!edit &&
              acked.key === edit.key &&
              acked.when === candidateWhen;
            const canSave = valid && (hardConflicts.length === 0 || ackMatches);
            const showBanner =
              isEditing &&
              (!valid ||
                hardConflicts.length > 0 ||
                softConflicts.length > 0 ||
                !!error);

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
                      <KeyCaptureInput
                        value={edit.key}
                        onChange={(k) => {
                          setEdit({ ...edit, key: k });
                          setAcked(null);
                        }}
                        autoFocus
                      />
                    ) : cmd.shortcut_display.length > 0 ? (
                      shortcutChips(cmd.shortcut!)
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
                          onClick={() => save(cmd)}
                          disabled={!canSave}
                        >
                          Save
                        </button>
                        <button onClick={cancelEdit}>Cancel</button>
                        {cmd.default_key && (
                          <button
                            onClick={() => {
                              reset(cmd);
                              cancelEdit();
                            }}
                            disabled={
                              !cmd.user_overridden &&
                              edit.key === cmd.default_key
                            }
                            title="Restore the built-in default"
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
                      {!valid && (
                        <div className={styles.kbBannerWarn}>
                          Press a non-modifier key (letter, number, function
                          key, etc.).
                        </div>
                      )}

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
                                  key: edit!.key,
                                  when: candidateWhen,
                                })
                              }
                              disabled={!valid}
                              title="Acknowledge the conflict — Save will then overwrite the existing binding"
                            >
                              Override
                            </button>
                          )}
                        </div>
                      )}

                      {showBanner &&
                        hardConflicts.length === 0 &&
                        softConflicts.length > 0 && (
                          <div className={styles.kbBannerWarn}>
                            Also used by{" "}
                            {softConflicts
                              .map(
                                (c) =>
                                  `${c.commandName} (${whenLabel(c.binding.when)})`,
                              )
                              .join(", ")}
                            . Whichever context applies will win.
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
