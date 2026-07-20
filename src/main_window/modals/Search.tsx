import { useEffect, useRef, useState } from "react";
import { commands } from "../../lib/bindings";
import { safe, tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
  DialogSubmitButton,
  DialogError,
  Field,
  FieldGroup,
  CheckboxField,
  useAsyncAction,
} from "./primitives";

type SearchProps = CommonDialogProps & ModalDataOf<"search">;

export default function SearchDialog({
  path,
  display_path,
  prefill,
  defaults,
  cancel,
  context,
}: SearchProps) {
  const [namePattern, setNamePattern] = useState(prefill?.name_pattern ?? "");
  const [contentPattern, setContentPattern] = useState(
    prefill?.content_pattern ?? "",
  );
  // Refine (prefill) restores the live search's options; a fresh search seeds
  // from the sticky last-used defaults.
  const [contentIsRegex, setContentIsRegex] = useState(
    prefill?.content_is_regex ?? defaults.content_is_regex,
  );
  const [caseSensitive, setCaseSensitive] = useState(
    prefill?.case_sensitive ?? defaults.case_sensitive,
  );
  const [followSymlinks, setFollowSymlinks] = useState(
    prefill?.follow_symlinks ?? defaults.follow_symlinks,
  );
  const inputRef = useRef<HTMLInputElement>(null);

  const { pending, error, run } = useAsyncAction(async () => {
    safe(
      commands.updateRuntimeState("search", {
        case_sensitive: caseSensitive,
        content_is_regex: contentIsRegex,
        follow_symlinks: followSymlinks,
      }),
    );
    return tryRun(
      commands.mountSearch(
        context?.pane_handle ?? 0,
        path,
        namePattern || null,
        contentPattern || null,
        contentIsRegex,
        caseSensitive,
        followSymlinks,
      ),
    );
  });

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
  }

  useEffect(() => {
    // Select-all on a prefilled pattern (refine flow) so typing replaces
    // it outright while arrow keys still allow tweaking in place.
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const canSubmit = namePattern.length > 0 || contentPattern.length > 0;

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title={`Search in ${display_path}`} />
      <DialogBody>
        <Field
          label="Name (substring; use *, ?, [ for glob — e.g. *.rs)"
          htmlFor="name-pattern"
        >
          <input
            ref={inputRef}
            id="name-pattern"
            type="text"
            value={namePattern}
            onChange={(e) => setNamePattern(e.target.value)}
            size={40}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            disabled={pending}
          />
        </Field>
        <Field
          label="Content (optional; substring or regex)"
          htmlFor="content-pattern"
        >
          <input
            id="content-pattern"
            type="text"
            value={contentPattern}
            onChange={(e) => setContentPattern(e.target.value)}
            size={40}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            disabled={pending}
          />
        </Field>
        <FieldGroup>
          <CheckboxField
            label="Content is a regular expression"
            checked={contentIsRegex}
            onChange={setContentIsRegex}
            disabled={pending}
          />
          <CheckboxField
            label="Case-sensitive"
            checked={caseSensitive}
            onChange={setCaseSensitive}
            disabled={pending}
          />
          <CheckboxField
            label="Follow symlinks"
            checked={followSymlinks}
            onChange={setFollowSymlinks}
            disabled={pending}
          />
        </FieldGroup>
        <DialogError error={error} />
      </DialogBody>
      <DialogFooter onCancel={cancel} cancelDisabled={pending}>
        <DialogSubmitButton
          pending={pending}
          pendingLabel="Starting…"
          disabled={!canSubmit}
        >
          Search
        </DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
