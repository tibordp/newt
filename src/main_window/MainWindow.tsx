import {
  useState,
  useEffect,
  useCallback,
} from "react";

import { Allotment, LayoutPriority } from "allotment";
import "allotment/dist/style.css";
import dialogStyles from "./modals/Dialog.module.scss";

import { Profiler } from "react";
import { enablePatches } from "immer";

import {
  TerminalData,
  safeCommand,
  useRemoteState,
  useTerminalData,
} from "../lib/ipc";

import * as Dialog from "@radix-ui/react-dialog";

import { ModalContent } from "./modals/ModalContent";
import { commands, executeCommand, modifiers } from "../lib/commands";
import CommandPalette from "./modals/CommandPalette";
import OperationsPanel, { OperationProgressModal } from "./OperationsPanel";
import { MainWindowState } from "./types";
import Pane from "./Pane";
import TerminalPanel from "./TerminalPanel";

enablePatches();

function App() {
  const remoteState = useRemoteState<MainWindowState>("main_window", []);
  const terminalData = useTerminalData([]);

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [focusGeneration, setFocusGeneration] = useState(0);

  const foregroundOp = remoteState?.foreground_operation_id != null
    ? remoteState.operations[remoteState.foreground_operation_id]
    : null;

  const refocusActivePane = useCallback((e?: Event) => {
    e?.preventDefault();
    setFocusGeneration(g => g + 1);
  }, []);

  const onkeydown = useCallback((e) => {
    const { ctrlOrMeta } = modifiers(e);

    if (e.key.toLowerCase() == "p" && ctrlOrMeta) {
      setPaletteOpen(true);
    } else {
      for (const cmd of commands) {
        if (cmd.shortcut?.matches(e)) {
          if (executeCommand(cmd, remoteState) !== null) {
            e.preventDefault();
            return;
          }
        }
      }
      return;
    }

    e.preventDefault();
  }, [remoteState]);

  useEffect(() => {
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, [onkeydown]);

  return (
      <TerminalData.Provider value={terminalData}>
        <Dialog.Root open={!!remoteState?.modal && remoteState.modal.type !== "select_vfs"} onOpenChange={open => { if (!open) safeCommand("close_modal"); }}>
          <Dialog.Portal>
            <Dialog.Content className={dialogStyles.dialogContent} onCloseAutoFocus={refocusActivePane}>
              <ModalContent state={remoteState?.modal} />
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
        {foregroundOp && (
          <OperationProgressModal op={foregroundOp} onCloseAutoFocus={refocusActivePane} />
        )}
        <CommandPalette
          open={paletteOpen}
          state={remoteState}
          onClose={() => setPaletteOpen(false)}
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
