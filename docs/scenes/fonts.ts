/// Each platform's UI and monospace fonts, which the Mac rendering the
/// scenes doesn't have: GNOME's Adwaita Sans and Mono and KDE's Noto Sans
/// and Hack as they are, Cascadia Mono, and Open Sans under Segoe UI's
/// names — by Segoe's designer, and openly licensed where Segoe isn't.
/// Only the platform's own are loaded, so a font list falls through to
/// what that platform would have.

import type { PlatformStyle } from "../../src/lib/bindings";
import openSans400 from "@fontsource/open-sans/files/open-sans-latin-400-normal.woff2?url";
import openSans500 from "@fontsource/open-sans/files/open-sans-latin-500-normal.woff2?url";
import openSans600 from "@fontsource/open-sans/files/open-sans-latin-600-normal.woff2?url";
import openSans700 from "@fontsource/open-sans/files/open-sans-latin-700-normal.woff2?url";

const SEGOE: [number, string][] = [
  [400, openSans400],
  [500, openSans500],
  [600, openSans600],
  [700, openSans700],
];

export async function loadFonts(platform: PlatformStyle) {
  switch (platform) {
    case "windows":
      for (const family of ["Segoe UI Variable Text", "Segoe UI"]) {
        for (const [weight, url] of SEGOE) {
          document.fonts.add(
            new FontFace(family, `url(${url})`, { weight: String(weight) }),
          );
        }
      }
      await Promise.all([
        import("@fontsource/cascadia-mono/400.css"),
        import("@fontsource/cascadia-mono/700.css"),
      ]);
      break;
    case "gnome":
      await Promise.all([
        import("@fontsource/adwaita-sans/400.css"),
        import("@fontsource/adwaita-sans/500.css"),
        import("@fontsource/adwaita-sans/600.css"),
        import("@fontsource/adwaita-sans/700.css"),
        import("@fontsource/adwaita-mono/400.css"),
        import("@fontsource/adwaita-mono/700.css"),
      ]);
      break;
    case "kde":
      await Promise.all([
        import("@fontsource/noto-sans/400.css"),
        import("@fontsource/noto-sans/500.css"),
        import("@fontsource/noto-sans/600.css"),
        import("@fontsource/noto-sans/700.css"),
        import("hack-font/build/web/hack.css"),
      ]);
      break;
    case "macos":
      break;
  }
}
