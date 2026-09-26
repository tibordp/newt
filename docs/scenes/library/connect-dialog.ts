import { behindDialog, docsColumns } from "../demo";
import type { Scene } from "../types";

const scene: Scene = {
  description: "The Connect dialog opening an SSH session in a new window",
  size: { width: 1000, height: 640 },
  ...docsColumns(1000),
  window: "main",
  state: behindDialog({
    type: "connect_remote",
    context: { pane_handle: 0 },
    data: {
      initial: { type: "ssh", host: "build-01" },
      default_open_in: "window",
      edit: null,
      connect_on_open: false,
    },
  }),
  commands: {
    discover_ssh_hosts: () => ({
      items: [
        { host: "build-01", hostname: "build-01.internal", user: "demo" },
        { host: "nas", hostname: "nas.home.arpa", user: "demo" },
        { host: "pi", hostname: "pi.home.arpa", user: "pi" },
      ],
      warning: null,
    }),
  },
};

export default scene;
