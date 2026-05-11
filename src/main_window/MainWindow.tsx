import {
  useEffect,
  useCallback,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";

import * as Dialog from "@radix-ui/react-dialog";
import { Allotment, LayoutPriority } from "allotment";
import "allotment/dist/style.css";
import ConnectionLog from "./ConnectionLog";
import dialogStyles from "./modals/Dialog.module.scss";

import { enablePatches } from "immer";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

import { commands } from "../lib/bindings";
import {
  TerminalData,
  safeSilent,
  useRemoteState,
  useTerminalData,
} from "../lib/ipc";
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
import CommandBar from "./CommandBar";

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

function sendAskpassResponse(response: string | null) {
  safeSilent(commands.askpassRespond(response));
}

const preventAskpassAutoFocus = (e: Event) => {
  // Let our autoFocus on the input win over Radix focusing Dialog.Content.
  e.preventDefault();
};
const preventAskpassInteractOutside = (e: Event) => e.preventDefault();

function AskpassDialog({
  prompt,
  isSecret,
}: {
  prompt: string;
  isSecret: boolean;
}) {
  const [value, setValue] = useState("");
  const isConfirm = !isSecret && prompt.includes("(yes/no/[fingerprint])");
  // Guard against double-respond: ESC fires onOpenChange(false) which routes
  // through cancel(); the buttons call respond() directly. Both paths cause
  // the askpass state to clear, so we must only send one response per prompt.
  const respondedRef = useRef(false);

  const respond = useCallback((response: string | null) => {
    if (respondedRef.current) return;
    respondedRef.current = true;
    sendAskpassResponse(response);
  }, []);

  const handleSubmit = useCallback(
    (e: FormEvent) => {
      e.preventDefault();
      respond(value || (isConfirm ? "yes" : value));
    },
    [value, isConfirm, respond],
  );

  const cancel = useCallback(() => {
    respond(isConfirm ? "no" : null);
  }, [isConfirm, respond]);

  return (
    <Dialog.Root
      open
      onOpenChange={(open) => {
        // Fires for ESC and (defensively) outside-click — never from our own
        // controlled `open` prop. Treat as cancellation; the prompt stays
        // visible until the backend clears the askpass state and unmounts us.
        if (!open) cancel();
      }}
    >
      <Dialog.Portal>
        <Dialog.Content
          className={dialogStyles.dialogContent}
          style={ASKPASS_DIALOG_STYLE}
          onOpenAutoFocus={preventAskpassAutoFocus}
          onPointerDownOutside={preventAskpassInteractOutside}
          onInteractOutside={preventAskpassInteractOutside}
        >
          <form onSubmit={handleSubmit}>
            <div className={dialogStyles.dialogContents}>
              <Dialog.Title className={dialogStyles.dialogTitle}>
                {isConfirm
                  ? "Host Key Verification"
                  : isSecret
                    ? "Authentication"
                    : "SSH"}
              </Dialog.Title>
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
              <button type="button" onClick={cancel}>
                {isConfirm ? "No" : "Cancel"}
              </button>
              <button type="submit" className="suggested">
                {isConfirm ? "Yes" : "OK"}
              </button>
            </div>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
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
      safeSilent(commands.init());
    }
  }, []);

  const foregroundOp =
    remoteState?.foreground_operation_id != null
      ? remoteState.operations[remoteState.foreground_operation_id]
      : null;

  const modalType = remoteState?.modal?.type;
  const modalOpen = !!modalType || !!foregroundOp || !!remoteState?.askpass;

  // Build the binding lookup map from resolved preferences
  const bindingMap = useMemo(
    () => (preferences ? buildBindingMap(preferences.bindings) : new Map()),
    [preferences?.bindings],
  );

  const onkeydown = useCallback(
    (e: KeyboardEvent) => {
      if (!remoteState || !preferences) return;

      // Don't intercept shortcuts while a modal dialog is open.
      if (remoteState.modal || remoteState.askpass) return;

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

  // Suppress the default browser context menu except on text inputs,
  // so only our custom Radix context menus are used.
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        target.isContentEditable
      ) {
        return;
      }
      e.preventDefault();
    };
    document.addEventListener("contextmenu", handler);
    return () => document.removeEventListener("contextmenu", handler);
  }, []);

  // Prevent the browser from navigating when files are dropped.
  useEffect(() => {
    const prevent = (e: DragEvent) => e.preventDefault();
    document.addEventListener("drop", prevent);
    document.addEventListener("dragover", prevent);
    return () => {
      document.removeEventListener("drop", prevent);
      document.removeEventListener("dragover", prevent);
    };
  }, []);

  // Route Tauri external drag-drop events to the pane under the cursor.
  // Dispatches CustomEvents on the pane's [data-pane-handle] element so
  // each pane can handle highlighting and drop logic locally.
  useEffect(() => {
    const appWindow = getCurrentWebviewWindow();
    let lastPaneEl: HTMLElement | null = null;

    const unlisten = appWindow.listen<{
      kind: string;
      paths?: string[];
      x?: number;
      y?: number;
    }>("external-drag", (event) => {
      const { kind, paths, x, y } = event.payload;

      if (kind === "leave") {
        if (lastPaneEl) {
          lastPaneEl.dispatchEvent(
            new CustomEvent("external-drag-leave", { bubbles: false }),
          );
          lastPaneEl = null;
        }
        return;
      }

      const el = document.elementFromPoint(x ?? 0, y ?? 0);
      const paneEl = el?.closest("[data-pane-handle]") as HTMLElement | null;

      // Pane changed — dispatch leave on old, enter on new
      if (paneEl !== lastPaneEl) {
        if (lastPaneEl) {
          lastPaneEl.dispatchEvent(
            new CustomEvent("external-drag-leave", { bubbles: false }),
          );
        }
        lastPaneEl = paneEl;
      }

      if (!paneEl) return;

      if (kind === "enter" || kind === "over") {
        paneEl.dispatchEvent(
          new CustomEvent("external-drag-over", {
            bubbles: false,
            detail: { x, y },
          }),
        );
      } else if (kind === "drop") {
        paneEl.dispatchEvent(
          new CustomEvent("external-drop", {
            bubbles: false,
            detail: { paths, x, y },
          }),
        );
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return (
    <TerminalData.Provider value={terminalData}>
      <ModalRouter state={remoteState} preferences={preferences} />
      {foregroundOp && <OperationProgressModal op={foregroundOp} />}
      <div className="container">
        {remoteState &&
          remoteState.connection_status.status === "connected" && (
            <>
              <div
                style={{ flex: 1, overflow: "hidden" }}
                onMouseDown={(e) => {
                  // Prevent focus theft from non-interactive chrome (dividers,
                  // headers, statusbars, etc.) so the file list, terminal, or
                  // filter input keeps focus.
                  const target = e.target as HTMLElement;
                  if (
                    !target.closest("ul") &&
                    !target.closest("input") &&
                    !target.closest("textarea") &&
                    !target.closest("button") &&
                    !target.closest("[class*='xterm']")
                  ) {
                    e.preventDefault();
                  }
                }}
              >
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
                          vfsProgress={
                            remoteState.vfs_progress?.[
                              String(props.path.vfs_id)
                            ]
                          }
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
                      activeTerminal={
                        remoteState.display_options.active_terminal
                      }
                      panesFocused={remoteState.display_options.panes_focused}
                      modalOpen={modalOpen}
                    />
                  </Allotment.Pane>
                </Allotment>
              </div>
              {Object.keys(remoteState.operations).length > 0 && (
                <OperationsPanel
                  operations={remoteState.operations}
                  foregroundOperationId={foregroundOp?.id}
                />
              )}
              {preferences?.settings.appearance?.show_command_bar && (
                <CommandBar state={remoteState} preferences={preferences} />
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
          remoteState.connection_status.status !== "connected" &&
          remoteState.connection_status.log.length > 0 && (
            <ConnectionLog log={remoteState.connection_status.log} />
          )}
        {remoteState &&
          remoteState.connection_status.status === "connecting" && (
            <div className="connection-status">
              {remoteState.connection_status.message}
            </div>
          )}
        {remoteState &&
          (remoteState.connection_status.status === "failed" ||
            remoteState.connection_status.status === "disconnected") && (
            <div className="connection-status connection-error">
              {remoteState.connection_status.error}{" "}
              <button
                className="connection-retry"
                onClick={() => safeSilent(commands.reconnect())}
              >
                Reconnect
              </button>
            </div>
          )}
      </div>
    </TerminalData.Provider>
  );
}

export default App;
