import { describe, it, expect } from "vitest";

import { fileIconKey, fileIconGlyph } from "./fileIcons";

const PNG = "_image_light";
const TS = "_typescript_light";
const TS_TEST = "_typescript_1_light";
const GENERIC = "_default_light";

describe("fileIconKey", () => {
  it("resolves a plain extension", () => {
    expect(fileIconKey("photo.png")).toBe(PNG);
  });

  it("skips dotted segments that aren't extensions", () => {
    expect(fileIconKey("abc.12.34.png")).toBe(PNG);
    expect(fileIconKey("backup.2024-01-01.tar")).toBe(fileIconKey("x.tar"));
  });

  it("prefers the longest matching suffix", () => {
    expect(fileIconKey("thing.test.ts")).toBe(TS_TEST);
    expect(fileIconKey("thing.ts")).toBe(TS);
    // The multi-part key still wins when unrelated segments precede it.
    expect(fileIconKey("app.v2.test.ts")).toBe(TS_TEST);
  });

  it("matches extensions case-insensitively", () => {
    expect(fileIconKey("DSC00123.JPG")).toBe(fileIconKey("dsc.jpg"));
  });

  it("matches a whole name ahead of its extension", () => {
    expect(fileIconKey("Makefile")).not.toBe(GENERIC);
  });

  /// A leading dot opens the suffix walk rather than being consumed by it.
  it("resolves dotfiles with a compound extension", () => {
    expect(fileIconKey(".eslintrc.json")).not.toBe(fileIconKey("x.json"));
    expect(fileIconKey(".eslintrc.json")).not.toBe(GENERIC);
  });

  it("falls back to the generic file icon", () => {
    expect(fileIconKey("mystery.qqqq")).toBe(GENERIC);
    expect(fileIconKey("noextension")).toBe(GENERIC);
    expect(fileIconKey("trailing.")).toBe(GENERIC);
  });
});

describe("fileIconGlyph", () => {
  it("returns a glyph and a color", () => {
    const { ch, color } = fileIconGlyph("photo.png");
    expect(ch.length).toBeGreaterThan(0);
    expect(color).toMatch(/^#/);
  });
});
