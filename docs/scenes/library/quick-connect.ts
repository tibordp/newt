import { behindDialog, docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "Quick Connect with saved profiles and recent connections",
  size: { width: 1000, height: 600 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "quick_connect",
    context: { pane_handle: 0 },
    data: {
      connections: [
        {
          id: "1",
          name: "build-01",
          type: "ssh",
          host: "build-01",
          open_in: "window",
        },
        {
          id: "2",
          name: "NAS",
          type: "sftp",
          host: "demo@nas.home.arpa",
          open_in: "pane",
        },
        {
          id: "3",
          name: "Backups",
          type: "s3",
          bucket: "demo.backup",
          region: "eu-central-1",
          open_in: "pane",
        },
        {
          id: "4",
          name: "api (staging)",
          type: "kube",
          context: "staging",
          namespace: "api",
          pod: "api-7c9f8d6b54-x2lqk",
          open_in: "window",
        },
      ],
      recent_connections: [
        { type: "docker", container: "postgres-dev", open_in: "pane" },
        { type: "ssh", host: "demo@pi.home.arpa", open_in: "window" },
      ],
    },
  }),
};

export default scene;
