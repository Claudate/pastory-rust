#!/bin/zsh
# Rust port build: cargo → universal (ARCHS="arm64 x86_64") → bundle.
# Mirrors build.sh, minus SwiftPM: cargo is the build database, so no
# stale-per-triple dance is needed.
set -euo pipefail
cd "$(dirname "$0")/rust"

CONFIG="${CONFIG:-release}"
# A Developer ID identity in the keychain is used automatically (same
# signature as the shipped app, so permissions carry over between a local
# build and a release); otherwise ad-hoc.
if [ -z "${SIGN_ID:-}" ]; then
    IDS="$(security find-identity -v -p codesigning 2>/dev/null || true)"
    DEV="$(echo "$IDS" | grep -o '"Developer ID Application: [^"]*"' | head -1 | tr -d '"' || true)"
    if [ -n "$DEV" ]; then
        SIGN_ID="$DEV"
    else
        SIGN_ID="-"
    fi
fi

REPO="$(pwd)/.."

# ARCHS="arm64 x86_64" builds each slice with --target and lipo's them;
# default is this machine only.
if [ -n "${ARCHS:-}" ]; then
    SLICES=()
    for a in $ARCHS; do
        cargo build -r --target "$a-apple-darwin"
        SLICES+=("target/$a-apple-darwin/$CONFIG/pastory")
    done
    mkdir -p target/universal
    lipo -create "${SLICES[@]}" -output target/universal/pastory
    BIN="target/universal/pastory"
else
    cargo build -r
    BIN="target/$CONFIG/pastory"
fi

# Dev builds can ship under a different folder / display name so the Rust
# port and the shipped Pastory stay distinguishable side by side (bundle id
# — and therefore TCC — is unchanged; the executable name keeps keying the
# signature attribution).
APP="../build/${APP_FOLDER:-Pastory}.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# The bundle's executable keeps the Swift name (Finder, signatures,
# Screen Recording attribution all key on it).
cp "$BIN" "$APP/Contents/MacOS/Pastory"
cp "$REPO/Resources/Info.plist" "$APP/Contents/Info.plist"
if [ -n "${APP_TITLE:-}" ]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName ${APP_TITLE}" "$APP/Contents/Info.plist"
fi
[ -d "$REPO/Resources/Fonts" ] && cp -R "$REPO/Resources/Fonts" "$APP/Contents/Resources/Fonts"
for f in AppIcon.icns Logo.png Pushpin.png MenuIcon.png MenuIcon@2x.png WeChatGroup.png Coffee.png; do
    [ -f "$REPO/Resources/$f" ] && cp "$REPO/Resources/$f" "$APP/Contents/Resources/$f"
done

# A Developer ID identity gets the hardened runtime + secure timestamp that
# notarization requires.
SIGN_FLAGS=()
case "$SIGN_ID" in "Developer ID Application"*) SIGN_FLAGS=(--options runtime --timestamp);; esac
if ! codesign --force --sign "$SIGN_ID" ${SIGN_FLAGS[@]+"${SIGN_FLAGS[@]}"} --entitlements "$REPO/Resources/Pastory.entitlements" "$APP" 2>/dev/null; then
    codesign --force --sign "$SIGN_ID" ${SIGN_FLAGS[@]+"${SIGN_FLAGS[@]}"} "$APP"
fi
echo "→ $APP (signed: $SIGN_ID)"
