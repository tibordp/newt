import iconMapping from "../assets/mapping.json";

const fileNames = iconMapping.light.fileNames as Record<string, string>;
const fileExtensions = iconMapping.light.fileExtensions as Record<
  string,
  string
>;
const iconDefinitions = iconMapping.iconDefinitions as unknown as Record<
  string,
  { fontCharacter: string; fontColor: string }
>;

/// Icon key for a name's extension, trying each dot-suffix from longest to
/// shortest. The mapping keys multi-part extensions (`test.ts`,
/// `eslintrc.json`, `css.map`) alongside plain ones, so the longest match has
/// to win, and a name carrying unrelated dotted segments — `abc.12.34.png` —
/// has to fall through them to the suffix that is a real extension.
function extensionIcon(name: string): string | undefined {
  for (
    let dot = name.indexOf(".");
    dot !== -1;
    dot = name.indexOf(".", dot + 1)
  ) {
    // Extension keys in the mapping are all lowercase, so a camera's `.JPG`
    // matches. Whole-name keys are not (`Makefile`, `Dockerfile`).
    const icon = fileExtensions[name.slice(dot + 1).toLowerCase()];
    if (icon) return icon;
  }
  return undefined;
}

/// Icon key for a file name: an exact whole-name match wins, then the
/// extension, then the generic file icon.
export function fileIconKey(name: string): string {
  return fileNames[name] || extensionIcon(name) || iconMapping.light.file;
}

/// Glyph and color to render a file name's icon with, from the bundled
/// Material icon font.
export function fileIconGlyph(name: string): { ch: string; color: string } {
  const { fontCharacter, fontColor } = iconDefinitions[fileIconKey(name)];
  return {
    ch: String.fromCodePoint(parseInt(fontCharacter, 16)),
    color: fontColor,
  };
}
