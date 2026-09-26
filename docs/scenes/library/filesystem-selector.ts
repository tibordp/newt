import { behindDialog, docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "The filesystem selector with mounted and mountable locations",
  size: { width: 1000, height: 560 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "select_vfs",
    context: { pane_handle: 0 },
    data: {
      targets: [
        {
          vfs_id: 0,
          type_name: "local",
          display_name: "Local",
          label: null,
          mount_dialog: null,
          root: null,
          volume: null,
          available_bytes: 297_710_000_000,
        },
        {
          vfs_id: 1,
          type_name: "s3",
          display_name: "S3",
          label: "demo.backup",
          mount_dialog: null,
          root: null,
          volume: null,
          available_bytes: null,
        },
        {
          vfs_id: 4,
          type_name: "sftp",
          display_name: "SFTP",
          label: "demo@nas.home.arpa",
          mount_dialog: null,
          root: null,
          volume: null,
          available_bytes: null,
        },
        {
          vfs_id: null,
          type_name: "s3",
          display_name: "S3",
          label: null,
          mount_dialog: "mount_s3",
          root: null,
          volume: null,
          available_bytes: null,
        },
        {
          vfs_id: null,
          type_name: "sftp",
          display_name: "SFTP",
          label: null,
          mount_dialog: "mount_sftp",
          root: null,
          volume: null,
          available_bytes: null,
        },
        {
          vfs_id: null,
          type_name: "remote",
          display_name: "Remote",
          label: null,
          mount_dialog: "connect_remote",
          root: null,
          volume: null,
          available_bytes: null,
        },
      ],
    },
  }),
};

export default scene;
