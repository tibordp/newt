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
} from "./primitives";

type NavigateProps = CommonDialogProps & ModalDataOf<"navigate">;

export default function Navigate({
  display_path,
  cancel,
  context,
}: NavigateProps) {
  const [newPath, setNewPath] = useState(display_path);
  const inputRef = useRef<HTMLInputElement>(null);

  function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    safe(commands.navigate(context?.pane_handle ?? 0, newPath, false));
  }

  useEffect(() => {
    inputRef.current?.select();
  }, []);

  return (
    <DialogShell onSubmit={onSubmit}>
      <DialogHeader title="Navigate to" />
      <DialogBody>
        <Field label="Path" htmlFor="path">
          <input
            ref={inputRef}
            type="text"
            id="path"
            value={newPath}
            onChange={(e) => setNewPath(e.target.value)}
            size={40}
            autoFocus
          />
        </Field>
      </DialogBody>
      <DialogFooter onCancel={cancel}>
        <DialogSubmitButton disabled={!newPath}>Navigate</DialogSubmitButton>
      </DialogFooter>
    </DialogShell>
  );
}
