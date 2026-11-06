#!/usr/bin/env bash
# Build a signed macOS app bundle.
#
#   scripts/build-macos.sh            build target/release/bundle/macos/*.app
#   scripts/build-macos.sh --install  ... and replace /Applications copy, then launch it
#
# Signing identity: first "Developer ID Application" certificate in the
# keychain, or set APPLE_SIGNING_IDENTITY explicitly. A stable identity keeps
# macOS privacy permissions (Accessibility, Input Monitoring) across updates.
#
# Notarization (optional, for sharing with other people's Macs): also set
#   APPLE_ID, APPLE_PASSWORD (app-specific password) and APPLE_TEAM_ID
# and Tauri notarizes and staples the app automatically.
set -euo pipefail
cd "$(dirname "$0")/../app"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  APPLE_SIGNING_IDENTITY="$(security find-identity -v -p codesigning \
    | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)"
fi
if [[ -z "$APPLE_SIGNING_IDENTITY" ]]; then
  echo "No 'Developer ID Application' certificate found; building ad-hoc signed." >&2
  APPLE_SIGNING_IDENTITY="-"
fi
export APPLE_SIGNING_IDENTITY

# Update artifacts must be signed with the updater key (see README).
KEY="$HOME/.tauri/shared-mouse-keyboard-updater.key"
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" && -f "$KEY" ]]; then
  export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY")"
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$KEY.password")"
fi
echo "Signing with: $APPLE_SIGNING_IDENTITY"

cargo tauri build --bundles app

APP="../target/release/bundle/macos/Shared Mouse Keyboard.app"
codesign --verify --deep --strict "$APP"
codesign -dv "$APP" 2>&1 | grep -E "Authority=Developer|TeamIdentifier|flags" || true

if [[ "${1:-}" == "--install" ]]; then
  osascript -e 'quit app "Shared Mouse Keyboard"' 2>/dev/null || true
  sleep 1
  pkill -f "Shared Mouse Keyboard.app/Contents/MacOS" 2>/dev/null || true
  rm -rf "/Applications/Shared Mouse Keyboard.app"
  cp -R "$APP" /Applications/
  open "/Applications/Shared Mouse Keyboard.app"
  echo "Installed to /Applications and launched."
fi
