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

# Debian Policy wants the copyright file under the *binary package* name, so
# it does not fall out of the Makefile's generic /usr/share/doc/newt.
DOCDIR="$STAGING/usr/share/doc/newt-fm"
mkdir -p "$DOCDIR"
cp THIRD-PARTY-NOTICES.md "$DOCDIR/"
cat > "$DOCDIR/copyright" <<'EOF'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: newt
Source: https://github.com/tibordp/newt

Files: *
Copyright: 2023-2026 The Newt Authors
License: GPL-3.0-or-later

License: GPL-3.0-or-later
 This program is free software: you can redistribute it and/or modify it
 under the terms of the GNU General Public License as published by the Free
 Software Foundation, either version 3 of the License, or (at your option)
 any later version.
 .
 This program is distributed in the hope that it will be useful, but WITHOUT
 ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 more details.
 .
 On Debian systems the full text of the GNU General Public License version 3
 can be found in /usr/share/common-licenses/GPL-3.

Comment: Newt bundles third-party components under their own licences, all
 GPL-compatible. They are credited in THIRD-PARTY-NOTICES.md, installed
 alongside this file.
EOF

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
