import { invoke } from "@tauri-apps/api/core";
import { CommonDialogProps } from "./ModalContent";
import { commands } from "../../lib/bindings";
import { safeSilent } from "../../lib/ipc";
import React from "react";
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./primitives";

export default function Debug({ cancel }: CommonDialogProps) {
  const [crashed, setCrashed] = React.useState(false);

  return (
    <DialogShell>
      <DialogHeader
        title="Debug"
        summary="Debug tools (only available in debug builds)."
      />
      <DialogBody>
        <button
          type="button"
          autoFocus
          onClick={() => invoke("plugin:webview|internal_toggle_devtools")}
        >
          Toggle DevTools
        </button>
        <button type="button" onClick={() => window.location.reload()}>
          Reload Window
        </button>
        <button type="button" onClick={() => setCrashed(true)}>
          Crash (throw error)
        </button>
        <button
          type="button"
          onClick={() => {
            safeSilent(commands.cmdDebugRunTestOperation(60));
            cancel();
          }}
        >
          Run test operation (1m)
        </button>
        {crashed &&
          (() => {
            throw new Error(
              "Test error thrown from Debug dialog. This should be caught by the ErrorBoundary and displayed in a user-friendly way.",
            );
          })()}
      </DialogBody>
      <DialogFooter>
        <button type="button" onClick={cancel}>
          Close
        </button>
      </DialogFooter>
    </DialogShell>
  );
}
