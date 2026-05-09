import {
  commands,
  type ModalContext,
  type ModalData,
} from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import About from "./About";
import Confirm from "./Confirm";
import ConnectRemote from "./ConnectRemote";
import CopyMove from "./CopyMove";
import CreateDirectory from "./CreateDirectory";
import CreateFile from "./CreateFile";
import Debug from "./Debug";
import MountK8s from "./MountK8s";
import MountS3 from "./MountS3";
import MountSftp from "./MountSftp";
import Navigate from "./Navigate";
import Properties from "./Properties";
import Rename from "./Rename";
import UserCommandInput from "./UserCommandInput";

export type { ModalContext, ModalData };

export type CommonDialogProps = {
  cancel: () => void;
  context?: ModalContext;
};

export function ModalContent({ state }: { state: ModalData | null }) {
  const commonProps: CommonDialogProps = {
    cancel: () => {
      safe(commands.closeModal());
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
    case "mount_s3":
      return <MountS3 {...commonProps} />;
    case "mount_sftp":
      return <MountSftp {...state.data} {...commonProps} />;
    case "mount_k8s":
      return <MountK8s {...state.data} {...commonProps} />;
    case "confirm":
      return <Confirm {...state.data} {...commonProps} />;
    case "properties":
      return <Properties {...state.data} {...commonProps} />;
    case "user_command_input":
      return <UserCommandInput {...state.data} {...commonProps} />;
    case "debug":
      return <Debug {...commonProps} />;
    case "about":
      return <About {...state.data} {...commonProps} />;
    default:
      return null;
  }
}
