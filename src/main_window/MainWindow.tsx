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
import styles from "./MainWindow.module.scss";
import QuickView from "./QuickView";

/// How long the focused row must rest before Quick View loads it.
const PREVIEW_DELAY_MS = 80;
import {
  DialogShell,
  DialogHeader,
  DialogBody,
  DialogFooter,
} from "./modals/primitives";

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
import {
  KEYBOARD_MENU_EVENT,
  MainWindowState,
  OperationState,
  isKeyboardContextMenu,
} from "./types";
import Pane from "./Pane";
import TerminalPanel from "./TerminalPanel";
import { usePreferences } from "../lib/preferences";
import { useRuntimeState } from "../lib/runtimeState";
import CommandBar from "./CommandBar";

enablePatches();

function sendAskpassResponse(response: string | null) {
  safeSilent(commands.askpassRespond(response));
}

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
  const inputRef = useRef<HTMLInputElement>(null);

  // Focus from Radix's open-autofocus hook rather than the `autoFocus`
  // attribute: the prompt can appear above another open dialog (a connect
  // dialog mid-handshake), whose focus trap reclaims any focus moved
  // before this dialog's scope is registered. On close, Radix's default
  // returns focus to wherever it was — that dialog, or the pane.
  const focusInput = useCallback((e: Event) => {
    e.preventDefault();
    inputRef.current?.focus();
  }, []);

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
          className={dialogStyles.dialogContentTop}
          onOpenAutoFocus={focusInput}
          onPointerDownOutside={preventAskpassInteractOutside}
          onInteractOutside={preventAskpassInteractOutside}
        >
          <DialogShell onSubmit={handleSubmit}>
            <DialogHeader
              title={
                isConfirm
                  ? "Host Key Verification"
                  : isSecret
                    ? "Authentication"
                    : "SSH"
              }
              summary={prompt}
            />
            <DialogBody>
              <input
                type={isSecret ? "password" : "text"}
                value={value}
                onChange={(e) => setValue(e.target.value)}
                ref={inputRef}
                size={40}
                aria-label={prompt}
              />
            </DialogBody>
            <DialogFooter
              onCancel={cancel}
              cancelLabel={isConfirm ? "No" : "Cancel"}
            >
              <button type="submit" className="suggested">
                {isConfirm ? "Yes" : "OK"}
              </button>
            </DialogFooter>
          </DialogShell>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function App() {
  const remoteState = useRemoteState<MainWindowState>("main_window", []);
  const terminalData = useTerminalData([]);
  const preferences = usePreferences();
  const runtimeState = useRuntimeState();
  // Persisted terminal panel height; the file-pane split stays 50/50.
  const terminalHeight = runtimeState?.layout?.terminal_height ?? 300;

  // Trigger connect for remote/elevated; no-op for local (already connected).
  const initCalled = useRef(false);
  useEffect(() => {
    if (!initCalled.current) {
      initCalled.current = true;
      safeSilent(commands.init());
    }
  }, []);

  const density = preferences?.settings.appearance?.density;
  useEffect(() => {
    document.documentElement.dataset.density = density ?? "comfortable";
  }, [density]);

  // While another window has the keyboard, focus marks (the cursor, the
  // active pane's dot, the terminal tab's line) turn grey.
  useEffect(() => {
    const root = document.documentElement;
    const update = () => {
      if (document.hasFocus()) delete root.dataset.windowInactive;
      else root.dataset.windowInactive = "";
    };
    update();
    window.addEventListener("focus", update);
    window.addEventListener("blur", update);
    return () => {
      window.removeEventListener("focus", update);
      window.removeEventListener("blur", update);
    };
  }, []);

  const foregroundOp =
    remoteState?.foreground_operation_id != null
      ? remoteState.operations[remoteState.foreground_operation_id]
      : null;

  // Silent operations produce no panel row. The backend clears the flag
  // when one fails, so failed ones appear here like any other.
  const visibleOperations = useMemo(() => {
    const entries = Object.entries(remoteState?.operations ?? {}).filter(
      (e): e is [string, OperationState] =>
        !!e[1] && (!e[1].silent || e[1].status === "failed"),
    );
    return entries.length > 0 ? Object.fromEntries(entries) : null;
  }, [remoteState?.operations]);

  const modalType = remoteState?.modal?.type;
  const modalOpen = !!modalType || !!foregroundOp || !!remoteState?.askpass;

  // Maximized shows only what has focus: the terminal panel when it holds
  // focus, otherwise the active pane. The split underneath is left as it
  // is — the shown view is drawn over it — so restoring it is exact.
  const display = remoteState?.display_options;
  const terminalMaximized =
    !!display?.maximized &&
    !display.panes_focused &&
    display.terminal_panel_visible;
  const panesMaximized = !!display?.maximized && !terminalMaximized;
  const quickView = !!display?.quick_view;

  // The title leads with where the active pane is, ahead of the session's
  // own title ("Newt", "Newt [host]").
  const baseTitle = remoteState?.window_title;
  const here =
    remoteState?.panes[display?.active_pane ?? 0]?.breadcrumbs.at(-1)?.label;
  useEffect(() => {
    if (baseTitle === undefined) return;
    safeSilent(
      commands.setWindowTitle(here ? `${here} - ${baseTitle}` : baseTitle),
    );
  }, [here, baseTitle]);

  // Quick View follows the active pane's focused row, once it stops moving.
  const previewPane = quickView
    ? remoteState?.panes[display.active_pane]
    : null;
  const previewKey = previewPane
    ? JSON.stringify([
        display?.active_pane,
        previewPane.path,
        previewPane.focused,
      ])
    : null;
  useEffect(() => {
    if (previewKey === null) return;
    const timer = setTimeout(
      () => safeSilent(commands.previewFocused()),
      PREVIEW_DELAY_MS,
    );
    return () => clearTimeout(timer);
  }, [previewKey]);

  const renderPane = (i: number) => {
    if (!remoteState) return null;
    const props = remoteState.panes[i];
    return (
      <Pane
        paneHandle={i}
        {...props}
        modal={remoteState.modal}
        modalOpen={modalOpen}
        vfsProgress={
          remoteState.vfs_progress?.[
            // While a navigation streams, the pane is
            // still *on* the old path — the VFS doing
            // the work is the one it is heading to.
            String((props.pending_path ?? props.path).vfs_id)
          ]
        }
        active={
          remoteState.display_options.panes_focused &&
          remoteState.display_options.active_pane === i
        }
        windowsDrives={remoteState.mount_summary?.has_split_root_vfs ?? false}
        maximized={panesMaximized}
        swappable={quickView || panesMaximized}
      />
    );
  };

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

  // Native menu items (macOS) arrive as window-scoped `menu-command` events
  // carrying a command id, dispatched through the same path as keybindings.
  // Subscribed once; the ref keeps the handler on fresh state.
  const menuDispatchRef = useRef({ remoteState, preferences });
  menuDispatchRef.current = { remoteState, preferences };
  useEffect(() => {
    const appWindow = getCurrentWebviewWindow();
    const unlisten = appWindow.listen<string>("menu-command", (event) => {
      const { remoteState, preferences } = menuDispatchRef.current;
      if (remoteState && preferences) {
        executeCommandById(event.payload, remoteState, preferences);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

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
      if (isKeyboardContextMenu(e)) {
        window.dispatchEvent(new Event(KEYBOARD_MENU_EVENT));
      }
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
                    !target.closest("[class*='xterm']") &&
                    !target.closest("[data-quick-view]")
                  ) {
                    e.preventDefault();
                  }
                }}
              >
                <Allotment
                  vertical
                  separator
                  proportionalLayout={false}
                  className={
                    display?.maximized ? styles.maximizedSplit : undefined
                  }
                  onDragEnd={(sizes) => {
                    // [fileArea, terminal]; persist the terminal pane height.
                    const h = sizes[1];
                    if (h != null && h > 0) {
                      safeSilent(
                        commands.updateRuntimeState(
                          "layout.terminal_height",
                          h,
                        ),
                      );
                    }
                  }}
                >
                  <Allotment.Pane
                    minSize={200}
                    priority={LayoutPriority.High}
                    className={
                      panesMaximized
                        ? styles.maximizedView
                        : terminalMaximized
                          ? styles.coveredView
                          : undefined
                    }
                  >
                    <Allotment
                      className={
                        panesMaximized ? styles.maximizedSplit : undefined
                      }
                    >
                      {[0, 1].map((slot) => (
                        <Allotment.Pane
                          key={slot}
                          className={
                            !panesMaximized
                              ? undefined
                              : slot ===
                                  (quickView
                                    ? 0
                                    : remoteState.display_options.active_pane)
                                ? styles.maximizedView
                                : styles.coveredView
                          }
                        >
                          {quickView && slot === 1 ? (
                            <QuickView
                              state={remoteState.preview}
                              activePane={
                                remoteState.display_options.active_pane
                              }
                              panesFocused={
                                remoteState.display_options.panes_focused
                              }
                            />
                          ) : (
                            // In Quick View the left slot holds both panes
                            // and shows the active one.
                            (quickView ? [0, 1] : [slot]).map((i) => (
                              <div
                                key={i}
                                className={
                                  quickView &&
                                  i !== remoteState.display_options.active_pane
                                    ? styles.hiddenPaneSlot
                                    : styles.paneSlot
                                }
                              >
                                {renderPane(i)}
                              </div>
                            ))
                          )}
                        </Allotment.Pane>
                      ))}
                    </Allotment>
                  </Allotment.Pane>
                  <Allotment.Pane
                    preferredSize={terminalHeight}
                    minSize={100}
                    priority={LayoutPriority.Low}
                    visible={remoteState.display_options.terminal_panel_visible}
                    className={
                      terminalMaximized
                        ? styles.maximizedView
                        : panesMaximized
                          ? styles.coveredView
                          : undefined
                    }
                  >
                    <TerminalPanel
                      terminals={Object.values(remoteState.terminals).filter(
                        (t) => t !== undefined,
                      )}
                      activeTerminal={
                        remoteState.display_options.active_terminal
                      }
                      panesFocused={remoteState.display_options.panes_focused}
                      modalOpen={modalOpen}
                      maximized={terminalMaximized}
                    />
                  </Allotment.Pane>
                </Allotment>
              </div>
              {visibleOperations && (
                <OperationsPanel
                  operations={visibleOperations}
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
            key={remoteState.askpass.id}
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
            <div className="connection-status connection-error" role="alert">
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
