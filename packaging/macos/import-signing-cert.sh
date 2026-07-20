#!/bin/bash
set -euo pipefail

# Import the Developer ID Application certificate into a throwaway keychain
# and put it on the search list, so `codesign` and `cargo tauri build` resolve
# $APPLE_SIGNING_IDENTITY without being handed the cert directly. (Handing
# Tauri APPLE_CERTIFICATE would spawn a *second* keychain and make the identity
# ambiguous between the two.) The keychain and search-list change persist for
# later steps in the same job.
#
# CI-only: relies on $RUNNER_TEMP. No-op when $APPLE_CERTIFICATE is empty (a
# fork run without secrets) — the caller then produces an unsigned artifact.
#
# Required env when signing:
#   APPLE_CERTIFICATE           base64 of the Developer ID Application .p12
#   APPLE_CERTIFICATE_PASSWORD  export password for that .p12

if [ -z "${APPLE_CERTIFICATE:-}" ]; then
  echo "APPLE_CERTIFICATE not set — skipping keychain setup (unsigned build)."
  exit 0
fi

KEYCHAIN="$RUNNER_TEMP/app-signing.keychain-db"
KEYCHAIN_PW="$(openssl rand -base64 24)"

security create-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PW" "$KEYCHAIN"

CERT="$RUNNER_TEMP/cert.p12"
echo "$APPLE_CERTIFICATE" | base64 --decode > "$CERT"
security import "$CERT" -P "${APPLE_CERTIFICATE_PASSWORD:-}" \
  -A -t cert -f pkcs12 -k "$KEYCHAIN"
rm -f "$CERT"
security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$KEYCHAIN_PW" "$KEYCHAIN" >/dev/null
security list-keychains -d user -s "$KEYCHAIN" \
  $(security list-keychains -d user | sed 's/["]//g')
