import { useState } from "react";
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
  FieldGroup,
  CheckboxField,
} from "./primitives";

type UserCommandInputProps = CommonDialogProps &
  ModalDataOf<"user_command_input">;

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
    safe(
      commands.executeUserCommand(
        context?.pane_handle ?? 0,
        command_index,
        promptValues,
        overrideConfirms ?? confirmValues,
      ),
    );
  }

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    submit();
  }

  // Single confirm with no prompts: show as a simple yes/no dialog
  if (isSingleConfirm) {
    return (
      <DialogShell>
        <DialogHeader title={command_title} />
        <DialogBody>{confirms[0]}</DialogBody>
        <DialogFooter onCancel={cancel} cancelLabel="No">
          <button
            type="button"
            className="suggested"
            autoFocus
            onClick={() => submit([true])}
          >
            Yes
          </button>
        </DialogFooter>
      </DialogShell>
    );
  }

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title={command_title} />
      <DialogBody>
        {confirms.length > 0 && (
          <FieldGroup>
            {confirms.map((message, i) => (
              <CheckboxField
                key={`confirm-${i}`}
                label={message}
                checked={confirmValues[i]}
                onChange={(checked) => updateConfirm(i, checked)}
              />
            ))}
          </FieldGroup>
        )}
        {prompts.map((prompt, i) => (
          <Field
            key={`prompt-${i}`}
            label={prompt.label}
            htmlFor={`prompt-${i}`}
          >
            <input
              type="text"
              id={`prompt-${i}`}
              value={promptValues[i]}
              onChange={(e) => updatePrompt(i, e.target.value)}
              autoFocus={i === 0}
            />
          </Field>
        ))}
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        <DialogSubmitButton>Run</DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
