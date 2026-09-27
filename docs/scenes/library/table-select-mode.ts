import { viewer } from "../builders";
import orders from "../fixtures/orders.csv?url";
import type { Scene } from "../types";

const path = "/Users/demo/Documents/orders.csv";

const scene: Scene = {
  description: "Table viewer's select mode, for copying part of a cell",
  size: { width: 980, height: 560 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "table"),
  files: { [path]: { url: orders, mime: "text/csv" } },
  ready: '[data-row="0"][data-col="0"]',
  steps: [
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "End" },
    { press: "Enter" },
    { waitFor: "textarea" },
  ],
};

export default scene;
