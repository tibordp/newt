#!/bin/bash
set -euo pipefail

# Build a .deb package from a pre-built Newt binary + agents.
#
# Required env:
#   VERSION   — package version (e.g. 0.1.0)
#   DEPS      — runtime dependencies for the Depends: field
#               (e.g. "libwebkit2gtk-4.1-0, libgtk-3-0, libayatana-appindicator3-1")
#
# Optional env:
#   ARCH      — Debian architecture (default: amd64)
#   DISTRO    — distro identifier for the output filename (default: generic)
#   BINARY    — path to the newt binary (default: target/release/newt)
#   AGENT_DIR — path to agents directory (default: agents)

ARCH="${ARCH:-amd64}"
DISTRO="${DISTRO:-generic}"
BINARY="${BINARY:-target/release/newt}"
AGENT_DIR="${AGENT_DIR:-agents}"

if [ -z "${VERSION:-}" ]; then
    echo "ERROR: VERSION must be set" >&2
    exit 1
fi
if [ -z "${DEPS:-}" ]; then
    echo "ERROR: DEPS must be set" >&2
    exit 1
fi

PKG="newt-fm_${VERSION}_${DISTRO}_${ARCH}"
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

# Install files via Makefile
make install DESTDIR="$STAGING" PREFIX=/usr BINARY="$BINARY" AGENT_DIR="$AGENT_DIR"

# Create DEBIAN/control
mkdir -p "$STAGING/DEBIAN"
cat > "$STAGING/DEBIAN/control" <<EOF
Package: newt-fm
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Tibor Djurica Potpara <tibor.djurica@ojdip.net>
Homepage: https://github.com/tibordp/newt
Description: Dual-pane file manager
 Newt is a keyboard-centric dual-pane file manager built with
 Tauri, featuring SSH remoting and virtual filesystem support.
Depends: ${DEPS}
Section: utils
Priority: optional
EOF

# Build the .deb
dpkg-deb --build --root-owner-group "$STAGING" "${PKG}.deb"
echo "Built: ${PKG}.deb"
