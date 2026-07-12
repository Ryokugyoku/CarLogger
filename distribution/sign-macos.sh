#!/usr/bin/env bash
set -euo pipefail
binary="${1:?binary}"
if [[ -z "${APPLE_CERTIFICATE_BASE64:-}" ]]; then echo 'MACOS_SIGNING=disabled (unsigned development artifact)'; exit 0; fi
keychain="$RUNNER_TEMP/apex-trace.keychain-db"; certificate="$RUNNER_TEMP/apex-trace.p12"
trap 'security delete-keychain "$keychain" >/dev/null 2>&1 || true; rm -f "$certificate"' EXIT
printf '%s' "$APPLE_CERTIFICATE_BASE64" | base64 --decode > "$certificate"
security create-keychain -p temporary "$keychain"
security unlock-keychain -p temporary "$keychain"
security import "$certificate" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k temporary "$keychain"
codesign --force --options runtime --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$binary"
codesign --verify --strict --verbose=2 "$binary"
if [[ -n "${APPLE_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
  archive="$RUNNER_TEMP/apex-trace-notarize.zip"; ditto -c -k --keepParent "$binary" "$archive"
  xcrun notarytool submit "$archive" --apple-id "$APPLE_ID" --password "$APPLE_APP_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
else echo 'MACOS_NOTARIZATION=disabled (credentials incomplete)'; fi
