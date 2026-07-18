#!/bin/bash
set -euo pipefail

# Build a slim AppImage from a pre-built Newt binary + agents.
#
# Slim: no bundled GTK/WebKitGTK — the AppImage requires the same system
# libraries as the .deb/.rpm (see AppRun). Deliberately not tauri's
# AppImage bundler, which forces GDK_BACKEND=x11 and GTK_THEME and leaks
# LD_LIBRARY_PATH & co. into every process Newt spawns.
#
# Required env:
#   VERSION   — version for the output filename (e.g. 0.1.0)
#
# Optional env:
#   ARCH      — AppImage architecture label (default: x86_64)
#   BINARY    — path to the newt binary (default: target/release/newt)
#   AGENT_DIR — path to agents directory (default: agents)
#   APPIMAGETOOL — appimagetool command (default: appimagetool)

export ARCH="${ARCH:-x86_64}"
BINARY="${BINARY:-target/release/newt}"
AGENT_DIR="${AGENT_DIR:-agents}"
APPIMAGETOOL="${APPIMAGETOOL:-appimagetool}"

if [ -z "${VERSION:-}" ]; then
    echo "ERROR: VERSION must be set" >&2
    exit 1
fi

STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT
APPDIR="$STAGING/Newt.AppDir"

# Same payload as the native packages
make install DESTDIR="$APPDIR" PREFIX=/usr BINARY="$BINARY" AGENT_DIR="$AGENT_DIR"

# AppDir entry points: AppRun + top-level desktop file and icon
install -m755 packaging/appimage/AppRun "$APPDIR/AppRun"
install -m644 packaging/newt.desktop "$APPDIR/newt.desktop"
install -m644 src-tauri/icons/128x128.png "$APPDIR/newt.png"
ln -s newt.png "$APPDIR/.DirIcon"

OUT="Newt-${VERSION}-${ARCH}.AppImage"
"$APPIMAGETOOL" "$APPDIR" "$OUT"
echo "Built: $OUT"
