import { viewer } from "../builders";
import icon from "../../../app-icon.png?url";
import type {
  DissectNode,
  DissectPage,
  DissectValue,
} from "../../../src/lib/bindings";
import type { Scene } from "../types";

const path = "/Users/demo/Pictures/app-icon.png";

// The dissection of app-icon.png as fillyfoal produces it, trimmed to what
// the shot shows.

type Spec = {
  name: string;
  at: number;
  len: number;
  value?: DissectValue;
  summary?: string;
  description?: string;
  children?: Spec[];
  /// Children beyond those listed.
  more?: number;
};

const num = (n: number, hex = false): DissectValue =>
  hex
    ? { kind: "number", text: `0x${n.toString(16)}`, raw: String(n) }
    : { kind: "number", text: String(n), raw: `0x${n.toString(16)}` };
const text = (s: string): DissectValue => ({
  kind: "text",
  text: s,
  raw: null,
});
const enumValue = (name: string, raw: number): DissectValue => ({
  kind: "enum",
  text: name,
  raw: `0x${raw.toString(16)}`,
});

const chunk = (
  type: string,
  at: number,
  dataLen: number,
  summary: string,
  safe: boolean,
  data: Spec[],
  crc: number,
): Spec => ({
  name: type,
  at,
  len: dataLen + 12,
  summary,
  children: [
    { name: "Length", at, len: 4, value: num(dataLen) },
    {
      name: "Type",
      at: at + 4,
      len: 4,
      value: text(type),
      summary: `${type === "IHDR" || type === "IDAT" ? "critical" : "ancillary"}, public, ${safe ? "safe" : "unsafe"} to copy`,
    },
    ...data,
    {
      name: "CRC",
      at: at + 8 + dataLen,
      len: 4,
      value: num(crc, true),
      summary: "valid",
    },
  ],
});

const idat = (at: number, crc: number) =>
  chunk(
    "IDAT",
    at,
    0x2000,
    "0x2000 bytes",
    false,
    [{ name: "Data", at: at + 8, len: 0x2000 }],
    crc,
  );

const root: Spec = {
  name: "app-icon.png",
  at: 0,
  len: 0x1e7e9,
  summary: "2048×2048, 8-bit RGBA",
  more: 7,
  children: [
    { name: "Signature", at: 0, len: 8 },
    chunk(
      "IHDR",
      0x8,
      13,
      "2048×2048, 8-bit RGBA",
      false,
      [
        {
          name: "Header",
          at: 0x10,
          len: 13,
          children: [
            { name: "Width", at: 0x10, len: 4, value: num(2048) },
            { name: "Height", at: 0x14, len: 4, value: num(2048) },
            { name: "Bit depth", at: 0x18, len: 1, value: num(8) },
            {
              name: "Color type",
              at: 0x19,
              len: 1,
              value: enumValue("RGBA", 6),
              description:
                "How pixels are stored: 6 is RGB with an alpha channel, eight or sixteen bits per sample.",
            },
            { name: "Compression method", at: 0x1a, len: 1, value: num(0) },
            { name: "Filter method", at: 0x1b, len: 1, value: num(0) },
            {
              name: "Interlace method",
              at: 0x1c,
              len: 1,
              value: enumValue("None", 0),
            },
          ],
        },
      ],
      0xb2a7d330,
    ),
    chunk(
      "pHYs",
      0x21,
      9,
      "0x9 bytes",
      true,
      [{ name: "Physical dimensions", at: 0x29, len: 9 }],
      0x8fe5f165,
    ),
    chunk(
      "tEXt",
      0x36,
      25,
      "Software",
      true,
      [
        { name: "Keyword", at: 0x3e, len: 9, value: text("Software") },
        { name: "Text", at: 0x47, len: 16, value: text("www.inkscape.org") },
      ],
      0x9bee3c1a,
    ),
    idat(0x5b, 0xdbef9af8),
    idat(0x2067, 0xd76b26ee),
    idat(0x4073, 0x2122b659),
  ],
};

const specs: Spec[] = [];
const keyOf = new Map<Spec, number>();
const walk = (spec: Spec) => {
  keyOf.set(spec, specs.length);
  specs.push(spec);
  spec.children?.forEach(walk);
};
walk(root);

const toNode = (spec: Spec): DissectNode => ({
  key: keyOf.get(spec)!,
  name: spec.name,
  value: spec.value ?? null,
  summary: spec.summary ?? null,
  description: spec.description ?? null,
  span: { source: 0, offset: spec.at, len: spec.len },
  target: null,
  diagnostics: [],
  interpretation:
    spec === root
      ? {
          format: { name: "png", title: "Portable Network Graphics" },
          forced: false,
        }
      : null,
  children: spec.children
    ? {
        state: spec.more ? "more" : "unloaded",
        loaded: 0,
        count: spec.more
          ? { n: spec.children.length + spec.more, at_least: true }
          : null,
        error: null,
      }
    : { state: "leaf", loaded: 0, count: null, error: null },
});

const page = (spec: Spec): DissectPage => ({
  parent: {
    ...toNode(spec),
    children: {
      ...toNode(spec).children,
      state: spec.more ? "more" : "complete",
      loaded: spec.children?.length ?? 0,
    },
  },
  nodes: (spec.children ?? []).map(toNode),
  secret: null,
});

const scene: Scene = {
  description:
    "The Structure viewer on a PNG: the chunk tree, the selected field's bytes and its details",
  size: { width: 1180, height: 640 },
  window: "viewer",
  state: viewer({ vfs_id: 0, path }, "dissect"),
  files: { [path]: { url: icon, mime: "image/png" } },
  commands: {
    dissect_open: () => ({ id: 1, page: page(root) }),
    dissect_children: (args) => page(specs[args.node as number]),
    dissect_source: () => ({ len: root.len, len_known: true, origin: null }),
    dissect_close: () => null,
  },
  steps: [
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "ArrowRight" },
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "ArrowRight" },
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "ArrowDown" },
    { press: "ArrowDown" },
  ],
};

export default scene;
