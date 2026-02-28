import {
  useState,
  useEffect,
  useCallback,
  useMemo,
} from "react";

import { Allotment, LayoutPriority } from "allotment";
import "allotment/dist/style.css";
import dialogStyles from "./modals/Dialog.module.scss";

import { enablePatches } from "immer";

import {
  TerminalData,
  safeCommand,
  useRemoteState,
  useTerminalData,
} from "../lib/ipc";

import * as Dialog from "@radix-ui/react-dialog";

import { ModalContent } from "./modals/ModalContent";
import {
  normalizeKeyEvent,
  buildBindingMap,
  getCurrentContext,
  executeCommandById,
} from "../lib/commands";
import CommandPalette from "./modals/CommandPalette";
import HotPaths from "./modals/HotPaths";
import OperationsPanel, { OperationProgressModal } from "./OperationsPanel";
import { MainWindowState } from "./types";
import Pane from "./Pane";
import TerminalPanel from "./TerminalPanel";
import { usePreferences } from "../lib/preferences";
import SettingsEditor from "./modals/SettingsEditor";

enablePatches();

// Modal types that use their own rendering (not the generic Dialog.Root)
const CUSTOM_MODAL_TYPES = ["select_vfs", "command_palette", "hot_paths", "settings"];

function App() {
  const remoteState = useRemoteState<MainWindowState>("main_window", []);
  const terminalData = useTerminalData([]);
  const preferences = usePreferences();

  const [focusGeneration, setFocusGeneration] = useState(0);

  const foregroundOp = remoteState?.foreground_operation_id != null
    ? remoteState.operations[remoteState.foreground_operation_id]
    : null;

  const refocusActivePane = useCallback((e?: Event) => {
    e?.preventDefault();
    setFocusGeneration(g => g + 1);
  }, []);

  const closeModal = useCallback(() => safeCommand("close_modal"), []);

  // Build the binding lookup map from resolved preferences
  const bindingMap = useMemo(
    () => preferences ? buildBindingMap(preferences.bindings) : new Map(),
    [preferences?.bindings],
  );

  const modalType = remoteState?.modal?.type;

  const onkeydown = useCallback((e: KeyboardEvent) => {
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

    if (executeCommandById(match.command, remoteState, preferences) !== null) {
      e.preventDefault();
    }
  }, [remoteState, preferences, bindingMap]);

  useEffect(() => {
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, [onkeydown]);

  return (
      <TerminalData.Provider value={terminalData}>
        <Dialog.Root
          open={!!modalType && !CUSTOM_MODAL_TYPES.includes(modalType)}
          onOpenChange={open => { if (!open) closeModal(); }}
        >
          <Dialog.Portal>
            <Dialog.Content className={dialogStyles.dialogContent} onCloseAutoFocus={refocusActivePane}>
              <ModalContent state={remoteState?.modal ?? null} />
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
        {foregroundOp && (
          <OperationProgressModal op={foregroundOp} onCloseAutoFocus={refocusActivePane} />
        )}
        <CommandPalette
          open={modalType === "command_palette"}
          preferences={preferences}
          state={remoteState}
          onClose={closeModal}
          onCloseAutoFocus={refocusActivePane}
        />
        <HotPaths
          open={modalType === "hot_paths"}
          state={remoteState}
          onClose={closeModal}
          onCloseAutoFocus={refocusActivePane}
        />
        <SettingsEditor
          open={modalType === "settings"}
          preferences={preferences}
          onClose={closeModal}
          onCloseAutoFocus={refocusActivePane}
        />
        <div className="container">
          {remoteState && (
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
                        focusGeneration={focusGeneration}
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
        </div>
      </TerminalData.Provider>
  );
}

export default App;
