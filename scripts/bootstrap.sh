#!/bin/sh
# Newt agent bootstrap script
# This script is sent to the remote host via the transport (ssh, docker, etc.)
# to detect the platform, cache and validate the agent binary, and exec it.
#
# Protocol:
#   stdout status lines (read by the Tauri host):
#     NEWT:READY              — agent installed, exec follows
#     NEWT:NEED:<triple>      — need binary upload for <triple>
#     NEWT:ERROR:<message>    — fatal error
#   On NEED, host writes to stdin:
#     <decimal size>\n        — byte count of the binary
#     <raw bytes>             — the binary itself
#   Then this script installs the binary, confirms with NEWT:READY, and
#   execs it. The confirmation is load-bearing: the host must not write
#   RPC bytes into stdin until the upload has been fully consumed —
#   some `head -c` implementations (BSD/macOS) read ahead and would
#   silently swallow them.
#
#   NEWT_FORCE_UPLOAD=1, injected by the host when its previous attempt
#   failed, asks for the upload even when a cached binary exists,
#   replacing one that is there but broken.

set -e

NEWT_HASH="__NEWT_HASH__"

# --- Detect platform ---
OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
    Linux)  OS_PART="unknown-linux-musl" ;;
    Darwin) OS_PART="apple-darwin" ;;
    *)
        echo "NEWT:ERROR:unsupported OS: $OS"
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64|amd64)   ARCH_PART="x86_64" ;;
    aarch64|arm64)   ARCH_PART="aarch64" ;;
    *)
        echo "NEWT:ERROR:unsupported architecture: $ARCH"
        exit 1
        ;;
esac

TRIPLE="${ARCH_PART}-${OS_PART}"

# --- Determine cache directory ---
if [ -n "${XDG_CACHE_HOME}" ]; then
    CACHE_DIR="${XDG_CACHE_HOME}/newt"
elif [ -n "${HOME}" ]; then
    CACHE_DIR="${HOME}/.cache/newt"
else
    # No home to cache in: a private directory for this connection alone.
    # A fixed name in /tmp could be one another user made first, holding
    # an "agent" of their own.
    CACHE_DIR=$(mktemp -d "${TMPDIR:-/tmp}/newt.XXXXXX")
fi

mkdir -p "$CACHE_DIR" 2>/dev/null || true

# Keyed by platform too: a home shared between machines holds one agent
# for each.
AGENT_PATH="${CACHE_DIR}/newt-agent-${NEWT_HASH}-${TRIPLE}"

# --- Check cached binary ---
if [ -x "$AGENT_PATH" ] && [ -z "${NEWT_FORCE_UPLOAD}" ]; then
    echo "NEWT:READY"
else
    # Detect available decompression tools
    CAPS=""
    if command -v gzip >/dev/null 2>&1; then
        CAPS="gzip"
    fi

    echo "NEWT:NEED:${TRIPLE}:${CAPS}"

    # Read transfer header: "SIZE ENCODING\n"
    #   ENCODING is "gzip" or "raw"
    read -r SIZE ENCODING

    # Validate size is a number
    case "$SIZE" in
        ''|*[!0-9]*)
            echo "NEWT:ERROR:invalid size: $SIZE" >&2
            exit 1
            ;;
    esac

    # Read exact byte count from stdin, decompress if needed. A short read
    # (the connection dropping mid-upload) must not install a truncated
    # agent: gzip fails on a cut-off stream, and a raw upload is counted.
    TMPFILE="${AGENT_PATH}.tmp.$$"
    case "$ENCODING" in
        gzip)
            if ! head -c "$SIZE" | gzip -d > "$TMPFILE"; then
                rm -f "$TMPFILE"
                echo "NEWT:ERROR:agent upload was cut short"
                exit 1
            fi
            ;;
        *)
            head -c "$SIZE" > "$TMPFILE"
            if [ "$(wc -c < "$TMPFILE")" -ne "$SIZE" ]; then
                rm -f "$TMPFILE"
                echo "NEWT:ERROR:agent upload was cut short"
                exit 1
            fi
            ;;
    esac

    chmod +x "$TMPFILE"
    mv "$TMPFILE" "$AGENT_PATH"

    # Clean up this platform's other versions, and names without a
    # platform. Another platform's agents belong to another machine
    # sharing this home.
    for f in "${CACHE_DIR}"/newt-agent-*; do
        case "${f##*/}" in
            "${AGENT_PATH##*/}"|"${AGENT_PATH##*/}".*)
                ;;
            newt-agent-*-"${TRIPLE}"|newt-agent-*-"${TRIPLE}".tmp.*)
                rm -f "$f" 2>/dev/null || true
                ;;
            newt-agent-*-*-*)
                ;;
            *)
                rm -f "$f" 2>/dev/null || true
                ;;
        esac
    done

    # Confirm the upload is fully consumed — the host holds off on RPC
    # traffic until it sees this (see protocol note above).
    echo "NEWT:READY"
fi

# Forward RUST_LOG from host (passed as NEWT_RUST_LOG to avoid shell conflicts)
if [ -n "${NEWT_RUST_LOG}" ]; then
    export RUST_LOG="${NEWT_RUST_LOG}"
fi

# NEWT_AGENT_MODE=vfs is injected by the spawner for FS-only agents
# (pane-scoped agent mounts).
exec "$AGENT_PATH" ${NEWT_AGENT_MODE:+--serve-vfs}

