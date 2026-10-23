import { viewer } from "../builders";
import captures from "../fixtures/captures.zip?url";
import recording from "../fixtures/captures.dissection.json";
import type { DissectLocated, DissectPage } from "../../../src/lib/bindings";
import type { Scene } from "../types";

// A packet capture inside a ZIP on S3, dissected down to a TCP header. The
// pages are what Newt's dissection returns for the fixture, recorded by
// `cargo test -p newt -- --ignored record_structure_scene`.

const path = "/captures/edge-2026-10-08.zip";
const pages = recording.pages as unknown as DissectPage[];
const byKey = new Map(pages.map((p) => [p.parent.key, p]));

const scene: Scene = {
  description:
    "The Structure viewer on a ZIP on S3: a packet capture inside it, dissected down to an IPv6 packet's TCP header",
  size: { width: 1180, height: 700 },
  window: "viewer",
  state: viewer({ vfs_id: 1, path }, "dissect", `s3://demo.netops${path}`),
  files: {
    [path]: { url: captures, mime: "application/zip", viewerMode: "dissect" },
  },
  commands: {
    dissect_open: () => ({ id: 1, page: pages[0] }),
    dissect_children: (args) => byKey.get(args.node as number),
    // Go to Offset lands on the TCP header by way of the recorded path.
    dissect_locate: (): DissectLocated => ({
      path: recording.path,
      pages: pages.slice(0, -1),
      secret: null,
    }),
    dissect_source: () => ({ len: 644, len_known: true, origin: null }),
    dissect_close: () => null,
  },
  steps: [
    { press: "Mod+G" },
    { type: recording.offset.toString(16) },
    { press: "Enter" },
    { press: "ArrowRight" },
  ],
};

export default scene;
