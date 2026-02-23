#!/bin/sh
# Newt agent bootstrap script
# This script is sent to the remote host via the transport (ssh, docker, etc.)
# to detect the platform, cache and validate the agent binary, and exec it.
#
# Protocol:
#   stdout status lines (read by the Tauri host):
#     NEWT:READY              — cached binary valid, exec follows
#     NEWT:NEED:<triple>      — need binary upload for <triple>
#     NEWT:ERROR:<message>    — fatal error
#   On NEED, host writes to stdin:
#     <decimal size>\n        — byte count of the binary
#     <raw bytes>             — the binary itself
#   Then this script writes the binary to cache and execs it.

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
    CACHE_DIR="/tmp/newt-$(id -u)"
fi

mkdir -p "$CACHE_DIR" 2>/dev/null || true

AGENT_PATH="${CACHE_DIR}/newt-agent-${NEWT_HASH}"

# --- Check cached binary ---
if [ -x "$AGENT_PATH" ]; then
    echo "NEWT:READY"
else
    echo "NEWT:NEED:${TRIPLE}"

    # Read size line from stdin
    read -r SIZE

    # Validate size is a number
    case "$SIZE" in
        ''|*[!0-9]*)
            echo "NEWT:ERROR:invalid size: $SIZE" >&2
            exit 1
            ;;
    esac

    # Read binary from stdin using dd
    FULL_BLOCKS=$((SIZE / 4096))
    REMAINDER=$((SIZE % 4096))

    TMPFILE="${AGENT_PATH}.tmp.$$"

    if [ "$FULL_BLOCKS" -gt 0 ]; then
        dd bs=4096 count="$FULL_BLOCKS" of="$TMPFILE" 2>/dev/null
    fi
    if [ "$REMAINDER" -gt 0 ]; then
        dd bs="$REMAINDER" count=1 >> "$TMPFILE" 2>/dev/null
    fi

    chmod +x "$TMPFILE"
    mv "$TMPFILE" "$AGENT_PATH"

    # Clean up old versions
    for f in "${CACHE_DIR}"/newt-agent-*; do
        case "$f" in
            "${AGENT_PATH}"|"${AGENT_PATH}".*)
                ;;
            *)
                rm -f "$f" 2>/dev/null || true
                ;;
        esac
    done
fi

exec "$AGENT_PATH"

