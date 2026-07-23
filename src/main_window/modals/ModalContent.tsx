import {
  commands,
  type ModalContext,
  type ModalData,
} from "../../lib/bindings";
import { safe } from "../../lib/ipc";
import About from "./About";
import ConfirmDelete from "./ConfirmDelete";
import ConfirmUnmapDrive from "./ConfirmUnmapDrive";
import ConnectRemote from "./ConnectRemote";
import CopyMove from "./CopyMove";
import CreateArchive from "./CreateArchive";
import CreateDirectory from "./CreateDirectory";
import CreateFile from "./CreateFile";
import Debug from "./Debug";
import MountK8s from "./MountK8s";
import MountS3 from "./MountS3";
import MountSftp from "./MountSftp";
import Navigate from "./Navigate";
import Properties from "./Properties";
import Rename from "./Rename";
import SearchDialog from "./Search";
import UserCommandInput from "./UserCommandInput";

export type { ModalContext, ModalData };

export type CommonDialogProps = {
  cancel: () => void;
  context?: ModalContext;
};

/// Extract the `data` payload of a specific modal variant. Dialog components
/// use this so their props track the codegen `ModalData` discriminated union
/// without duplicating field shapes.
export type ModalDataOf<
  K extends Extract<ModalData, { data: unknown }>["type"],
> = Extract<ModalData, { type: K; data: unknown }>["data"];

export function ModalContent({
  state,
  mountLog,
}: {
  state: ModalData | null;
  mountLog?: string[];
}) {
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
    case "create_archive":
      return <CreateArchive {...state.data} {...commonProps} />;
    case "connect_remote":
      return (
        <ConnectRemote {...state.data} {...commonProps} mountLog={mountLog} />
      );
    case "mount_s3":
      return <MountS3 {...state.data} {...commonProps} />;
    case "mount_sftp":
      return <MountSftp {...state.data} {...commonProps} />;
    case "mount_k8s":
      return <MountK8s {...state.data} {...commonProps} />;
    case "search":
      return <SearchDialog {...state.data} {...commonProps} />;
    case "confirm_delete":
      return <ConfirmDelete {...state.data} {...commonProps} />;
    case "confirm_unmap_drive":
      return <ConfirmUnmapDrive {...state.data} {...commonProps} />;
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
