/// The platform look the token layer (`_platform.scss`) follows: the design
/// language as `data-platform` on the root, the system accent and UI font as
/// custom properties. Rust hands every window the current values before its
/// first paint (`window.__NEWT_APPEARANCE__`); a window asks again whenever
/// it regains focus, which is when a change made in the system settings
/// shows.

import { useSyncExternalStore } from "react";
import {
  commands,
  type PlatformStyle,
  type SystemAppearance,
} from "./bindings";

declare global {
  interface Window {
    __NEWT_APPEARANCE__?: SystemAppearance;
  }
}

/// The UI font size each platform ships with, where the system reports
/// none: macOS's 13pt system font, Windows' 9pt message font, GNOME's 11pt
/// and KDE's 10pt defaults.
const FONT_SIZE: Record<PlatformStyle, number> = {
  macos: 13,
  windows: 12,
  gnome: (11 * 96) / 72,
  kde: (10 * 96) / 72,
};

/// The macOS title bar before Tahoe, where Rust couldn't measure one.
const MACOS_TITLEBAR = 28;

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
    font_size: null,
    titlebar_height: null,
  };
}

let current: SystemAppearance = window.__NEWT_APPEARANCE__ ?? fallback();
let fullscreen = false;
const listeners = new Set<() => void>();

export function fontSize(a: SystemAppearance = current): number {
  return a.font_size ?? FONT_SIZE[a.platform];
}

function titlebarHeight(): number {
  if (current.platform !== "macos" || fullscreen) return 0;
  return current.titlebar_height ?? MACOS_TITLEBAR;
}

function apply() {
  const root = document.documentElement;
  root.dataset.platform = current.platform;
  const set = (name: string, value: string | null) =>
    value === null
      ? root.style.removeProperty(name)
      : root.style.setProperty(name, value);
  set("--system-accent", current.accent);
  set(
    "--font-system",
    current.font_family && JSON.stringify(current.font_family),
  );
  set("--font-size-base", `${fontSize()}px`);
  set("--titlebar-height", `${titlebarHeight()}px`);
}

function update(next: SystemAppearance) {
  if (JSON.stringify(next) === JSON.stringify(current)) return;
  current = next;
  apply();
  listeners.forEach((l) => l());
}

/// Applies the appearance this window was built with, and keeps it current.
/// Runs once per window, before the app renders.
export function initAppearance() {
  apply();
  window.addEventListener("focus", () => {
    commands.getSystemAppearance().then(update, () => {});
  });
}

/// macOS hides the title bar in full screen, so the strip under it goes too.
export function setFullscreen(value: boolean) {
  if (value === fullscreen) return;
  fullscreen = value;
  apply();
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
