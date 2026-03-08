import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { invoke } from "@tauri-apps/api/core";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";

type Prompt = {
  label: string;
  default: string;
};

type UserCommandInputProps = CommonDialogProps & {
  command_index: number;
  command_title: string;
  prompts: Prompt[];
  confirms: string[];
};

export default function UserCommandInput({
  command_index,
  command_title,
  prompts,
  confirms,
  cancel,
  context,
}: UserCommandInputProps) {
  const isSingleConfirm = confirms.length === 1 && prompts.length === 0;

  const [promptValues, setPromptValues] = useState<string[]>(
    prompts.map((p) => p.default),
  );
  const [confirmValues, setConfirmValues] = useState<boolean[]>(
    confirms.map(() => true),
  );

  function updatePrompt(index: number, value: string) {
    setPromptValues((prev) => {
      const next = [...prev];
      next[index] = value;
      return next;
    });
  }

  function updateConfirm(index: number, value: boolean) {
    setConfirmValues((prev) => {
      const next = [...prev];
      next[index] = value;
      return next;
    });
  }

  function submit(overrideConfirms?: boolean[]) {
    invoke("execute_user_command", {
      paneHandle: context?.pane_handle,
      index: command_index,
      promptResponses: promptValues,
      confirmResponses: overrideConfirms ?? confirmValues,
    });
  }

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    submit();
  }

  // Single confirm with no prompts: show as a simple yes/no dialog
  if (isSingleConfirm) {
    return (
      <>
        <div className={dialogStyles.dialogContents}>
          <Dialog.Title className={dialogStyles.dialogTitle}>
            {command_title}
          </Dialog.Title>
          <p className={dialogStyles.dialogSummary}>{confirms[0]}</p>
        </div>
        <div className={dialogStyles.dialogButtons}>
          <button type="button" onClick={cancel}>
            No
          </button>
          <button
            type="button"
            className="suggested"
            autoFocus
            onClick={() => submit([true])}
          >
            Yes
          </button>
        </div>
      </>
    );
  }

  return (
    <form onSubmit={onSubmit}>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>
          {command_title}
        </Dialog.Title>
        {confirms.map((message, i) => (
          <label
            key={`confirm-${i}`}
            style={{ display: "flex", alignItems: "center", gap: 8 }}
          >
            <input
              type="checkbox"
              checked={confirmValues[i]}
              onChange={(e) => updateConfirm(i, e.target.checked)}
            />
            {message}
          </label>
        ))}
        {prompts.map((prompt, i) => (
          <label key={`prompt-${i}`}>
            {prompt.label}
            <input
              type="text"
              value={promptValues[i]}
              onChange={(e) => updatePrompt(i, e.target.value)}
              autoFocus={i === 0}
            />
          </label>
        ))}
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel}>
          Cancel
        </button>
        <button type="submit" className="suggested">
          Run
        </button>
      </div>
    </form>
  );
}
