import type { CSSProperties } from "react";
import { usePreferences } from "./preferences";

/// The `[editor]` or `[viewer]` font, with the line height its rows take:
/// 18px at the default 13px.
export function useMonospaceFont(section: "editor" | "viewer") {
  const settings = usePreferences()?.settings[section];
  const family = settings?.font_family ?? "monospace";
  const size = settings?.font_size ?? 13;
  const lineHeight = Math.round(size * 1.4);
  /// As the custom properties the viewer's stylesheets read.
  const style = {
    "--viewer-font": family,
    "--viewer-font-size": `${size}px`,
    "--viewer-line-height": `${lineHeight}px`,
  } as CSSProperties;
  return { family, size, lineHeight, style };
}
