import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import * as Dialog from "@radix-ui/react-dialog";

import {
  commands,
  type BatchRenameFilter,
  type BatchRenamePreview,
  type BatchRenameRow,
  type Rule,
} from "../../lib/bindings";
import { FileIcon } from "../../lib/fileIcons";
import { safe, tryRun } from "../../lib/ipc";
import { usePreferences } from "../../lib/preferences";
import { rowHeightFor } from "../density";
import type { ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  DialogError,
  useAsyncAction,
} from "./primitives";
import styles from "./BatchRename.module.scss";

const preventAutoFocus = (e: Event) => e.preventDefault();

/// Rows rendered past each edge of the list.
const OVERSCAN = 10;
/// How long typing pauses before the rule is sent.
const RULE_DELAY_MS = 100;

const isMac = navigator.platform.startsWith("Mac");
/// Monaco's toggle keys: Alt+C and Alt+P, with Cmd on macOS.
const toggleKey = (e: React.KeyboardEvent, code: string) =>
  e.code === code && e.altKey && (isMac ? e.metaKey : !e.ctrlKey);
const toggleLabel = (key: string) => (isMac ? `⌥⌘${key}` : `Alt+${key}`);

const REPLACE_HELP =
  "$1, $<name>: a group · $&: the whole match · $$: a dollar · \\u \\l \\U \\L before a group: change its case";

const count = (n: number) => n.toLocaleString();

export default function BatchRenameContent({
  data,
}: {
  data: ModalDataOf<"batch_rename">;
}) {
  return (
    <Dialog.Content
      className={styles.content}
      onCloseAutoFocus={preventAutoFocus}
    >
      <BatchRename {...data} />
    </Dialog.Content>
  );
}

function BatchRename({
  title,
  defaults,
  preview,
}: ModalDataOf<"batch_rename">) {
  const [find, setFind] = useState("");
  const [replace, setReplace] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(defaults.case_sensitive);
  const [keepCase, setKeepCase] = useState(defaults.keep_case);
  const rule: Rule = {
    find,
    replace,
    case_sensitive: caseSensitive,
    keep_case: keepCase,
  };

  useEffect(() => {
    const timer = setTimeout(
      () =>
        void commands.batchRenameSetRule({
          find,
          replace,
          case_sensitive: caseSensitive,
          keep_case: keepCase,
        }),
      RULE_DELAY_MS,
    );
    return () => clearTimeout(timer);
  }, [find, replace, caseSensitive, keepCase]);

  const { pending, error, run } = useAsyncAction(async () => {
    safe(
      commands.updateRuntimeState("batch_rename", {
        case_sensitive: caseSensitive,
        keep_case: keepCase,
      }),
    );
    return tryRun(commands.batchRenameStart(rule));
  });

  const blocked =
    preview.rule_error !== null ||
    preview.errors > 0 ||
    preview.renamed === 0 ||
    !find;

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!blocked) run();
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (toggleKey(e, "KeyC")) {
      e.preventDefault();
      setCaseSensitive((v) => !v);
    } else if (toggleKey(e, "KeyP")) {
      e.preventDefault();
      setKeepCase((v) => !v);
    }
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Batch Rename" summary={title} />
      <DialogBody className={styles.body}>
        <div className={styles.fields} onKeyDown={onKeyDown}>
          <label htmlFor="batch-rename-find">Find</label>
          <div className={styles.field}>
            <input
              id="batch-rename-find"
              type="text"
              value={find}
              onChange={(e) => setFind(e.target.value)}
              placeholder="IMG_(\d+)"
              title="A regular expression, matched anywhere in the name"
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              aria-invalid={preview.rule_error !== null}
              autoFocus
            />
            <Toggle
              label="Aa"
              title={`Case-sensitive (${toggleLabel("C")})`}
              on={caseSensitive}
              onChange={setCaseSensitive}
            />
          </div>
          <label htmlFor="batch-rename-replace">Replace</label>
          <div className={styles.field}>
            <input
              id="batch-rename-replace"
              type="text"
              value={replace}
              onChange={(e) => setReplace(e.target.value)}
              placeholder="trip-$1"
              title={REPLACE_HELP}
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
            />
            <Toggle
              label="AB"
              title={`Keep case: the replacement takes the case of what it replaces (${toggleLabel("P")})`}
              on={keepCase}
              onChange={setKeepCase}
            />
          </div>
        </div>
        <Status preview={preview} />
        <RowList preview={preview} />
        <DialogError error={error} />
      </DialogBody>
      <DialogFooter onCancel={() => safe(commands.closeModal())}>
        <DialogSubmitButton
          pending={pending}
          pendingLabel="Starting…"
          disabled={blocked}
        >
          {preview.renamed > 0 ? `Rename ${count(preview.renamed)}` : "Rename"}
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}

/// An icon button that stays pressed while its option is on, Monaco's.
function Toggle({
  label,
  title,
  on,
  onChange,
}: {
  label: string;
  title: string;
  on: boolean;
  onChange: (on: boolean) => void;
}) {
  return (
    <button
      type="button"
      className={`${styles.toggle} ${on ? styles.toggleOn : ""}`}
      aria-pressed={on}
      aria-label={title}
      title={title}
      onClick={() => onChange(!on)}
    >
      {label}
    </button>
  );
}

function Status({ preview }: { preview: BatchRenamePreview }) {
  const unchanged = preview.total - preview.renamed;
  return (
    <div className={styles.status}>
      {preview.rule_error !== null ? (
        <span className={styles.error} role="alert">
          {preview.rule_error}
        </span>
      ) : (
        <span>
          {count(preview.renamed)} renamed · {count(unchanged)} unchanged
          {preview.errors > 0 && (
            <span className={styles.error}>
              {" "}
              · {count(preview.errors)}{" "}
              {preview.errors === 1 ? "error" : "errors"}
            </span>
          )}
          {preview.warnings > 0 && (
            <span className={styles.warning}>
              {" "}
              · {count(preview.warnings)}{" "}
              {preview.warnings === 1 ? "warning" : "warnings"}
            </span>
          )}
          {preview.copies !== null && (
            <span
              className={styles.muted}
              title={`${preview.copies} has no rename of its own: each one copies and deletes, and a folder copies everything in it`}
            >
              {" "}
              · copies on {preview.copies}
            </span>
          )}
        </span>
      )}
      <select
        className={styles.filter}
        aria-label="Show"
        value={preview.filter}
        onChange={(e) =>
          void commands.batchRenameSetFilter(
            e.target.value as BatchRenameFilter,
          )
        }
      >
        <option value="all">All</option>
        <option value="changed">Renamed</option>
        <option value="problems">Problems</option>
      </select>
    </div>
  );
}

/// The rows, virtualized: the host sends the ones in view.
function RowList({ preview }: { preview: BatchRenamePreview }) {
  const preferences = usePreferences();
  const rowHeight = rowHeightFor(preferences?.settings?.appearance?.density);
  const listRef = useRef<HTMLDivElement>(null);
  const [cursor, setCursor] = useState<number | null>(null);
  const viewport = useRef<[number, number]>([-1, -1]);

  const sendViewport = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    const first = Math.max(0, Math.floor(el.scrollTop / rowHeight) - OVERSCAN);
    const visible = Math.ceil(el.clientHeight / rowHeight) + 2 * OVERSCAN;
    const [f, c] = viewport.current;
    if (f === first && c === visible) return;
    viewport.current = [first, visible];
    void commands.batchRenameSetViewport(first, visible);
  }, [rowHeight]);

  useLayoutEffect(() => {
    sendViewport();
    const el = listRef.current;
    if (!el) return;
    const observer = new ResizeObserver(sendViewport);
    observer.observe(el);
    return () => observer.disconnect();
  }, [sendViewport]);

  // A cursor past the rows shown (the filter changed) comes back in.
  const shown = preview.shown;
  const at = cursor === null ? null : Math.min(cursor, shown - 1);
  const activeCursor = at !== null && at >= 0 ? at : null;

  function moveTo(pos: number) {
    const el = listRef.current;
    if (!el || shown === 0) return;
    const clamped = Math.max(0, Math.min(shown - 1, pos));
    setCursor(clamped);
    const top = clamped * rowHeight;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + rowHeight > el.scrollTop + el.clientHeight) {
      el.scrollTop = top + rowHeight - el.clientHeight;
    }
    sendViewport();
  }

  async function nextProblem(forward: boolean) {
    const pos = await commands.batchRenameFindProblem(activeCursor, forward);
    if (pos !== null) moveTo(pos);
  }

  function onKeyDown(e: React.KeyboardEvent) {
    const page = Math.max(
      1,
      Math.floor((listRef.current?.clientHeight ?? rowHeight) / rowHeight) - 1,
    );
    const from = activeCursor ?? -1;
    const keys: Record<string, () => void> = {
      ArrowDown: () => moveTo(from + 1),
      ArrowUp: () => moveTo(Math.max(from - 1, 0)),
      PageDown: () => moveTo(from + page),
      PageUp: () => moveTo(Math.max(from - page, 0)),
      Home: () => moveTo(0),
      End: () => moveTo(shown - 1),
    };
    const action = keys[e.key];
    if (action && !e.altKey && !e.metaKey && !e.ctrlKey) {
      e.preventDefault();
      action();
    }
  }

  // F8 / Shift+F8 from anywhere in the dialog, as in Monaco's problems.
  useEffect(() => {
    function onF8(e: KeyboardEvent) {
      if (e.key !== "F8" || e.altKey || e.metaKey || e.ctrlKey) return;
      e.preventDefault();
      void nextProblem(!e.shiftKey);
    }
    document.addEventListener("keydown", onF8, true);
    return () => document.removeEventListener("keydown", onF8, true);
  });

  return (
    <div className={styles.table}>
      <div className={styles.header}>
        <span>Name</span>
        <span>New name</span>
      </div>
      <div
        ref={listRef}
        className={styles.list}
        tabIndex={0}
        role="listbox"
        aria-label="Renames (F8: next problem)"
        aria-activedescendant={
          activeCursor !== null ? `batch-rename-row-${activeCursor}` : undefined
        }
        onScroll={sendViewport}
        onKeyDown={onKeyDown}
      >
        <div style={{ height: shown * rowHeight, position: "relative" }}>
          {preview.window.map((row, i) => {
            const pos = preview.offset + i;
            return (
              <Row
                key={pos}
                id={`batch-rename-row-${pos}`}
                row={row}
                top={pos * rowHeight}
                cursor={pos === activeCursor}
                onClick={() => setCursor(pos)}
              />
            );
          })}
        </div>
        {shown === 0 && (
          <div className={styles.empty}>
            {preview.filter === "problems" ? "No problems" : "Nothing to show"}
          </div>
        )}
      </div>
    </div>
  );
}

function Row({
  id,
  row,
  top,
  cursor,
  onClick,
}: {
  id: string;
  row: BatchRenameRow;
  top: number;
  cursor: boolean;
  onClick: () => void;
}) {
  const problem = row.problem;
  const icon = <FileIcon name={row.name} folder={row.is_dir} />;
  const classes = [
    styles.row,
    cursor ? styles.cursor : "",
    row.new_name === null ? styles.unchanged : "",
    problem ? (problem.error ? styles.rowError : styles.rowWarning) : "",
  ];
  return (
    <div
      id={id}
      role="option"
      aria-selected={cursor}
      className={classes.join(" ")}
      style={{ top }}
      title={problem?.message}
      onMouseDown={onClick}
    >
      <div className={styles.cell}>
        {icon}
        <span className={styles.text}>
          <Marked text={row.name} ranges={row.matched} />
          {row.folder !== null && (
            <span className={styles.folder}> ({row.folder})</span>
          )}
        </span>
      </div>
      <div className={styles.cell}>
        {row.new_name !== null && (
          <span className={styles.text}>
            <Marked text={row.new_name} ranges={row.replaced} />
          </span>
        )}
        {problem && (
          <span className={styles.problem}>
            {problem.error ? "✕" : "⚠"} {problem.message}
          </span>
        )}
      </div>
    </div>
  );
}

/// `text` with the UTF-16 `ranges` marked.
function Marked({
  text,
  ranges,
}: {
  text: string;
  ranges: [number, number][];
}) {
  const parts: React.ReactNode[] = [];
  let last = 0;
  ranges.forEach(([start, end], i) => {
    if (start > last) parts.push(text.slice(last, start));
    parts.push(
      <mark key={i} className={styles.mark}>
        {text.slice(start, end)}
      </mark>,
    );
    last = end;
  });
  if (last < text.length) parts.push(text.slice(last));
  return <>{parts}</>;
}
