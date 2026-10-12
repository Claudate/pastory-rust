#!/bin/zsh
# Mac App Store variant: cargo --features mas → bundle → provisioning →
# sandbox entitlements → (pkg when the store certs are present).
# Behaviour fork lives in rust/src (feature `mas`, docs/MAS_PLAN.md §2-3).
#
# Required for an UPLOADABLE product:
#   PROVISION_PROFILE=<path>   App Store provisioning profile for the bundle
#                              id (portal/Xcode; embedded at Contents/embedded.provisionprofile)
#   keychain "Apple Distribution" identity + an installer identity for
#   productbuild (auto-detected, override with SIGN_ID / INSTALLER_SIGN_ID).
# Without them the script still emits build/PasteJar-mas.app ad-hoc-signed
# with the sandbox entitlements — local smoke testing only, Transporter will
# reject it (that is expected, not a bug).
set -euo pipefail
cd "$(dirname "$0")/rust"

CONFIG="${CONFIG:-release}"
if [ -z "${SIGN_ID:-}" ]; then
    IDS="$(security find-identity -v -p codesigning 2>/dev/null || true)"
    DIST="$(echo "$IDS" | grep -o '"Apple Distribution: [^"]*"' | head -1 | tr -d '"' || true)"
    SIGN_ID="${DIST:--}"
fi
INSTALLER_ID="${INSTALLER_SIGN_ID:-}"
if [ -z "$INSTALLER_ID" ] && [ "$SIGN_ID" != "-" ]; then
    INSTALLER_ID="$( (security find-identity -v -p codesigning 2>/dev/null || true) \
        | grep -o '"3rd Party Mac Developer Installer: [^"]*"' | head -1 | tr -d '"' || true)"
fi

REPO="$(pwd)/.."

# PASTEJAR_FEED_HOST is baked into the client exactly like build-rs.sh does it
# (cargo inherits the shell env). A store build should point at the feed so
# the daily check stays mainland-friendly.
if [ -n "${ARCHS:-}" ]; then
    SLICES=()
    for a in $=ARCHS; do
        cargo build -r --features mas --target "$a-apple-darwin"
        SLICES+=("target/$a-apple-darwin/$CONFIG/pastory")
    done
    mkdir -p target/universal-mas
    lipo -create "${SLICES[@]}" -output target/universal-mas/pastory
    BIN="target/universal-mas/pastory"
else
    cargo build -r --features mas
    BIN="target/$CONFIG/pastory"
fi

# Bundle assembly mirrors build-rs.sh (same names — the store listing decides
# the final display name via the bundle id swap round, MAS_PLAN P0/bundle-id).
APP="../build/${APP_FOLDER:-PasteJar-mas}.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/PasteJar"
cp "$REPO/Resources/Info.plist" "$APP/Contents/Info.plist"
[ -d "$REPO/Resources/Fonts" ] && cp -R "$REPO/Resources/Fonts" "$APP/Contents/Resources/Fonts"
for f in AppIcon.icns Logo.png Pushpin.png MenuIcon.png MenuIcon@2x.png WeChatGroup.png Coffee.png; do
    [ -f "$REPO/Resources/$f" ] && cp "$REPO/Resources/$f" "$APP/Contents/Resources/$f"
done

if [ -n "${PROVISION_PROFILE:-}" ]; then
    cp "$PROVISION_PROFILE" "$APP/Contents/embedded.provisionprofile"
    echo "→ embedded provisioning: $PROVISION_PROFILE"
else
    echo "→ PROVISION_PROFILE unset: app is not uploadable (local smoke only)"
fi

# Store apps: sandbox entitlements, secure timestamp, no hardened runtime
# (incompatible with App Sandbox signing for MAS).
if ! codesign --force --sign "$SIGN_ID" --timestamp --entitlements "$REPO/Resources/Pastory-mas.entitlements" "$APP" 2>/dev/null; then
    codesign --force --sign "$SIGN_ID" --entitlements "$REPO/Resources/Pastory-mas.entitlements" "$APP"
fi
echo "→ $APP (signed: $SIGN_ID)"

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
if [ "$SIGN_ID" != "-" ] && [ -n "${PROVISION_PROFILE:-}" ] && [ -n "$INSTALLER_ID" ]; then
    mkdir -p ../dist
    PKG="../dist/PasteJar-$VERSION-mas.pkg"
    rm -f "$PKG"
    productbuild --component "$APP" /Applications --sign "$INSTALLER_ID" "$PKG"
    echo "→ $PKG (installer signed: $INSTALLER_ID) — upload via Transporter / App Store Connect"
else
    echo "→ pkg skipped: needs Apple Distribution + PROVISION_PROFILE + installer identity (see header)"
fi
