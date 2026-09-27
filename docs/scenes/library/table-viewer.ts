import { viewer } from "../builders";
import orders from "../fixtures/orders.csv?url";
import type { Scene } from "../types";

const path = "/Users/demo/Documents/orders.csv";

const scene: Scene = {
  description: "Table viewer on a CSV, with a range selected",
  size: { width: 980, height: 560 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "table"),
  files: { [path]: { url: orders, mime: "text/csv" } },
  ready: '[data-row="0"][data-col="0"]',
  steps: [
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "ArrowRight" },
    { press: "ArrowRight" },
    { press: "ArrowRight" },
    { press: "Shift+ArrowDown" },
    { press: "Shift+ArrowDown" },
    { press: "Shift+ArrowDown" },
    { press: "Shift+ArrowRight" },
    { press: "Shift+ArrowRight" },
  ],
};

export default scene;
