import { safeCommand } from "../../lib/ipc";

import Confirm from "./Confirm";
import ConnectRemote from "./ConnectRemote";
import CopyMove from "./CopyMove";
import CreateDirectory from "./CreateDirectory";
import CreateFile from "./CreateFile";
import MountSftp from "./MountSftp";
import Navigate from "./Navigate";
import Properties from "./Properties";
import Rename from "./Rename";
import UserCommandInput from "./UserCommandInput";

export type ModalState = {
  type: string;
  data: any;
  context: Context;
};

export type Context = {
  pane_handle?: number;
};

export type CommonDialogProps = {
  cancel: () => void;
  context?: Context;
};

export function ModalContent({ state }: { state: ModalState | null }) {
  const commonProps = {
    cancel: () => {
      safeCommand("close_modal");
    },
    context: state?.context,
  };

  switch (state?.type) {
    case "create_directory":
      return <CreateDirectory {...state.data} {...commonProps} />;
    case "create_file":
      return <CreateFile {...state.data} {...commonProps} />;
    case "navigate":
      return <Navigate {...state.data} {...commonProps} />;
    case "rename":
      return <Rename {...state.data} {...commonProps} />;
    case "copy_move":
      return <CopyMove {...state.data} {...commonProps} />;
    case "connect_remote":
      return <ConnectRemote {...state.data} {...commonProps} />;
    case "mount_sftp":
      return <MountSftp {...state.data} {...commonProps} />;
    case "confirm":
      return <Confirm {...state.data} {...commonProps} />;
    case "properties":
      return <Properties {...state.data} {...commonProps} />;
    case "user_command_input":
      return <UserCommandInput {...state.data} {...commonProps} />;
    default:
      return null;
  }
}
