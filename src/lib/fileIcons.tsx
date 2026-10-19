import type React from "react";

import mapping from "../assets/file-icons.json";

/// An icon: a seti glyph, or one from Symbols Nerd Font's Material set
/// (`md`), in a palette colour resolved per theme through `--icon-<color>`.
export type FileIconGlyph = { ch: string; color: string | null; md: boolean };

type Entry = { char: string; color: string | null };

const seti = mapping.icons as Record<string, Entry>;

/// The glyph upstream gives an extension, for an addition that should look
/// like its sibling — `.heic` like `.png`.
function like(ext: string): FileIconGlyph {
  return glyph(seti[(mapping.fileExtensions as Record<string, string>)[ext]]);
}

function md(codepoint: number, color: string): FileIconGlyph {
  return { ch: String.fromCodePoint(codepoint), color, md: true };
}

function glyph({ char, color }: Entry): FileIconGlyph {
  return { ch: String.fromCodePoint(parseInt(char, 16)), color, md: false };
}

const MD_APPLE = 0xf0035;
const MD_DEBIAN = 0xf08da;
const MD_REDHAT = 0xf111b;
const MD_PENGUIN = 0xf0ec0;
const MD_ANDROID = 0xf0032;
const MD_DISC = 0xf05ee;
const MD_PUZZLE = 0xf0431;
const MD_FILE_POWERPOINT = 0xf0227;
const MD_BOOK = 0xf00ba;
const MD_PACKAGE = 0xf03d7;
const MD_CONSOLE = 0xf018d;

/// Space-separated keys to one glyph each.
function table(groups: [string, FileIconGlyph][]): Map<string, FileIconGlyph> {
  return new Map(
    groups.flatMap(([keys, g]) => keys.split(" ").map((k) => [k, g] as const)),
  );
}

/// Newt's additions, for what a file manager lists that a code editor's
/// theme doesn't cover. Upstream wins where both map a key.
export const ADDED_EXTENSIONS = table([
  [
    "bmp heic heif tif xcf jxl icns eps cr2 cr3 nef arw dng orf rw2",
    like("png"),
  ],
  ["m4a aac opus oga aiff aif wma mid midi", like("mp3")],
  ["mkv m4v wmv flv mpeg 3gp", like("mp4")],
  ["odt rtf pages", like("docx")],
  ["ods numbers", like("xlsx")],
  ["tsv", like("csv")],
  ["ttc", like("ttf")],
  ["sqlite sqlite3 db", like("sql")],
  ["ini plist", like("conf")],
  ["pub p12 pfx asc gpg sig", like("key")],
  [
    "tar tgz gz bz2 tbz tbz2 xz txz zst tzst lz4 lzma lz 7z rar cab cpio",
    like("zip"),
  ],
  ["exe msi msix appx com", like("bat")],
  ["app pkg dmg", md(MD_APPLE, "white")],
  ["deb", md(MD_DEBIAN, "pink")],
  ["rpm", md(MD_REDHAT, "red")],
  ["appimage flatpak snap", md(MD_PENGUIN, "yellow")],
  ["apk aab", md(MD_ANDROID, "green")],
  ["iso img", md(MD_DISC, "grey-light")],
  ["so dylib dll o a lib", md(MD_PUZZLE, "purple")],
  ["ppt pptx odp", md(MD_FILE_POWERPOINT, "orange")],
  ["epub mobi djvu", md(MD_BOOK, "purple")],
]);

export const ADDED_NAMES = table([
  [
    "cargo.lock go.sum authorized_keys known_hosts id_rsa id_ecdsa id_ed25519",
    like("key"),
  ],
]);

const fileNames = mapping.fileNames as Record<string, string>;
const fileExtensions = mapping.fileExtensions as Record<string, string>;

const PACKAGE = md(MD_PACKAGE, "orange");
const EXECUTABLE = md(MD_CONSOLE, "green");
const GENERIC = glyph(seti[mapping.file]);

/// The icon a name maps to: the whole name, then each dot-suffix from
/// longest to shortest. Keys are lowercase, matched as VS Code does. The
/// mapping keys multi-part extensions (`test.ts`, `eslintrc.json`) alongside
/// plain ones, so the longest match has to win, and a name carrying unrelated
/// dotted segments — `abc.12.34.png` — has to fall through them to the suffix
/// that is a real extension.
function nameIcon(name: string): FileIconGlyph | undefined {
  const lower = name.toLowerCase();
  const whole = fileNames[lower];
  if (whole) return glyph(seti[whole]);
  const added = ADDED_NAMES.get(lower);
  if (added) return added;
  for (
    let dot = lower.indexOf(".");
    dot !== -1;
    dot = lower.indexOf(".", dot + 1)
  ) {
    const ext = lower.slice(dot + 1);
    const icon = fileExtensions[ext];
    if (icon) return glyph(seti[icon]);
    const added = ADDED_EXTENSIONS.get(ext);
    if (added) return added;
  }
  return undefined;
}

/// What a file is besides its name, where the caller knows it.
export type FileIconFacts = {
  /// macOS presents the folder as a file (an app, an `.rtfd`).
  isPackage?: boolean;
  /// Any execute bit. A symlink's own mode says nothing about its target.
  executable?: boolean;
};

/// The icon for a file: its name's, else a package's or an executable's,
/// else the generic one.
export function fileIconGlyph(
  name: string,
  facts: FileIconFacts = {},
): FileIconGlyph {
  return (
    nameIcon(name) ??
    (facts.isPackage ? PACKAGE : facts.executable ? EXECUTABLE : GENERIC)
  );
}

/// A seti icon by its id in the mapping (`git`), for chrome outside the
/// file list.
export function setiGlyph(id: string): FileIconGlyph {
  return glyph(seti[id]);
}

/// The CSS colour for a glyph, through the current theme's palette.
export function iconColor(g: FileIconGlyph): string | undefined {
  return g.color ? `var(--icon-${g.color})` : undefined;
}

/// The file-list icon: the folder image, or the glyph for a file's name and
/// facts.
export function FileIcon({
  name,
  folder,
  ...facts
}: { name: string; folder: boolean } & FileIconFacts) {
  if (folder) return <div className="file-icon folder" aria-hidden />;
  const g = fileIconGlyph(name, facts);
  return (
    <div
      className={g.md ? "file-icon md" : "file-icon"}
      style={{ "--icon-color": iconColor(g) } as React.CSSProperties}
      aria-hidden
    >
      {g.ch}
    </div>
  );
}
