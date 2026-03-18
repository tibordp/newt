import * as Dialog from "@radix-ui/react-dialog";
import { invoke } from "@tauri-apps/api/core";
import { CommonDialogProps } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";
import React from "react";

export default function Debug({ cancel }: CommonDialogProps) {
  const [crashed, setCrashed] = React.useState(false);

  return (
    <div>
      <div className={dialogStyles.dialogContents}>
        <Dialog.Title className={dialogStyles.dialogTitle}>Debug</Dialog.Title>
        <p className={dialogStyles.dialogSummary}>
          Debug tools (only available in debug builds).
        </p>
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: "var(--space-4)",
          }}
        >
          <button
            type="button"
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
        </div>
        {crashed &&
          (() => {
            throw new Error(
              "Test error thrown from Debug dialog. This should be caught by the ErrorBoundary and displayed in a user-friendly way.",
            );
          })()}
      </div>
      <div className={dialogStyles.dialogButtons}>
        <button type="button" onClick={cancel} autoFocus>
          Close
        </button>
      </div>
    </div>
  );
}
