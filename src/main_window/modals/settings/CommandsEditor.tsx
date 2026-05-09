import { useMemo, useState } from "react";

import { commands as ipc } from "../../../lib/bindings";
import { unwrap, safeSilent } from "../../../lib/ipc";
import {
  CommandInfo,
  ResolvedBinding,
  UserCommandEntry,
} from "../../../lib/preferences";
import styles from "../SettingsEditor.module.scss";
import {
  detectConflicts,
  isCompleteKey,
  KeyCaptureInput,
  shortcutChips,
  whenLabel,
} from "./keybindingHelpers";

function emptyCommand(): UserCommandEntry {
  return { title: "", run: "", terminal: false };
}

export function CommandsEditor({
  commands,
  bindings,
  allCommands,
}: {
  commands: UserCommandEntry[];
  bindings: ResolvedBinding[];
  allCommands: CommandInfo[];
}) {
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [editForm, setEditForm] = useState<UserCommandEntry>(emptyCommand());
  const [isAdding, setIsAdding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Tracks the (key, when) the user has explicitly acknowledged as a
  // conflict — same pattern as KeybindingsEditor.
  const [acked, setAcked] = useState<{ key: string; when: string } | null>(
    null,
  );

  const commandsById = useMemo(() => {
    const m = new Map<string, CommandInfo>();
    for (const c of allCommands) m.set(c.id, c);
    return m;
  }, [allCommands]);

  const startEdit = (index: number) => {
    setEditingIndex(index);
    setEditForm({ ...commands[index] });
    setIsAdding(false);
    setError(null);
    setAcked(null);
  };

  const startAdd = () => {
    setEditingIndex(null);
    setEditForm(emptyCommand());
    setIsAdding(true);
    setError(null);
    setAcked(null);
  };

  const cancelEdit = () => {
    setEditingIndex(null);
    setIsAdding(false);
    setError(null);
    setAcked(null);
  };

  // The keybinding for a user command always resolves with `pane_focused` —
  // see `resolve_bindings` in preferences/mod.rs. The editForm's `when` field
  // is a separate concept (file/directory/selection match for the command's
  // run condition), not the dispatch context.
  const KEYBINDING_WHEN = "pane_focused";

  // The "own" command id while editing/adding — used so conflict detection
  // doesn't flag the command's own current binding as a conflict with itself.
  const ownCommandId =
    editingIndex !== null ? `user_command_${editingIndex}` : "__new__";

  const saveEdit = async () => {
    try {
      // We do NOT pre-clear conflicting bindings: the new binding wins by
      // resolution order, and the user can later Reset either side to
      // reclaim — Reset is symmetric and explicit.
      if (isAdding) {
        await unwrap(ipc.addUserCommandEntry(editForm));
      } else if (editingIndex !== null) {
        await unwrap(ipc.updateUserCommandEntry(editingIndex, editForm));
      }
      setEditingIndex(null);
      setIsAdding(false);
      setError(null);
    } catch (e: any) {
      setError(typeof e === "string" ? e : (e?.message ?? String(e)));
    }
  };

  const removeCommand = async (index: number) => {
    await safeSilent(ipc.removeUserCommandEntry(index));
    if (editingIndex === index) {
      setEditingIndex(null);
    }
  };

  const renderForm = () => {
    const candidateKey = editForm.key ?? "";
    const conflicts = candidateKey
      ? detectConflicts(
          candidateKey,
          KEYBINDING_WHEN,
          ownCommandId,
          bindings,
          commandsById,
        )
      : [];
    const hardConflicts = conflicts.filter((c) => c.kind === "hard");
    const softConflicts = conflicts.filter((c) => c.kind === "soft");
    const keyValid = !candidateKey || isCompleteKey(candidateKey);
    const ackMatches =
      !!acked && acked.key === candidateKey && acked.when === KEYBINDING_WHEN;
    const canSave = keyValid && (hardConflicts.length === 0 || ackMatches);

    return (
      <div className={styles.commandForm}>
        <label>
          Title
          <input
            type="text"
            value={editForm.title}
            onChange={(e) =>
              setEditForm({ ...editForm, title: e.target.value })
            }
            autoFocus
          />
        </label>
        <label>
          Run
          <textarea
            value={editForm.run}
            onChange={(e) => setEditForm({ ...editForm, run: e.target.value })}
            rows={3}
            style={{ fontFamily: "monospace" }}
          />
        </label>
        <div className={styles.commandFormRow}>
          <label>
            Key
            <KeyCaptureInput
              value={candidateKey}
              onChange={(k) => {
                setEditForm({ ...editForm, key: k || undefined });
                setAcked(null);
              }}
              size="regular"
            />
          </label>
          <label>
            Applies to
            <select
              value={editForm.applies_to ?? "any"}
              onChange={(e) =>
                setEditForm({
                  ...editForm,
                  applies_to:
                    e.target.value === "any" ? undefined : e.target.value,
                })
              }
            >
              <option value="any">Any focused item</option>
              <option value="file">Files only</option>
              <option value="directory">Directories only</option>
              <option value="selection">Selection</option>
            </select>
          </label>
          <label>
            <span className={styles.checkboxSpacer} aria-hidden="true">
              &nbsp;
            </span>
            <span className={styles.checkboxRow}>
              <input
                type="checkbox"
                checked={editForm.terminal}
                onChange={(e) =>
                  setEditForm({ ...editForm, terminal: e.target.checked })
                }
              />
              Run in terminal
            </span>
          </label>
        </div>

        {!keyValid && (
          <div className={styles.kbBannerWarn}>
            Press a non-modifier key (letter, number, function key, etc.).
          </div>
        )}

        {hardConflicts.length > 0 && (
          <div className={styles.kbBannerError}>
            <span>
              Already used by{" "}
              {hardConflicts
                .map((c) => `${c.commandName} (${whenLabel(c.binding.when)})`)
                .join(", ")}
              .
            </span>
            {!ackMatches && (
              <button
                onClick={() =>
                  setAcked({ key: candidateKey, when: KEYBINDING_WHEN })
                }
                disabled={!keyValid}
                title="Acknowledge the conflict — Save will then overwrite the existing binding"
              >
                Override
              </button>
            )}
          </div>
        )}

        {hardConflicts.length === 0 && softConflicts.length > 0 && (
          <div className={styles.kbBannerWarn}>
            Also used by{" "}
            {softConflicts
              .map((c) => `${c.commandName} (${whenLabel(c.binding.when)})`)
              .join(", ")}
            . Whichever context applies will win.
          </div>
        )}

        {error && <div className={styles.kbBannerError}>{error}</div>}

        <div className={styles.commandFormActions}>
          {!isAdding && editingIndex !== null && (
            <button onClick={() => removeCommand(editingIndex)}>Delete</button>
          )}
          <div className={styles.commandFormPrimary}>
            <button onClick={cancelEdit}>Cancel</button>
            <button
              className="suggested"
              onClick={() => saveEdit()}
              disabled={!canSave}
            >
              Save
            </button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className={styles.settingsList}>
      {commands.length === 0 && !isAdding && (
        <div
          style={{ color: "var(--color-fg-muted)", padding: "var(--space-4)" }}
        >
          No user commands configured
        </div>
      )}
      {commands.map((cmd, i) => (
        <div key={i} className={styles.userCmdEntry}>
          {editingIndex === i ? (
            renderForm()
          ) : (
            <div className={styles.userCmdRow}>
              <div className={styles.settingInfo}>
                <div className={styles.userCmdHeader}>
                  <span className={styles.settingLabel}>
                    {cmd.title || "(untitled)"}
                  </span>
                  {cmd.key && (
                    <span className={styles.userCmdShortcut}>
                      {shortcutChips(cmd.key)}
                    </span>
                  )}
                </div>
                {cmd.run.trim() ? (
                  <pre className={styles.userCmdCode}>{cmd.run}</pre>
                ) : (
                  <pre className={styles.userCmdCodeEmpty}>(no command)</pre>
                )}
                {(cmd.applies_to || cmd.terminal) && (
                  <div className={styles.userCmdTags}>
                    {cmd.applies_to && (
                      <span className={styles.userCmdTag}>
                        applies to {cmd.applies_to}
                      </span>
                    )}
                    {cmd.terminal && (
                      <span className={styles.userCmdTag}>terminal</span>
                    )}
                  </div>
                )}
              </div>
              <div className={styles.settingControl}>
                <button onClick={() => startEdit(i)}>Edit</button>
              </div>
            </div>
          )}
        </div>
      ))}
      {isAdding && <div className={styles.userCmdEntry}>{renderForm()}</div>}
      {!isAdding && (
        <div style={{ padding: "var(--space-4) 0" }}>
          <button onClick={startAdd}>Add Command</button>
        </div>
      )}
      <div className={styles.templateHelp}>
        <div className={styles.templateHelpTitle}>Template Reference</div>
        <details>
          <summary>Details</summary>
          <div className={styles.templateHelpBody}>
            <p>
              The <b>Run</b> field uses{" "}
              <a
                href="https://docs.rs/minijinja"
                target="_blank"
                rel="noreferrer"
              >
                Jinja2
              </a>{" "}
              templates.
            </p>

            <h4>Variables</h4>
            <table>
              <tbody>
                <tr>
                  <td>
                    <code>{"{{ dir }}"}</code>
                  </td>
                  <td>Current pane directory</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ other_dir }}"}</code>
                  </td>
                  <td>Other pane directory</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ hostname }}"}</code>
                  </td>
                  <td>Machine hostname</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ env.HOME }}"}</code>
                  </td>
                  <td>Environment variable (any name)</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.name }}"}</code>
                  </td>
                  <td>Focused file name</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.path }}"}</code>
                  </td>
                  <td>Focused file full path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.stem }}"}</code>
                  </td>
                  <td>Filename without extension</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.ext }}"}</code>
                  </td>
                  <td>File extension</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.size }}"}</code>
                  </td>
                  <td>File size in bytes</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.modified }}"}</code>
                  </td>
                  <td>Last modified (Unix timestamp)</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ file.is_dir }}"}</code>
                  </td>
                  <td>Whether focused item is a directory</td>
                </tr>
                <tr>
                  <td>
                    <code>{"{{ files }}"}</code>
                  </td>
                  <td>Selected files (or focused file)</td>
                </tr>
              </tbody>
            </table>

            <h4>Filters</h4>
            <table>
              <tbody>
                <tr>
                  <td>
                    <code>{"shell_quote"}</code>
                  </td>
                  <td>Shell-escape a string</td>
                </tr>
                <tr>
                  <td>
                    <code>{"basename"}</code>
                  </td>
                  <td>Extract filename from path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"dirname"}</code>
                  </td>
                  <td>Extract directory from path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"stem"}</code>
                  </td>
                  <td>Filename without extension</td>
                </tr>
                <tr>
                  <td>
                    <code>{"ext"}</code>
                  </td>
                  <td>Extract extension from path</td>
                </tr>
                <tr>
                  <td>
                    <code>{"regex_replace(pattern, replacement)"}</code>
                  </td>
                  <td>Regex substitution</td>
                </tr>
                <tr>
                  <td>
                    <code>{"join_path"}</code>
                  </td>
                  <td>Join path segments</td>
                </tr>
              </tbody>
            </table>

            <h4>Functions</h4>
            <table>
              <tbody>
                <tr>
                  <td>
                    <code>{'prompt("Label", default="")'}</code>
                  </td>
                  <td>Show input dialog before running</td>
                </tr>
                <tr>
                  <td>
                    <code>{'confirm("Are you sure?")'}</code>
                  </td>
                  <td>Show confirmation — aborts if declined</td>
                </tr>
              </tbody>
            </table>

            <h4>Examples</h4>
            <pre className={styles.userCmdCode}>
              {
                "tar czf {{ file.stem }}.tar.gz {{ files | map(attribute='name') | shell_quote | join(' ') }}"
              }
            </pre>
            <pre className={styles.userCmdCode}>
              {
                'mv {{ file.name | shell_quote }} {{ prompt("New name", file.name) | shell_quote }}'
              }
            </pre>
            <pre className={styles.userCmdCode}>
              {
                '{% do confirm("Play " ~ file.name ~ "?" ) %} paplay {{ file.path | shell_quote }}'
              }
            </pre>
          </div>
        </details>
      </div>
    </div>
  );
}
