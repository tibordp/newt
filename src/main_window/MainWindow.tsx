import {
  useEffect,
  useCallback,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";

import { Allotment, LayoutPriority } from "allotment";
import "allotment/dist/style.css";
import dialogStyles from "./modals/Dialog.module.scss";

import { enablePatches } from "immer";

import { invoke } from "@tauri-apps/api/core";
import { TerminalData, useRemoteState, useTerminalData } from "../lib/ipc";

import {
  normalizeKeyEvent,
  buildBindingMap,
  getCurrentContext,
  executeCommandById,
} from "../lib/commands";
import ModalRouter from "./modals/ModalRouter";
import OperationsPanel, { OperationProgressModal } from "./OperationsPanel";
import { MainWindowState } from "./types";
import Pane from "./Pane";
import TerminalPanel from "./TerminalPanel";
import { usePreferences } from "../lib/preferences";

enablePatches();

const ASKPASS_DIALOG_STYLE = {
  top: 40,
  inset: "auto" as const,
  left: 0,
  right: 0,
  marginInline: "auto",
  width: 500,
  maxWidth: "80%",
};

function respond(response: string | null) {
  invoke("askpass_respond", { response }).catch(console.error);
}

function AskpassDialog({
  prompt,
  isSecret,
}: {
  prompt: string;
  isSecret: boolean;
}) {
  const [value, setValue] = useState("");
  const isConfirm = !isSecret && prompt.includes("(yes/no/[fingerprint])");

  const handleSubmit = useCallback(
    (e: FormEvent) => {
      e.preventDefault();
      respond(value || (isConfirm ? "yes" : value));
    },
    [value, isConfirm],
  );

  return (
    <div className={dialogStyles.dialogContent} style={ASKPASS_DIALOG_STYLE}>
      <form onSubmit={handleSubmit}>
        <div className={dialogStyles.dialogContents}>
          <h2 className={dialogStyles.dialogTitle}>
            {isConfirm
              ? "Host Key Verification"
              : isSecret
                ? "Authentication"
                : "SSH"}
          </h2>
          <label style={{ whiteSpace: "pre-wrap", marginBottom: "0.5em" }}>
            {prompt}
          </label>
          <input
            type={isSecret ? "password" : "text"}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            autoFocus
            size={40}
          />
        </div>
        <div className={dialogStyles.dialogButtons}>
          <button
            type="button"
            onClick={() => respond(isConfirm ? "no" : null)}
          >
            {isConfirm ? "No" : "Cancel"}
          </button>
          <button type="submit" className="suggested">
            {isConfirm ? "Yes" : "OK"}
          </button>
        </div>
      </form>
    </div>
  );
}

function App() {
  const remoteState = useRemoteState<MainWindowState>("main_window", []);
  const terminalData = useTerminalData([]);
  const preferences = usePreferences();

  // Trigger connect for remote/elevated; no-op for local (already connected).
  const initCalled = useRef(false);
  useEffect(() => {
    if (!initCalled.current) {
      initCalled.current = true;
      invoke("init").catch(console.error);
    }
  }, []);

  const foregroundOp =
    remoteState?.foreground_operation_id != null
      ? remoteState.operations[remoteState.foreground_operation_id]
      : null;

  const modalType = remoteState?.modal?.type;
  const modalOpen = !!modalType || !!foregroundOp;

  // Build the binding lookup map from resolved preferences
  const bindingMap = useMemo(
    () => (preferences ? buildBindingMap(preferences.bindings) : new Map()),
    [preferences?.bindings],
  );

  const onkeydown = useCallback(
    (e: KeyboardEvent) => {
      if (!remoteState || !preferences) return;

      // Don't intercept shortcuts while a modal dialog is open.
      if (remoteState.modal) return;

      const normalizedKey = normalizeKeyEvent(e);
      if (!normalizedKey) return;

      const candidates = bindingMap.get(normalizedKey);
      if (!candidates) return;

      const context = getCurrentContext(remoteState);

      // Find the best matching binding: prefer context-specific over global.
      let match = null;
      for (const binding of candidates) {
        if (binding.when) {
          if (binding.when === context) {
            match = binding;
          }
        } else {
          if (!match || !match.when) {
            match = binding;
          }
        }
      }

      if (!match) return;

      if (
        executeCommandById(match.command, remoteState, preferences) !== null
      ) {
        e.preventDefault();
      }
    },
    [remoteState, preferences, bindingMap],
  );

  useEffect(() => {
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, [onkeydown]);

  return (
    <TerminalData.Provider value={terminalData}>
      <ModalRouter state={remoteState} preferences={preferences} />
      {foregroundOp && <OperationProgressModal op={foregroundOp} />}
      <div className="container">
        {remoteState &&
          remoteState.connection_status.status === "connected" && (
            <>
              <Allotment vertical separator proportionalLayout={false}>
                <Allotment.Pane minSize={200} priority={LayoutPriority.High}>
                  <Allotment>
                    {remoteState.panes.map((props, i) => (
                      <Pane
                        key={i}
                        paneHandle={i}
                        {...props}
                        modal={remoteState.modal}
                        modalOpen={modalOpen}
                        active={
                          remoteState.display_options.panes_focused &&
                          remoteState.display_options.active_pane === i
                        }
                      />
                    ))}
                  </Allotment>
                </Allotment.Pane>
                <Allotment.Pane
                  preferredSize={300}
                  minSize={100}
                  priority={LayoutPriority.Low}
                  visible={remoteState.display_options.terminal_panel_visible}
                >
                  <TerminalPanel
                    terminals={Object.values(remoteState.terminals)}
                    activeTerminal={remoteState.display_options.active_terminal}
                    panesFocused={remoteState.display_options.panes_focused}
                    modalOpen={modalOpen}
                  />
                </Allotment.Pane>
              </Allotment>
              {Object.keys(remoteState.operations).length > 0 && (
                <OperationsPanel
                  operations={remoteState.operations}
                  foregroundOperationId={foregroundOp?.id}
                />
              )}
            </>
          )}
        {remoteState && remoteState.askpass && (
          <AskpassDialog
            prompt={remoteState.askpass.prompt}
            isSecret={remoteState.askpass.is_secret}
          />
        )}
        {remoteState &&
          remoteState.connection_status.status === "connecting" && (
            <div className="connection-status">
              {remoteState.connection_status.message}
            </div>
          )}
        {remoteState && remoteState.connection_status.status === "failed" && (
          <div className="connection-status connection-error">
            {remoteState.connection_status.error}
          </div>
        )}
        {remoteState &&
          remoteState.connection_status.status === "disconnected" && (
            <div className="connection-status connection-error">
              {remoteState.connection_status.error}
            </div>
          )}
      </div>
    </TerminalData.Provider>
  );
}

export default App;
