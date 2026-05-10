import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { commands } from "../../lib/bindings";
import { tryRun } from "../../lib/ipc";
import { CommonDialogProps, ModalDataOf } from "./ModalContent";
import { useAsyncAction } from "./useAsyncAction";
import { DialogError, DialogSubmitButton } from "./DialogActions";
import dialogStyles from "./Dialog.module.scss";

type SearchProps = CommonDialogProps & ModalDataOf<"search">;

export default function SearchDialog({
  path,
  display_path,
  cancel,
  context,
}: SearchProps) {
  const [namePattern, setNamePattern] = useState("");
  const [contentPattern, setContentPattern] = useState("");
  const [contentIsRegex, setContentIsRegex] = useState(false);
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [followSymlinks, setFollowSymlinks] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const { pending, error, run } = useAsyncAction(async () =>
    tryRun(
      commands.mountSearch(
        context?.pane_handle ?? 0,
        path,
        namePattern || null,
        contentPattern || null,
        contentIsRegex,
        caseSensitive,
        followSymlinks,
      ),
    ),
  );

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    run();
  }

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const canSubmit = namePattern.length > 0 || contentPattern.length > 0;

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          Search in {display_path}
        </Dialog.Title>
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-4)",
          }}
        >
          <div>
            <label htmlFor="name-pattern">Name (glob, e.g. *.rs)</label>
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
          </div>
          <div>
            <label htmlFor="content-pattern">
              Content (optional; substring or regex)
            </label>
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
          </div>
          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: "var(--space-2)",
              fontSize: "0.9em",
            }}
          >
            <input
              type="checkbox"
              checked={contentIsRegex}
              onChange={(e) => setContentIsRegex(e.target.checked)}
              disabled={pending}
            />
            Content is a regular expression
          </label>
          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: "var(--space-2)",
              fontSize: "0.9em",
            }}
          >
            <input
              type="checkbox"
              checked={caseSensitive}
              onChange={(e) => setCaseSensitive(e.target.checked)}
              disabled={pending}
            />
            Case-sensitive
          </label>
          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: "var(--space-2)",
              fontSize: "0.9em",
            }}
          >
            <input
              type="checkbox"
              checked={followSymlinks}
              onChange={(e) => setFollowSymlinks(e.target.checked)}
              disabled={pending}
            />
            Follow symlinks
          </label>
          <DialogError error={error} />
        </div>
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel} disabled={pending}>
          Cancel
        </button>
        <DialogSubmitButton
          pending={pending}
          pendingLabel="Starting…"
          disabled={!canSubmit}
        >
          Search
        </DialogSubmitButton>
      </div>
    </form>
  );
}
