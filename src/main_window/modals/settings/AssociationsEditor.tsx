import { useCallback, useEffect, useRef, useState } from "react";

import {
  commands as ipc,
  type AssociationCell,
  type AssociationChange,
  type AssociationFocus,
  type AssociationKind,
  type AssociationRow,
  type AssociationTable,
  type BrowseFormat,
  type EditorLanguage,
  type EnterChoice,
  type Origin,
  type ViewerMode,
} from "../../../lib/bindings";
import { DropdownMenu as DM } from "../../../lib/menus";
import { safeSilent } from "../../../lib/ipc";
import type {
  PreferencesState,
  UserCommandEntry,
} from "../../../lib/preferences";
import menuStyles from "../../Menu.module.scss";
import styles from "../SettingsEditor.module.scss";

const FORMAT_LABELS: Record<BrowseFormat, string> = {
  zip: "ZIP",
  "7z": "7-Zip",
  tar: "tar, cpio or ar",
  compressed: "compressed file",
  disc: "disc image",
};
const FORMATS = Object.keys(FORMAT_LABELS) as BrowseFormat[];

const VIEWER_LABELS: Record<ViewerMode, string> = {
  text: "Text",
  hex: "Hex",
  table: "Table",
  markdown: "Markdown",
  image: "Image",
  audio: "Audio",
  video: "Video",
  pdf: "PDF",
};
const VIEWER_MODES = Object.keys(VIEWER_LABELS) as ViewerMode[];

type Column = "enter" | "viewer" | "language";

const capitalize = (s: string) => s.replace(/^./, (c) => c.toUpperCase());

function enterLabel(
  choice: EnterChoice | null,
  kind: AssociationKind,
  format: BrowseFormat | null,
): string {
  switch (choice?.action) {
    case undefined:
      return kind === "file" ? "Open in default app" : "Go in";
    case "open":
      return "Open in default app";
    case "browse":
      return kind === "file"
        ? `Browse as ${format ? FORMAT_LABELS[format] : "its contents say"}`
        : "Go in";
    case "view":
      return "Open in viewer";
    case "edit":
      return "Open in editor";
    case "command":
      return `Run “${choice.command}”`;
  }
}

function originLabel(origin: Origin): string {
  switch (origin.source) {
    case "entry":
      return `from ${origin.patterns.join(", ")}${origin.profile ? " (profile)" : ""}`;
    case "built_in":
      return "built-in";
    case "file_type":
      return "by file type";
    case "default":
      return "default";
  }
}

/// The first item of every cell's menu: what applies when the row sets
/// nothing, and where that comes from.
function defaultItem(label: string, origin: Origin): string {
  return origin.source === "default"
    ? `Default — ${label}`
    : `Default — ${label} (${originLabel(origin)})`;
}

/// The value a cell shows: its own when set, else what it inherits.
function effective<T>(cell: AssociationCell<T>): T | null {
  return cell.set ?? cell.inherited;
}

/// The Enter radio value of a row's own setting.
function enterValue(row: AssociationRow): string {
  const set = row.enter.set;
  if (!set) return "default";
  if (set.action === "command") return `command:${set.command}`;
  if (set.action === "browse" && row.kind === "file") {
    return `browse:${row.format.set ?? ""}`;
  }
  return set.action;
}

function enterChange(value: string): AssociationChange {
  if (value === "default") return { property: "enter", value: null };
  if (value.startsWith("command:")) {
    return {
      property: "enter",
      value: { action: "command", command: value.slice("command:".length) },
    };
  }
  if (value.startsWith("browse:") && value.length > "browse:".length) {
    return {
      property: "browse_as",
      format: value.slice("browse:".length) as BrowseFormat,
    };
  }
  const action = value.replace(/:$/, "") as "open" | "browse" | "view" | "edit";
  return { property: "enter", value: { action } };
}

type FocusTarget = { pattern: string; kind: AssociationKind; col: Column };

export function AssociationsEditor({
  filter,
  onClearFilter,
  preferences,
  userCommands,
  focus,
}: {
  filter: string;
  onClearFilter: () => void;
  /// Re-read whenever preferences change.
  preferences: PreferencesState | null;
  userCommands: UserCommandEntry[];
  /// The row to start at (Change Association).
  focus: AssociationFocus | null;
}) {
  const [table, setTable] = useState<AssociationTable | null>(null);
  const [languages, setLanguages] = useState<EditorLanguage[]>([]);
  // The add row: a pattern being typed, and what it would inherit.
  const [ghostText, setGhostText] = useState(
    focus && !focus.exists ? focus.pattern : "",
  );
  const [ghostKind, setGhostKind] = useState<AssociationKind>(
    focus?.kind ?? "file",
  );
  const [ghostRow, setGhostRow] = useState<AssociationRow | null>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const focusAfterLoad = useRef<FocusTarget | null>(
    focus?.exists
      ? { pattern: focus.pattern, kind: focus.kind, col: "enter" }
      : null,
  );
  const focusGhostAfterLoad = useRef(!!focus && !focus.exists);

  useEffect(() => {
    ipc.editorLanguages().then(setLanguages).catch(console.error);
  }, []);

  useEffect(() => {
    let cancelled = false;
    ipc
      .associationTable(filter)
      .then((t) => !cancelled && setTable(t))
      .catch(console.error);
    return () => {
      cancelled = true;
    };
  }, [filter, preferences]);

  useEffect(() => {
    let cancelled = false;
    if (!ghostText.trim()) {
      setGhostRow(null);
      return;
    }
    ipc
      .associationRow(ghostText, ghostKind)
      .then((row) => !cancelled && setGhostRow(row))
      .catch(console.error);
    return () => {
      cancelled = true;
    };
  }, [ghostText, ghostKind, preferences]);

  const cellButton = (target: FocusTarget, ghost = false) =>
    gridRef.current?.querySelector<HTMLElement>(
      `[data-pattern="${CSS.escape(target.pattern)}"][data-kind="${target.kind}"][data-col="${target.col}"]${ghost ? "[data-ghost]" : ":not([data-ghost])"}`,
    );

  // A change re-reads the table, and a pattern added from the add row
  // lands among the user's: focus follows it there.
  useEffect(() => {
    const target = focusAfterLoad.current;
    if (!target) return;
    const button = cellButton(target);
    if (button) {
      focusAfterLoad.current = null;
      button.focus();
      button.scrollIntoView({ block: "nearest" });
    }
  }, [table]);

  useEffect(() => {
    if (!focusGhostAfterLoad.current || !ghostRow) return;
    const button = cellButton(
      { pattern: ghostRow.pattern, kind: ghostRow.kind, col: "enter" },
      true,
    );
    if (button) {
      focusGhostAfterLoad.current = false;
      button.focus();
    }
  }, [ghostRow]);

  const change = (row: AssociationRow, col: Column, c: AssociationChange) => {
    focusAfterLoad.current = { pattern: row.pattern, kind: row.kind, col };
    if (row === ghostRow) setGhostText("");
    safeSilent(ipc.setAssociation(row.pattern, row.kind, c));
  };

  const addCandidate = (pattern: string) => {
    setGhostText(pattern);
    focusGhostAfterLoad.current = true;
    onClearFilter();
  };

  const languageLabel = (id: string | null) =>
    id === null
      ? "Plain Text"
      : (languages.find((l) => l.id === id)?.label ?? id);

  // Arrow keys move between cells; Enter and Space open a cell's menu.
  const onGridKeyDown = useCallback((e: React.KeyboardEvent) => {
    const cell = (e.target as HTMLElement).closest<HTMLElement>("[data-cell]");
    if (!cell || !gridRef.current) return;
    // The add row's text field keeps its own caret keys.
    if (
      cell instanceof HTMLInputElement &&
      ["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)
    ) {
      return;
    }
    const cells = Array.from(
      gridRef.current.querySelectorAll<HTMLElement>("[data-cell]"),
    );
    const row = Number(cell.dataset.row);
    const col = Number(cell.dataset.cell);
    // The cell of row `r` nearest to column `c`.
    const at = (r: number, c: number) =>
      cells
        .filter((el) => Number(el.dataset.row) === r)
        .sort(
          (a, b) =>
            Math.abs(Number(a.dataset.cell) - c) -
            Math.abs(Number(b.dataset.cell) - c),
        )[0];
    const lastRow = Math.max(...cells.map((el) => Number(el.dataset.row)));
    let next: HTMLElement | undefined;
    switch (e.key) {
      case "ArrowDown":
        for (let r = row + 1; r <= lastRow && !next; r++) next = at(r, col);
        break;
      case "ArrowUp":
        for (let r = row - 1; r >= 0 && !next; r--) next = at(r, col);
        // Off the top: back to the search box that narrowed the table.
        next ??=
          gridRef.current
            .closest('[role="dialog"]')
            ?.querySelector<HTMLElement>('input[type="text"]') ?? undefined;
        break;
      case "ArrowRight":
        next = cells.find(
          (el) =>
            Number(el.dataset.row) === row && Number(el.dataset.cell) > col,
        );
        break;
      case "ArrowLeft":
        next = cells
          .filter(
            (el) =>
              Number(el.dataset.row) === row && Number(el.dataset.cell) < col,
          )
          .pop();
        break;
      case "Home":
        next = cells.find((el) => Number(el.dataset.row) === row);
        break;
      case "End":
        next = cells.filter((el) => Number(el.dataset.row) === row).pop();
        break;
      default:
        return;
    }
    // Radix opens a menu on ArrowDown unless the key is taken.
    e.preventDefault();
    next?.focus();
  }, []);

  if (!table) return <div className={styles.settingsList} />;

  const yours = table.rows.filter((r) => r.customized);
  const builtIn = table.rows.filter((r) => !r.customized);
  // Row 0 is the add row.
  let rowIndex = 1;

  const renderCells = (row: AssociationRow, ghost: boolean) => {
    const index = ghost ? 0 : rowIndex++;
    const isFile = row.kind === "file";
    const format = effective(row.format);
    const commonCell = (col: Column, colIndex: number) => ({
      "data-cell": colIndex,
      "data-row": index,
      "data-pattern": row.pattern,
      "data-kind": row.kind,
      "data-col": col,
      ...(ghost ? { "data-ghost": "" } : {}),
    });
    const onCloseAutoFocus = (col: Column) => (e: Event) => {
      // The row may have moved by the time the menu closes.
      e.preventDefault();
      const target = { pattern: row.pattern, kind: row.kind, col };
      const button =
        focusAfterLoad.current === null ? cellButton(target, ghost) : null;
      if (button) button.focus();
      else focusAfterLoad.current ??= target;
    };

    const enterCell = (
      <CellMenu
        cell={row.enter}
        label={enterLabel(effective(row.enter), row.kind, format)}
        detail={
          isFile && format && effective(row.enter)?.action !== "browse"
            ? `Browse Into as ${FORMAT_LABELS[format]}`
            : null
        }
        triggerProps={commonCell("enter", 1)}
        onCloseAutoFocus={onCloseAutoFocus("enter")}
        ariaLabel={`Enter on ${row.pattern}`}
      >
        <DM.RadioGroup
          value={enterValue(row)}
          onValueChange={(v) => change(row, "enter", enterChange(v))}
        >
          <MenuRadio value="default">
            {defaultItem(
              enterLabel(row.enter.inherited, row.kind, format),
              row.enter.origin,
            )}
          </MenuRadio>
          <DM.Separator className={menuStyles.separator} />
          {!isFile && <MenuRadio value="browse">Go in</MenuRadio>}
          <MenuRadio value="open">Open in default app</MenuRadio>
          {isFile && (
            <>
              <MenuRadio value="view">Open in viewer</MenuRadio>
              <MenuRadio value="edit">Open in editor</MenuRadio>
              <DM.Label className={menuStyles.sectionHeader}>
                Browse as
              </DM.Label>
              {FORMATS.map((f) => (
                <MenuRadio key={f} value={`browse:${f}`}>
                  {capitalize(FORMAT_LABELS[f])}
                </MenuRadio>
              ))}
            </>
          )}
          <DM.Label className={menuStyles.sectionHeader}>Run command</DM.Label>
          {userCommands.length === 0 && (
            <DM.Item className={menuStyles.item} disabled>
              No user commands yet
            </DM.Item>
          )}
          {userCommands.map((c, i) => (
            <MenuRadio key={i} value={`command:${c.title}`}>
              {c.title || "(untitled)"}
            </MenuRadio>
          ))}
        </DM.RadioGroup>
        {isFile && effective(row.enter)?.action !== "browse" && (
          <>
            <DM.Separator className={menuStyles.separator} />
            <DM.Sub>
              <DM.SubTrigger className={menuStyles.item}>
                <span className={menuStyles.checkColumn} aria-hidden />
                Browse Into as
                <span className={menuStyles.shortcut}>
                  {format ? FORMAT_LABELS[format] : "its contents say"}{" "}
                  <span aria-hidden>›</span>
                </span>
              </DM.SubTrigger>
              <DM.Portal>
                <DM.SubContent className={menuStyles.content} loop>
                  <DM.RadioGroup
                    value={row.format.set ?? "default"}
                    onValueChange={(v) =>
                      change(row, "enter", {
                        property: "format",
                        value: v === "default" ? null : (v as BrowseFormat),
                      })
                    }
                  >
                    <MenuRadio value="default">
                      {defaultItem(
                        row.format.inherited
                          ? FORMAT_LABELS[row.format.inherited]
                          : "what its contents say",
                        row.format.origin,
                      )}
                    </MenuRadio>
                    <DM.Separator className={menuStyles.separator} />
                    {FORMATS.map((f) => (
                      <MenuRadio key={f} value={f}>
                        {capitalize(FORMAT_LABELS[f])}
                      </MenuRadio>
                    ))}
                  </DM.RadioGroup>
                </DM.SubContent>
              </DM.Portal>
            </DM.Sub>
          </>
        )}
      </CellMenu>
    );

    const viewerCell = isFile ? (
      <CellMenu
        cell={row.viewer}
        label={
          effective(row.viewer)
            ? VIEWER_LABELS[effective(row.viewer)!]
            : "By contents"
        }
        triggerProps={commonCell("viewer", 2)}
        onCloseAutoFocus={onCloseAutoFocus("viewer")}
        ariaLabel={`Viewer mode for ${row.pattern}`}
      >
        <DM.RadioGroup
          value={row.viewer.set ?? "default"}
          onValueChange={(v) =>
            change(row, "viewer", {
              property: "viewer",
              value: v === "default" ? null : (v as ViewerMode),
            })
          }
        >
          <MenuRadio value="default">
            {defaultItem(
              row.viewer.inherited
                ? VIEWER_LABELS[row.viewer.inherited]
                : "by contents",
              row.viewer.origin,
            )}
          </MenuRadio>
          <DM.Separator className={menuStyles.separator} />
          {VIEWER_MODES.map((m) => (
            <MenuRadio key={m} value={m}>
              {VIEWER_LABELS[m]}
            </MenuRadio>
          ))}
        </DM.RadioGroup>
      </CellMenu>
    ) : (
      <span className={styles.associationNone} aria-hidden>
        —
      </span>
    );

    const languageCell = isFile ? (
      <CellMenu
        cell={row.language}
        label={languageLabel(effective(row.language))}
        triggerProps={commonCell("language", 3)}
        onCloseAutoFocus={onCloseAutoFocus("language")}
        ariaLabel={`Editor syntax highlighting for ${row.pattern}`}
        scroll
      >
        <DM.RadioGroup
          value={row.language.set ?? "default"}
          onValueChange={(v) =>
            change(row, "language", {
              property: "language",
              value: v === "default" ? null : v,
            })
          }
        >
          <MenuRadio value="default">
            {defaultItem(
              languageLabel(row.language.inherited),
              row.language.origin,
            )}
          </MenuRadio>
          <DM.Separator className={menuStyles.separator} />
          {languages.map((l) => (
            <MenuRadio key={l.id} value={l.id}>
              {l.label}
            </MenuRadio>
          ))}
        </DM.RadioGroup>
      </CellMenu>
    ) : (
      <span className={styles.associationNone} aria-hidden>
        —
      </span>
    );

    return (
      <>
        <div role="gridcell">{enterCell}</div>
        <div role="gridcell">{viewerCell}</div>
        <div role="gridcell">{languageCell}</div>
      </>
    );
  };

  const renderRow = (row: AssociationRow) => (
    <div
      role="row"
      key={`${row.kind}:${row.pattern}`}
      className={styles.associationRow}
    >
      <div role="rowheader" className={styles.associationName}>
        <span className={styles.associationPattern}>{row.pattern}</span>
        {row.kind === "directory" && (
          <span className={styles.associationMeta}>folders</span>
        )}
        {row.shared_with.length > 0 && (
          <span className={styles.associationMeta}>
            with {row.shared_with.join(", ")}
          </span>
        )}
      </div>
      {renderCells(row, false)}
      <div role="gridcell" className={styles.associationReset}>
        {row.customized && (
          <button
            type="button"
            className={styles.resetButton}
            onClick={() =>
              safeSilent(
                ipc.setAssociation(row.pattern, row.kind, {
                  property: "clear",
                }),
              )
            }
            title={`Delete the association for ${row.pattern}`}
          >
            Delete
          </button>
        )}
      </div>
    </div>
  );

  // Typing the pattern of a row already listed edits that row.
  const ghostListed =
    !!ghostRow &&
    table.rows.some(
      (r) =>
        r.kind === ghostRow.kind &&
        r.pattern.toLowerCase() === ghostRow.pattern.toLowerCase(),
    );

  const addRow = (
    <div
      role="row"
      className={`${styles.associationRow} ${styles.associationAddRow}`}
    >
      <div role="rowheader" className={styles.associationName}>
        <span className={styles.associationAddMark} aria-hidden>
          +
        </span>
        <input
          type="text"
          className={styles.associationAddInput}
          value={ghostText}
          onChange={(e) => setGhostText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && ghostRow) {
              e.preventDefault();
              cellButton(
                {
                  pattern: ghostRow.pattern,
                  kind: ghostRow.kind,
                  col: "enter",
                },
                true,
              )?.focus();
            }
          }}
          placeholder="Add a pattern"
          aria-label="Add a pattern"
          data-cell={0}
          data-row={0}
        />
        {ghostText && (
          <span
            className={styles.fileActionsGroup}
            role="group"
            aria-label="The pattern matches"
          >
            {(["file", "directory"] as const).map((k) => (
              <button
                key={k}
                type="button"
                className={styles.associationKindButton}
                aria-pressed={ghostKind === k}
                onClick={() => setGhostKind(k)}
              >
                {k === "file" ? "Files" : "Folders"}
              </button>
            ))}
          </span>
        )}
      </div>
      {ghostRow ? (
        renderCells(ghostRow, true)
      ) : (
        <div role="gridcell" className={styles.associationAddHint}>
          {ghostText.trim() && "Not a pattern"}
        </div>
      )}
      <div role="gridcell" className={styles.associationAddHint}>
        {ghostListed && "listed"}
      </div>
    </div>
  );

  const preview = table.name;
  return (
    <div className={styles.settingsList}>
      {preview && (
        <div className={styles.associationPreview} aria-live="polite">
          <span className={styles.associationPattern}>{preview.name}</span>
          {"  "}
          Enter:{" "}
          {enterLabel(preview.enter, "file", preview.format).toLowerCase()} ·
          View: {preview.viewer ? VIEWER_LABELS[preview.viewer] : "by contents"}{" "}
          · Syntax: {languageLabel(preview.language)}
        </div>
      )}
      <div
        role="grid"
        aria-label="File associations"
        className={styles.associationGrid}
        ref={gridRef}
        onKeyDown={onGridKeyDown}
      >
        <div role="row" className={styles.associationHead}>
          <div role="columnheader">Name</div>
          <div role="columnheader">Enter</div>
          <div role="columnheader">View (F3)</div>
          <div role="columnheader">Syntax highlighting (F4)</div>
          <div role="columnheader" aria-hidden />
        </div>
        {addRow}
        {yours.length > 0 && (
          <>
            <div className={styles.associationSection} role="presentation">
              Yours
            </div>
            {yours.map(renderRow)}
          </>
        )}
        {builtIn.length > 0 && (
          <>
            <div className={styles.associationSection} role="presentation">
              Built-in
            </div>
            {builtIn.map(renderRow)}
          </>
        )}
        {table.rows.length === 0 && (
          <div className={styles.associationEmpty}>
            No pattern matches “{filter}”.
            {table.candidate && (
              <button
                type="button"
                onClick={() => addCandidate(table.candidate!)}
              >
                Add {table.candidate}
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function MenuRadio({
  value,
  children,
}: {
  value: string;
  children: React.ReactNode;
}) {
  return (
    <DM.RadioItem value={value} className={menuStyles.item}>
      <span className={menuStyles.checkColumn} aria-hidden>
        <DM.ItemIndicator>•</DM.ItemIndicator>
      </span>
      {children}
    </DM.RadioItem>
  );
}

/// A cell showing what applies, as a button opening the menu of choices.
/// What the row sets itself is marked; what it inherits is muted, with
/// where it comes from on hover.
function CellMenu<T>({
  cell,
  label,
  detail,
  triggerProps,
  onCloseAutoFocus,
  ariaLabel,
  scroll,
  children,
}: {
  cell: AssociationCell<T>;
  label: string;
  detail?: string | null;
  triggerProps: Record<string, string | number>;
  onCloseAutoFocus: (e: Event) => void;
  ariaLabel: string;
  scroll?: boolean;
  children: React.ReactNode;
}) {
  const set = cell.set !== null;
  return (
    <DM.Root>
      <DM.Trigger asChild>
        <button
          type="button"
          className={`${styles.associationCell} ${set ? styles.associationCellSet : ""}`}
          title={set ? "Set here" : originLabel(cell.origin)}
          aria-label={`${ariaLabel}: ${label}${set ? "" : `, ${originLabel(cell.origin)}`}`}
          {...triggerProps}
        >
          <span className={styles.associationCellLabel}>
            {label}
            {detail && (
              <span className={styles.associationCellDetail}>{detail}</span>
            )}
          </span>
          {set && (
            <span className={styles.kbModifiedDot} aria-hidden>
              •
            </span>
          )}
        </button>
      </DM.Trigger>
      <DM.Portal>
        <DM.Content
          className={`${menuStyles.content} ${scroll ? styles.associationMenuScroll : ""}`}
          align="start"
          loop
          onCloseAutoFocus={onCloseAutoFocus}
        >
          {children}
        </DM.Content>
      </DM.Portal>
    </DM.Root>
  );
}
