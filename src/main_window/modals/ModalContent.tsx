import { safeCommand } from "../../lib/ipc";

import CreateDirectory from "./CreateDirectory";
import Navigate from "./Navigate";

import "./ModalContent.scss";
import Rename from "./Rename";

export type ModalState = {
  type: string;
  data: any;
};

export type Context = {
  pane_handle?: number;
};

export type CommonDialogProps = {
  cancel: () => void;
  context?: Context;
};

export function ModalContent({ state }) {
  const commonProps = {
    cancel: () => {
      safeCommand("close_modal");
    },
    context: state?.context,
  };

  switch (state?.type) {
    case "create_directory":
      return <CreateDirectory {...state.data} {...commonProps} />;
    case "navigate":
      return <Navigate {...state.data} {...commonProps} />;
    case "rename":
      return <Rename {...state.data} {...commonProps} />;
    default:
      return null;
  }
}
