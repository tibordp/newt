import { behindDialog, docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "The S3 mount dialog using a named AWS profile",
  size: { width: 1000, height: 640 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "mount_s3",
    context: { pane_handle: 1 },
    data: {
      initial: {
        type: "s3",
        credential_mode: "profile",
        profile: "demo",
        region: "eu-central-1",
        bucket: "demo.backup",
      },
      edit: null,
      connect_on_open: false,
    },
  }),
};

export default scene;
