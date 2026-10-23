/// The platform a scene renders as (`?platform=`), and the demo machine's
/// local paths as that platform has them. Scenes are written with macOS's
/// paths; on Linux the home folder is `/home/demo` and volumes mount under
/// `/run/media/demo`, on Windows they are drives, carried in the app's
/// wire form (`/?/C:/Users/demo`) and shown as `C:\Users\demo`.

import type { PlatformStyle } from "../../src/lib/bindings";

export const PLATFORM = (new URLSearchParams(location.search).get("platform") ??
  "macos") as PlatformStyle;

/// GNOME and KDE are both Linux.
export const OS: "macos" | "windows" | "linux" =
  PLATFORM === "gnome" || PLATFORM === "kde" ? "linux" : PLATFORM;

/// macOS path prefix → Linux and Windows counterparts, most specific first.
const ROOTS: [string, { linux: string; windows: string }][] = [
  ["/Users/demo", { linux: "/home/demo", windows: "C:\\Users\\demo" }],
  ["/Users", { linux: "/home", windows: "C:\\Users" }],
  [
    "/Volumes/Walker 0.9",
    { linux: "/run/media/demo/Walker 0.9", windows: "E:\\" },
  ],
  ["/Volumes/Backup", { linux: "/run/media/demo/Backup", windows: "D:\\" }],
  ["/Volumes/media", { linux: "/run/media/demo/media", windows: "Z:\\" }],
  ["/var/log", { linux: "/var/log", windows: "C:\\Logs" }],
];

const rootOf = (path: string) =>
  ROOTS.find(([mac]) => path === mac || path.startsWith(`${mac}/`));

/// A local path as the platform shows it. Paths outside the demo machine's
/// known places are left as written.
export function localDisplay(path: string): string {
  if (path === "/" && OS === "windows") return "C:\\";
  const root = rootOf(path);
  if (OS === "macos" || !root) return path;
  const [mac, native] = root;
  const rest = path.slice(mac.length);
  if (OS === "linux") return native.linux + rest;
  const base = native.windows.replace(/\\$/, "");
  const shown = base + rest.replace(/\//g, "\\");
  return /^[A-Z]:$/.test(shown) ? `${shown}\\` : shown;
}

/// A local path as `VfsPath.path` carries it.
export function localWire(path: string): string {
  if (OS !== "windows" || !rootOf(path)) return localDisplay(path);
  return `/?/${localDisplay(path).replace(/\\$/, "").replace(/\\/g, "/")}`;
}

/// Whether a local wire path is a root, with nothing above it.
export const isLocalRoot = (wire: string) =>
  wire === "/" || /^\/\?\/[A-Z]:$/.test(wire);

/// The breadcrumbs Rust builds for a local path (`local_breadcrumbs`).
export function localBreadcrumbs(vfsId: number, path: string) {
  const wire = localWire(path);
  const comps = wire.split("/").filter(Boolean);
  const windows = comps[0] === "?";
  const sep = windows ? "\\" : "/";
  const rootDepth = windows ? 2 : 0;
  const crumb = (label: string, depth: number) => ({
    label,
    path: { vfs_id: vfsId, path: `/${comps.slice(0, depth).join("/")}` },
    note: null,
  });
  return [
    crumb(windows ? `${comps[1]}\\` : "/", rootDepth),
    ...comps
      .slice(rootDepth)
      .map((seg, i, rest) =>
        crumb(i === rest.length - 1 ? seg : `${seg}${sep}`, rootDepth + i + 1),
      ),
  ];
}

const PATH_START = /^\/(Users|Volumes|var\/log)(\/|$)/;
const EMBEDDED =
  /(?<![\w:./~-])\/(?:Users|Volumes|var\/log)(?:\/[^\s"'`|<>]*)?/g;

/// Local paths in text as the platform shows them: the whole text when it
/// is a path (spaces and all), else each path in it, up to whitespace.
export function localizeText(text: string): string {
  if (OS === "macos") return text;
  if (PATH_START.test(text) && !text.includes("\n")) return localDisplay(text);
  return text.replace(EMBEDDED, localDisplay);
}

/// A scene's data with its local paths the platform's: `VfsPath`s on the
/// local filesystem (VFS 0) in wire form, paths in text shown.
export function localize<T>(value: T): T {
  if (OS === "macos") return value;
  if (typeof value === "string") return localizeText(value) as T;
  if (Array.isArray(value)) return value.map(localize) as T;
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [key, v] of Object.entries(value)) out[key] = localize(v);
    const local = value as { vfs_id?: unknown; path?: unknown };
    if (local.vfs_id === 0 && typeof local.path === "string") {
      out.path = localWire(local.path);
    }
    return out as T;
  }
  return value;
}
