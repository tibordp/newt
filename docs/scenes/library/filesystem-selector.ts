import { behindDialog, docsColumns } from "../demo";
import type { VfsTarget, VolumeKind } from "../../../src/lib/bindings";
import type { Scene } from "../types";

const target_ = (fields: Partial<VfsTarget>): VfsTarget => ({
  kind: "Mounted",
  vfs_id: null,
  type_name: "local",
  display_name: "Local",
  label: null,
  detail: null,
  mount_dialog: null,
  path: null,
  volume: null,
  available_bytes: null,
  ...fields,
});

const volume = (
  name: string,
  kind: VolumeKind,
  available_bytes: number,
  target: string | null = null,
): VfsTarget =>
  target_({
    kind: "Volume",
    vfs_id: 0,
    label: name,
    detail: target ?? `/Volumes/${name}`,
    path: `/Volumes/${name}`,
    volume: {
      kind,
      fs_type: null,
      label: name,
      target,
      mount_point: `/Volumes/${name}`,
    },
    available_bytes,
  });

const scene: Scene = {
  description:
    "The filesystem selector with mounted filesystems, volumes and mountable locations",
  size: { width: 1000, height: 560 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "select_vfs",
    context: { pane_handle: 0 },
    data: {
      targets: [
        target_({ vfs_id: 0, detail: "/", available_bytes: 297_710_000_000 }),
        volume("Backup", "Removable", 1_204_000_000_000),
        volume("Walker 0.9", "DiskImage", 33_180_000),
        volume(
          "media",
          "Network",
          3_870_000_000_000,
          "//demo@nas._smb._tcp.local/media",
        ),
        target_({
          vfs_id: 1,
          type_name: "s3",
          display_name: "S3",
          label: "demo.backup",
        }),
        target_({
          vfs_id: 4,
          type_name: "sftp",
          display_name: "SFTP",
          label: "demo@nas.home.arpa",
        }),
        target_({
          kind: "Connect",
          type_name: "s3",
          display_name: "S3",
          mount_dialog: "mount_s3",
        }),
        target_({
          kind: "Connect",
          type_name: "sftp",
          display_name: "SFTP",
          mount_dialog: "mount_sftp",
        }),
        target_({
          kind: "Connect",
          type_name: "remote",
          display_name: "Remote",
          mount_dialog: "connect_remote",
        }),
      ],
    },
  }),
};

export default scene;
