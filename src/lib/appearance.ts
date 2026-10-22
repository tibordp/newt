/// The platform look the token layer (`_platform.scss`) follows: the design
/// language as `data-platform` on the root, the system accent, UI font and
/// KDE colour scheme as custom properties, Mica as `data-backdrop`. Rust
/// hands every window the current values before its first paint
/// (`window.__NEWT_APPEARANCE__`); a window asks again whenever it regains
/// focus, which is when a change made in the system settings shows.

import { useSyncExternalStore } from "react";
import {
  commands,
  type ColorScheme,
  type PlatformStyle,
  type SystemAppearance,
} from "./bindings";

declare global {
  interface Window {
    __NEWT_APPEARANCE__?: SystemAppearance;
  }
}

/// The UI font size each look is drawn at: macOS's 13pt system font,
/// Windows' 9pt message font, and on Linux the size WebKitGTK's text matches
/// the desktop's default at — its 13px already reads as GTK's 11pt.
const FONT_SIZE: Record<PlatformStyle, number> = {
  macos: 13,
  windows: 12,
  gnome: 13,
  kde: 13,
};

/// The desktop's default UI font size in points, which `FONT_SIZE` stands
/// for: a desktop set larger or smaller scales Newt's text with it.
const DEFAULT_POINTS: Partial<Record<PlatformStyle, number>> = {
  gnome: 11,
  kde: 10,
};

/// Title bar heights where Rust measured none: macOS's before Tahoe, and
/// the header bar Newt draws on GNOME.
const TITLEBAR: Partial<Record<PlatformStyle, number>> = {
  macos: 28,
  gnome: 46,
};

const SCHEME_PROPERTIES: Record<
  Exclude<keyof ColorScheme, "dark">,
  `--scheme-${string}`
> = {
  window_bg: "--scheme-window-bg",
  window_fg: "--scheme-window-fg",
  window_fg_inactive: "--scheme-window-fg-inactive",
  view_bg: "--scheme-view-bg",
  view_fg: "--scheme-view-fg",
  header_bg: "--scheme-header-bg",
  header_bg_inactive: "--scheme-header-bg-inactive",
  header_fg: "--scheme-header-fg",
  button_bg: "--scheme-button-bg",
  button_fg: "--scheme-button-fg",
  selection_fg: "--scheme-selection-fg",
  focus: "--scheme-focus",
};

function fallback(): SystemAppearance {
  const ua = navigator.userAgent;
  const platform: PlatformStyle = /Mac/.test(ua)
    ? "macos"
    : /Windows/.test(ua)
      ? "windows"
      : "gnome";
  return {
    platform,
    accent: null,
    font_family: null,
    font_points: null,
    titlebar_height: null,
    title_leading: false,
    window_buttons: null,
    color_scheme: null,
    mica: false,
  };
}

let current: SystemAppearance = window.__NEWT_APPEARANCE__ ?? fallback();
let fullscreen = false;
const listeners = new Set<() => void>();
const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");

export function fontSize(a: SystemAppearance = current): number {
  const size = FONT_SIZE[a.platform];
  const points = DEFAULT_POINTS[a.platform];
  return a.font_points && points ? (size * a.font_points) / points : size;
}

function titlebarHeight(): number {
  if (fullscreen) return 0;
  return current.titlebar_height ?? TITLEBAR[current.platform] ?? 0;
}

function apply() {
  const root = document.documentElement;
  root.dataset.platform = current.platform;
  if (current.mica) root.dataset.backdrop = "mica";
  else delete root.dataset.backdrop;
  const set = (name: string, value: string | null | undefined) =>
    value == null
      ? root.style.removeProperty(name)
      : root.style.setProperty(name, value);
  set("--system-accent", current.accent);
  set(
    "--font-system",
    current.font_family && JSON.stringify(current.font_family),
  );
  set("--font-size-base", `${fontSize()}px`);
  set("--titlebar-height", `${titlebarHeight()}px`);
  // A colour scheme is light or dark; one that doesn't match the theme in
  // effect (the app's theme set apart from the desktop's) gives way to the
  // look's own colours.
  const scheme = current.color_scheme;
  const matches = !!scheme && scheme.dark === darkQuery.matches;
  for (const [key, property] of Object.entries(SCHEME_PROPERTIES)) {
    set(
      property,
      matches ? scheme[key as keyof typeof SCHEME_PROPERTIES] : null,
    );
  }
}

function changed() {
  apply();
  listeners.forEach((l) => l());
}

function update(next: SystemAppearance) {
  if (JSON.stringify(next) === JSON.stringify(current)) return;
  current = next;
  changed();
}

/// Applies the appearance this window was built with, and keeps it current.
/// Runs once per window, before the app renders.
export function initAppearance() {
  apply();
  darkQuery.addEventListener("change", apply);
  // While another window has the keyboard, focus marks (the cursor, the
  // active pane's dot, the terminal tab's line) turn grey, and so does
  // GNOME's header bar.
  const root = document.documentElement;
  const focusChanged = () => {
    if (document.hasFocus()) delete root.dataset.windowInactive;
    else root.dataset.windowInactive = "";
  };
  focusChanged();
  window.addEventListener("blur", focusChanged);
  window.addEventListener("focus", () => {
    focusChanged();
    commands.getSystemAppearance().then(update, () => {});
  });
}

/// macOS hides the title bar in full screen, and GNOME's header bar goes
/// with it, so the strip under it goes too.
export function setFullscreen(value: boolean) {
  if (value === fullscreen) return;
  fullscreen = value;
  changed();
}

export function useAppearance(): SystemAppearance {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
}

export function useFullscreen(): boolean {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => fullscreen,
  );
}
