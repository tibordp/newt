import { useCallback } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { safeCommand } from "../../lib/ipc";
import { MainWindowState } from "../types";
import { PreferencesState } from "../../lib/preferences";
import CommandPaletteContent from "./CommandPalette";
import HotPathsContent from "./HotPaths";
import SettingsEditorContent from "./SettingsEditor";
import { ModalContent } from "./ModalContent";
import dialogStyles from "./Dialog.module.scss";

const preventAutoFocus = (e: Event) => e.preventDefault();

// Modal types not rendered by this router (handled elsewhere, e.g. VfsSelector dropdown)
const EXCLUDED_MODAL_TYPES = ["select_vfs"];

export default function ModalRouter({
  state,
  preferences,
}: {
  state: MainWindowState | null;
  preferences: PreferencesState | null;
}) {
  const modalType = state?.modal?.type;
  const isOpen = !!modalType && !EXCLUDED_MODAL_TYPES.includes(modalType);

  const closeModal = useCallback(() => safeCommand("close_modal"), []);

  function renderContent() {
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
      case "settings":
        return <SettingsEditorContent preferences={preferences} />;
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
