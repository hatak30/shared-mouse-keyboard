#!/bin/bash
# Builds libsmk_ios.a for the platform Xcode is building for. Run from the Xcode build phase.
set -euo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [[ "${PLATFORM_NAME:-iphonesimulator}" == "iphoneos" ]]; then
  TARGET=aarch64-apple-ios
else
  TARGET=aarch64-apple-ios-sim
fi
cd "$ROOT"
cargo build -p smk-ios --release --target "$TARGET"
mkdir -p "$ROOT/ios/build"
cp "target/$TARGET/release/libsmk_ios.a" "$ROOT/ios/build/libsmk_ios.a"
