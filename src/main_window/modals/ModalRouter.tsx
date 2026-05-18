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

// Modal types not rendered by this router. These are anchored per-pane
// (rendered inline by Pane via Radix DropdownMenu): the VFS selector and
// the history navigator. They each have their own outside-click /
// dismissal behavior driven by Radix.
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
    // WSL is Windows-only. Gating the sole reference to the import behind
    // the build-time `__WINDOWS__` literal lets Rollup DCE drop both this
    // branch and `SelectWslDistro.tsx` from non-Windows bundles.
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
            <ModalContent state={state?.modal ?? null} />
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
      <Dialog.Portal>{renderContent()}</Dialog.Portal>
    </Dialog.Root>
  );
}
