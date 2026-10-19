import { describe, it, expect } from "vitest";

import mapping from "../assets/file-icons.json";
import {
  ADDED_EXTENSIONS,
  ADDED_NAMES,
  fileIconGlyph,
  setiGlyph,
} from "./fileIcons";

const icon = (name: string, facts = {}) => fileIconGlyph(name, facts);
const GENERIC = setiGlyph(mapping.file);

describe("fileIconGlyph", () => {
  it("resolves a plain extension", () => {
    expect(icon("photo.png")).toEqual(setiGlyph("image"));
  });

  it("skips dotted segments that aren't extensions", () => {
    expect(icon("abc.12.34.png")).toEqual(icon("x.png"));
    expect(icon("backup.2024-01-01.tar")).toEqual(icon("x.tar"));
  });

  it("prefers the longest matching suffix", () => {
    expect(icon("thing.test.ts")).not.toEqual(icon("thing.ts"));
    // The multi-part key still wins when unrelated segments precede it.
    expect(icon("app.v2.test.ts")).toEqual(icon("thing.test.ts"));
  });

  it("matches names and extensions case-insensitively", () => {
    expect(icon("DSC00123.JPG")).toEqual(icon("dsc.jpg"));
    expect(icon("LICENSE")).toEqual(icon("license"));
    expect(icon("LICENSE")).not.toEqual(GENERIC);
    expect(icon("CMakeLists.txt")).not.toEqual(icon("x.txt"));
  });

  it("matches a whole name ahead of its extension", () => {
    expect(icon("Makefile")).not.toEqual(GENERIC);
  });

  /// A leading dot opens the suffix walk rather than being consumed by it.
  it("resolves dotfiles with a compound extension", () => {
    expect(icon(".eslintrc.json")).not.toEqual(icon("x.json"));
    expect(icon(".eslintrc.json")).not.toEqual(GENERIC);
  });

  it("resolves the theme's language associations", () => {
    expect(icon("x.yml")).toEqual(icon("x.yaml"));
    expect(icon(".gitignore")).not.toEqual(GENERIC);
    expect(icon(".bashrc")).toEqual(icon("x.sh"));
    expect(icon("Gemfile")).toEqual(icon("x.rb"));
  });

  it("covers Newt's additions", () => {
    expect(icon("backup.tar.gz")).toEqual(icon("x.zip"));
    expect(icon("setup.exe")).toEqual(icon("x.bat"));
    expect(icon("IMG_0001.HEIC")).toEqual(icon("x.png"));
    expect(icon("ripgrep.deb").md).toBe(true);
    expect(icon("Cargo.lock")).toEqual(icon("x.key"));
  });

  it("gives an unmapped package the package glyph", () => {
    const pkg = icon("Notes.rtfd", { isPackage: true });
    expect(pkg.md).toBe(true);
    expect(pkg).not.toEqual(GENERIC);
    expect(icon("Safari.app", { isPackage: true })).toEqual(icon("Safari.app"));
  });

  it("gives an executable the console glyph only when its name has none", () => {
    const exe = icon("rg", { executable: true });
    expect(exe.md).toBe(true);
    expect(icon("python3.12", { executable: true })).toEqual(exe);
    expect(icon("deploy.sh", { executable: true })).toEqual(icon("deploy.sh"));
    expect(icon("rg")).toEqual(GENERIC);
  });

  it("falls back to the generic file icon", () => {
    expect(icon("mystery.qqqq")).toEqual(GENERIC);
    expect(icon("noextension")).toEqual(GENERIC);
    expect(icon("trailing.")).toEqual(GENERIC);
  });
});

describe("additions", () => {
  /// After a re-sync, an addition upstream now maps is dead weight.
  it("are not shadowed by upstream", () => {
    const upstreamNames = Object.keys(mapping.fileNames);
    const upstreamExtensions = Object.keys(mapping.fileExtensions);
    expect(
      [...ADDED_NAMES.keys()].filter((k) => upstreamNames.includes(k)),
    ).toEqual([]);
    expect(
      [...ADDED_EXTENSIONS.keys()].filter((k) =>
        upstreamExtensions.includes(k),
      ),
    ).toEqual([]);
  });

  it("are keyed in lowercase and resolve to a glyph", () => {
    for (const [k, g] of [...ADDED_NAMES, ...ADDED_EXTENSIONS]) {
      expect(k).toBe(k.toLowerCase());
      expect(g.ch.length).toBeGreaterThan(0);
    }
  });
});
