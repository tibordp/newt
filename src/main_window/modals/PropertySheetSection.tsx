import { useEffect, useMemo, useState } from "react";
import {
  type PropertyField,
  type PropertyGrant,
  type PropertyGrantee,
  type PropertyPatchOp,
  type PropertySheetState,
} from "../../lib/bindings";
import styles from "./Properties.module.scss";

// Extended-properties section of the Properties dialog: renders a
// generically-typed sheet (pushed via ModalDataKind.Properties.sheet)
// and reports the accumulated patch ops upward. All edit state here is
// ephemeral form state; the sheet itself comes from Rust.

type MapEditState = {
  edited: Record<string, string>;
  deleted: string[];
  added: { id: number; key: string; value: string }[];
};

const EMPTY_MAP_EDIT: MapEditState = { edited: {}, deleted: [], added: [] };

function granteeIdentifier(g: PropertyGrantee): string {
  if ("user" in g) return g.user.id;
  if ("group" in g) return g.group.uri;
  return g.email.address;
}

function granteeKind(g: PropertyGrantee): "user" | "group" | "email" {
  if ("user" in g) return "user";
  if ("group" in g) return "group";
  return "email";
}

function makeGrantee(
  kind: "user" | "group" | "email",
  identifier: string,
): PropertyGrantee {
  if (kind === "user") return { user: { id: identifier, display_name: null } };
  if (kind === "group") return { group: { uri: identifier } };
  return { email: { address: identifier } };
}

function granteeLabel(g: PropertyGrantee): string {
  if ("user" in g && g.user.display_name) {
    return `${g.user.display_name} (${g.user.id})`;
  }
  return granteeIdentifier(g);
}

function normalizeGrants(grants: PropertyGrant[]): PropertyGrant[] {
  return grants.filter((g) => granteeIdentifier(g.grantee).trim() !== "");
}

export function PropertySheetSection({
  state,
  onOpsChange,
}: {
  state: PropertySheetState;
  onOpsChange: (ops: PropertyPatchOp[]) => void;
}) {
  const [textEdits, setTextEdits] = useState<Record<string, string>>({});
  const [choiceEdits, setChoiceEdits] = useState<Record<string, string>>({});
  const [mapEdits, setMapEdits] = useState<Record<string, MapEditState>>({});
  const [grantEdits, setGrantEdits] = useState<Record<string, PropertyGrant[]>>(
    {},
  );
  const [nextRowId, setNextRowId] = useState(0);

  const fields = useMemo(
    () =>
      state.status === "loaded"
        ? state.sheet.groups.flatMap((g) => g.fields)
        : [],
    [state],
  );

  const ops = useMemo(() => {
    const ops: PropertyPatchOp[] = [];
    for (const field of fields) {
      if (!field.editable) continue;
      const v = field.value;
      if ("text" in v || "choice" in v) {
        const original = "text" in v ? v.text.value : v.choice.value;
        const edits = "text" in v ? textEdits : choiceEdits;
        const edit = edits[field.key];
        if (edit !== undefined && edit !== original) {
          ops.push({ key: field.key, op: { set: { value: edit } } });
        }
      } else if ("map" in v) {
        const edit = mapEdits[field.key];
        if (!edit) continue;
        const set: Record<string, string> = {};
        for (const [key, value] of Object.entries(edit.edited)) {
          if (value !== v.map.entries[key]) set[key] = value;
        }
        for (const row of edit.added) {
          if (row.key.trim() !== "") set[row.key] = row.value;
        }
        if (Object.keys(set).length > 0 || edit.deleted.length > 0) {
          ops.push({
            key: field.key,
            op: { map_patch: { set, delete: edit.deleted } },
          });
        }
      } else if ("grants" in v) {
        const edit = grantEdits[field.key];
        if (edit === undefined) continue;
        const grants = normalizeGrants(edit);
        if (JSON.stringify(grants) !== JSON.stringify(v.grants.value ?? null)) {
          ops.push({ key: field.key, op: { replace_grants: { grants } } });
        }
      }
    }
    return ops;
  }, [fields, textEdits, choiceEdits, mapEdits, grantEdits]);

  useEffect(() => {
    onOpsChange(ops);
  }, [ops, onOpsChange]);

  if (state.status === "hidden") return null;
  if (state.status === "loading") {
    return <div className={styles.sheetStatus}>Loading properties…</div>;
  }
  if (state.status === "failed") {
    return (
      <div className={styles.sheetStatus}>
        Failed to load properties: {state.error}
      </div>
    );
  }

  return (
    <>
      {state.sheet.groups.map((group) => (
        <div key={group.label}>
          <div className={styles.permSectionHeader}>{group.label}</div>
          {group.fields.map((field) => (
            <FieldEditor
              key={field.key}
              field={field}
              textEdits={textEdits}
              setTextEdits={setTextEdits}
              choiceEdits={choiceEdits}
              setChoiceEdits={setChoiceEdits}
              mapEdit={mapEdits[field.key] ?? EMPTY_MAP_EDIT}
              setMapEdit={(s) => setMapEdits((m) => ({ ...m, [field.key]: s }))}
              grantEdit={grantEdits[field.key]}
              setGrantEdit={(g) =>
                setGrantEdits((m) => ({ ...m, [field.key]: g }))
              }
              allocRowId={() => {
                const id = nextRowId;
                setNextRowId(id + 1);
                return id;
              }}
            />
          ))}
        </div>
      ))}
    </>
  );
}

function FieldEditor({
  field,
  textEdits,
  setTextEdits,
  choiceEdits,
  setChoiceEdits,
  mapEdit,
  setMapEdit,
  grantEdit,
  setGrantEdit,
  allocRowId,
}: {
  field: PropertyField;
  textEdits: Record<string, string>;
  setTextEdits: React.Dispatch<React.SetStateAction<Record<string, string>>>;
  choiceEdits: Record<string, string>;
  setChoiceEdits: React.Dispatch<React.SetStateAction<Record<string, string>>>;
  mapEdit: MapEditState;
  setMapEdit: (s: MapEditState) => void;
  grantEdit: PropertyGrant[] | undefined;
  setGrantEdit: (g: PropertyGrant[]) => void;
  allocRowId: () => number;
}) {
  const v = field.value;

  if ("text" in v) {
    const original = v.text.value;
    const mixed = original === null && !field.write_only;
    if (!field.editable) {
      return (
        <FieldRow label={field.label}>
          <span>{original ?? "(mixed)"}</span>
        </FieldRow>
      );
    }
    return (
      <FieldRow label={field.label}>
        <input
          type="text"
          className={styles.sheetTextInput}
          value={textEdits[field.key] ?? original ?? ""}
          placeholder={mixed ? "(mixed)" : undefined}
          autoComplete="off"
          autoCorrect="off"
          onChange={(e) => {
            const value = e.target.value;
            setTextEdits((m) => {
              // On a mixed original, an emptied input means "leave alone",
              // not "set to empty on all".
              if (mixed && value === "") {
                const rest = { ...m };
                delete rest[field.key];
                return rest;
              }
              return { ...m, [field.key]: value };
            });
          }}
        />
      </FieldRow>
    );
  }

  if ("choice" in v) {
    const original = v.choice.value;
    const unset = field.write_only
      ? "(keep current)"
      : original === null
        ? "(mixed)"
        : null;
    if (!field.editable) {
      return (
        <FieldRow label={field.label}>
          <span>{original ?? "(mixed)"}</span>
        </FieldRow>
      );
    }
    return (
      <FieldRow label={field.label}>
        <select
          value={choiceEdits[field.key] ?? original ?? ""}
          onChange={(e) => {
            const value = e.target.value;
            setChoiceEdits((m) => {
              if (value === "") {
                const rest = { ...m };
                delete rest[field.key];
                return rest;
              }
              return { ...m, [field.key]: value };
            });
          }}
        >
          {unset !== null && <option value="">{unset}</option>}
          {v.choice.choices.map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
      </FieldRow>
    );
  }

  if ("map" in v) {
    const entries = Object.entries(v.map.entries);
    return (
      <FieldRow label={field.label} block>
        <div className={styles.mapGrid}>
          {entries.map(([key, value]) => {
            const deleted = mapEdit.deleted.includes(key);
            return (
              <div
                key={key}
                className={
                  deleted
                    ? `${styles.mapRow} ${styles.mapRowDeleted}`
                    : styles.mapRow
                }
              >
                <span className={styles.mapKey}>{key}</span>
                <input
                  type="text"
                  value={mapEdit.edited[key] ?? value ?? ""}
                  placeholder={value === null ? "(mixed)" : undefined}
                  disabled={!field.editable || deleted}
                  autoComplete="off"
                  autoCorrect="off"
                  onChange={(e) => {
                    const edited = { ...mapEdit.edited };
                    // A mixed value edited back to empty = leave alone.
                    if (value === null && e.target.value === "") {
                      delete edited[key];
                    } else {
                      edited[key] = e.target.value;
                    }
                    setMapEdit({ ...mapEdit, edited });
                  }}
                />
                {field.editable && (
                  <button
                    type="button"
                    className={styles.sheetRowButton}
                    title={deleted ? "Restore" : "Remove"}
                    onClick={() =>
                      setMapEdit({
                        ...mapEdit,
                        deleted: deleted
                          ? mapEdit.deleted.filter((k) => k !== key)
                          : [...mapEdit.deleted, key],
                      })
                    }
                  >
                    {deleted ? "↺" : "×"}
                  </button>
                )}
              </div>
            );
          })}
          {mapEdit.added.map((row) => (
            <div key={row.id} className={styles.mapRow}>
              <input
                type="text"
                value={row.key}
                placeholder="key"
                autoComplete="off"
                autoCorrect="off"
                onChange={(e) =>
                  setMapEdit({
                    ...mapEdit,
                    added: mapEdit.added.map((r) =>
                      r.id === row.id ? { ...r, key: e.target.value } : r,
                    ),
                  })
                }
              />
              <input
                type="text"
                value={row.value}
                placeholder="value"
                autoComplete="off"
                autoCorrect="off"
                onChange={(e) =>
                  setMapEdit({
                    ...mapEdit,
                    added: mapEdit.added.map((r) =>
                      r.id === row.id ? { ...r, value: e.target.value } : r,
                    ),
                  })
                }
              />
              <button
                type="button"
                className={styles.sheetRowButton}
                title="Remove"
                onClick={() =>
                  setMapEdit({
                    ...mapEdit,
                    added: mapEdit.added.filter((r) => r.id !== row.id),
                  })
                }
              >
                {"×"}
              </button>
            </div>
          ))}
          {field.editable && (
            <button
              type="button"
              className={styles.sheetAddButton}
              onClick={() =>
                setMapEdit({
                  ...mapEdit,
                  added: [
                    ...mapEdit.added,
                    { id: allocRowId(), key: "", value: "" },
                  ],
                })
              }
            >
              Add entry
            </button>
          )}
        </div>
      </FieldRow>
    );
  }

  // Grants
  const original = v.grants.value;
  const grants = grantEdit ?? original;

  if (grants === null || grants === undefined) {
    // Differing grant lists across the selection: the only available
    // action is an explicit whole-list replace.
    return (
      <FieldRow label={field.label} block>
        <div className={styles.mixedNote}>
          Selection has differing grants.
          {field.editable && (
            <button
              type="button"
              className={styles.sheetAddButton}
              onClick={() => setGrantEdit([])}
            >
              Replace grants on all…
            </button>
          )}
        </div>
      </FieldRow>
    );
  }

  return (
    <FieldRow label={field.label} block>
      <div className={styles.mapGrid}>
        {grants.map((grant, i) => (
          <div key={i} className={styles.grantRow}>
            {field.editable ? (
              <>
                <select
                  value={granteeKind(grant.grantee)}
                  onChange={(e) =>
                    setGrantEdit(
                      grants.map((g, j) =>
                        j === i
                          ? {
                              ...g,
                              grantee: makeGrantee(
                                e.target.value as "user" | "group" | "email",
                                granteeIdentifier(g.grantee),
                              ),
                            }
                          : g,
                      ),
                    )
                  }
                >
                  <option value="user">User ID</option>
                  <option value="group">Group URI</option>
                  <option value="email">Email</option>
                </select>
                <input
                  type="text"
                  value={granteeIdentifier(grant.grantee)}
                  autoComplete="off"
                  autoCorrect="off"
                  onChange={(e) =>
                    setGrantEdit(
                      grants.map((g, j) =>
                        j === i
                          ? {
                              ...g,
                              grantee: makeGrantee(
                                granteeKind(g.grantee),
                                e.target.value,
                              ),
                            }
                          : g,
                      ),
                    )
                  }
                />
                <select
                  value={grant.permission}
                  onChange={(e) =>
                    setGrantEdit(
                      grants.map((g, j) =>
                        j === i ? { ...g, permission: e.target.value } : g,
                      ),
                    )
                  }
                >
                  {/* Keep an out-of-vocabulary permission selectable. */}
                  {!v.grants.permission_choices.includes(grant.permission) && (
                    <option value={grant.permission}>{grant.permission}</option>
                  )}
                  {v.grants.permission_choices.map((p) => (
                    <option key={p} value={p}>
                      {p}
                    </option>
                  ))}
                </select>
                <button
                  type="button"
                  className={styles.sheetRowButton}
                  title="Remove"
                  onClick={() => setGrantEdit(grants.filter((_, j) => j !== i))}
                >
                  {"×"}
                </button>
              </>
            ) : (
              <span>
                {granteeLabel(grant.grantee)} — {grant.permission}
              </span>
            )}
          </div>
        ))}
        {field.editable && (
          <button
            type="button"
            className={styles.sheetAddButton}
            onClick={() =>
              setGrantEdit([
                ...grants,
                {
                  grantee: makeGrantee("user", ""),
                  permission: v.grants.permission_choices[0] ?? "",
                },
              ])
            }
          >
            Add grant
          </button>
        )}
      </div>
    </FieldRow>
  );
}

function FieldRow({
  label,
  block,
  children,
}: {
  label: string;
  block?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div className={block ? styles.sheetFieldBlock : styles.sheetFieldRow}>
      <span className={styles.sheetFieldLabel}>{label}</span>
      {children}
    </div>
  );
}
