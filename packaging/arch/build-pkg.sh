#!/bin/bash
set -euo pipefail

# Build an Arch Linux package (.pkg.tar.zst) from a pre-built Newt binary + agents.
#
# Required env:
#   VERSION   — package version (e.g. 0.1.0)
#
# Optional env:
#   ARCH      — package architecture (default: x86_64)
#   BINARY    — path to the newt binary (default: target/release/newt)
#   AGENT_DIR — path to agents directory (default: agents)

ARCH="${ARCH:-x86_64}"
BINARY="${BINARY:-target/release/newt}"
AGENT_DIR="${AGENT_DIR:-agents}"

if [ -z "${VERSION:-}" ]; then
    echo "ERROR: VERSION must be set" >&2
    exit 1
fi

PKG="newt-fm-${VERSION}-1-${ARCH}"
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT

# Install files via Makefile
make install DESTDIR="$STAGING" PREFIX=/usr BINARY="$BINARY" AGENT_DIR="$AGENT_DIR"

# Create .PKGINFO
cat > "$STAGING/.PKGINFO" <<EOF
pkgname = newt-fm
pkgver = ${VERSION}-1
pkgdesc = Dual-pane file manager
url = https://github.com/tibordp/newt
builddate = $(date +%s)
size = $(du -sb "$STAGING" | cut -f1)
arch = ${ARCH}
license = GPL-2.0-only
depend = webkit2gtk-4.1
depend = gtk3
depend = libappindicator-gtk3
EOF

# Build the package
cd "$STAGING"
bsdtar -cf "${OLDPWD}/${PKG}.pkg.tar.zst" --zstd .PKGINFO usr/
cd "$OLDPWD"
echo "Built: ${PKG}.pkg.tar.zst"
