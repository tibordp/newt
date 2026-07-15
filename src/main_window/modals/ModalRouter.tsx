import { useCallback } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safe } from "../../lib/ipc";
import { MainWindowState } from "../types";
import { PreferencesState } from "../../lib/preferences";
import CommandPaletteContent from "./CommandPalette";
import ConnectionLogContent from "./ConnectionLogDialog";
import HotPathsContent from "./HotPaths";
import QuickConnectContent from "./QuickConnect";
import SelectWslDistroContent from "./SelectWslDistro";
import SettingsEditorContent from "./SettingsEditor";
import { ModalContent } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";
import { commands } from "../../lib/bindings";

const preventAutoFocus = (e: Event) => e.preventDefault();

// Anchored per-pane (rendered inline by Pane via Radix DropdownMenu, with
// their own dismissal behavior), so not routed here.
const EXCLUDED_MODAL_TYPES = ["select_vfs", "history_navigator"];

export default function ModalRouter({
  state,
  preferences,
}: {
  state: MainWindowState | null;
  preferences: PreferencesState | null;
}) {
  const modalType = state?.modal?.type;
  const isOpen = !!modalType && !EXCLUDED_MODAL_TYPES.includes(modalType);

  const closeModal = useCallback(() => safe(commands.closeModal()), []);

  function renderContent() {
    // Gating the sole import reference behind the build-time `__WINDOWS__`
    // literal lets Rollup DCE drop this branch and SelectWslDistro.tsx from
    // non-Windows bundles.
    if (__WINDOWS__ && modalType === "select_wsl_distro") {
      return (
        <SelectWslDistroContent distros={state?.modal?.data?.distros ?? []} />
      );
    }
    switch (modalType) {
      case "command_palette":
        return (
          <CommandPaletteContent
            preferences={preferences}
            state={state}
            categoryFilter={state?.modal?.data?.category_filter}
          />
        );
      case "hot_paths":
        return <HotPathsContent state={state} />;
      case "quick_connect":
        return (
          <QuickConnectContent
            connections={state?.modal?.data?.connections ?? []}
            state={state}
          />
        );
      case "settings":
        return <SettingsEditorContent preferences={preferences} />;
      case "connection_log":
        return <ConnectionLogContent state={state} />;
      default:
        return (
          <Dialog.Content
            className={dialogStyles.dialogContent}
            onCloseAutoFocus={preventAutoFocus}
          >
            <ModalContent
              state={state?.modal ?? null}
              mountLog={state?.mount_log}
            />
          </Dialog.Content>
        );
    }
  }

  return (
    <Dialog.Root
      open={isOpen}
      onOpenChange={(open) => {
        if (!open) closeModal();
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className={dialogStyles.dialogOverlay} />
        {renderContent()}
      </Dialog.Portal>
    </Dialog.Root>
  );
}
