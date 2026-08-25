import { useEffect, useRef, useState } from "react";
import { commands } from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  Field,
  FieldRow,
} from "./primitives";

type SelectByPatternProps = CommonDialogProps &
  ModalDataOf<"select_by_pattern">;

export default function SelectByPattern({
  pattern: initialPattern,
  subtract: initialSubtract,
  cancel,
  context,
}: SelectByPatternProps) {
  const paneHandle = context?.pane_handle;
  const [pattern, setPattern] = useState(initialPattern);
  const [subtract, setSubtract] = useState(initialSubtract);
  // null: pattern doesn't compile; undefined: no count yet
  const [matches, setMatches] = useState<number | null | undefined>();
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.select();
  }, []);

  useEffect(() => {
    if (paneHandle == null) return;
    let stale = false;
    commands.countPatternMatches(paneHandle, pattern).then((r) => {
      if (!stale && r.status === "ok") setMatches(r.data);
    });
    return () => {
      stale = true;
    };
  }, [paneHandle, pattern]);

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (paneHandle == null || matches == null) return;
    safe(commands.selectByPattern(paneHandle, pattern, subtract));
  }

  const hint =
    matches === null
      ? "Invalid pattern"
      : matches === undefined
        ? " "
        : `${matches} ${matches === 1 ? "entry matches" : "entries match"}`;

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader
        title={subtract ? "Deselect by Pattern" : "Select by Pattern"}
      />
      <DialogBody>
        <Field label="Pattern" htmlFor="pattern" hint={hint}>
          <input
            type="text"
            id="pattern"
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
            placeholder="*.txt — or /regex"
            size={40}
            ref={inputRef}
            autoFocus
            spellCheck={false}
            aria-invalid={matches === null}
          />
        </Field>
        <FieldRow label="Action">
          <select
            value={subtract ? "deselect" : "select"}
            onChange={(e) => setSubtract(e.target.value === "deselect")}
          >
            <option value="select">Add to selection</option>
            <option value="deselect">Remove from selection</option>
          </select>
        </FieldRow>
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        <DialogSubmitButton disabled={!pattern || matches == null}>
          {subtract ? "Deselect" : "Select"}
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
