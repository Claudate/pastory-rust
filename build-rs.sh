#!/bin/zsh
# Rust port build: cargo → universal (ARCHS="arm64 x86_64") → bundle.
# Mirrors build.sh, minus SwiftPM: cargo is the build database, so no
# stale-per-triple dance is needed.
set -euo pipefail
cd "$(dirname "$0")/rust"

CONFIG="${CONFIG:-release}"
# PASTEJAR_FEED_HOST=<feed 域名> 编译期烧进两级取源（docs/MAS_PLAN.md §1-2）；
# cargo 继承 shell 环境，脚本不主动传——不设置则只走 GitHub（旧行为）。
# A keychain identity is used automatically (same signature as the shipped
# app, so TCC permissions carry over between a local build and a release).
#
# Only Developer ID used to be recognised, so a machine holding just the MAS
# "Apple Distribution" certificate fell back to ad-hoc — and an ad-hoc code
# requirement is the bare cdhash. Every rebuild then produced a new cdhash:
# Screen Recording / Accessibility stopped matching while System Settings
# still listed the app with its switch on (the record belongs to the older
# binary). Order: Developer ID (notarizable) → Apple Development → Apple
# Distribution (MAS). Nothing found → ad-hoc.
if [ -z "${SIGN_ID:-}" ]; then
    IDS="$(security find-identity -v -p codesigning 2>/dev/null || true)"
    SIGN_ID="-"
    for PREFIX in "Developer ID Application: " "Apple Development: " "Apple Distribution: "; do
        FOUND="$(echo "$IDS" | grep -o "\"${PREFIX}[^\"]*\"" | head -1 | tr -d '"' || true)"
        if [ -n "$FOUND" ]; then
            SIGN_ID="$FOUND"
            break
        fi
    done
fi

REPO="$(pwd)/.."

# ARCHS="arm64 x86_64" builds each slice with --target and lipo's them;
# default is this machine only. `$=ARCHS` word-splits: zsh does not split
# unquoted expansions (this runs under /bin/zsh, CI included).
if [ -n "${ARCHS:-}" ]; then
    SLICES=()
    for a in $=ARCHS; do
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

# Bundle folder / display name, and the executable's file name. Rust is the
# only live port now (Swift 版已冻结归档), so the app ships as
# "PasteJar.app" / executable "PasteJar" / display name "PasteJar"
# (bundle id — and therefore TCC — is unchanged).
APP="../build/${APP_FOLDER:-PasteJar}.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/PasteJar"
cp "$REPO/Resources/Info.plist" "$APP/Contents/Info.plist"
if [ -n "${APP_TITLE:-}" ]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName ${APP_TITLE}" "$APP/Contents/Info.plist"
fi
[ -d "$REPO/Resources/Fonts" ] && cp -R "$REPO/Resources/Fonts" "$APP/Contents/Resources/Fonts"
for f in AppIcon.icns Logo.png Pushpin.png MenuIcon.png MenuIcon@2x.png WeChatGroup.png WeChatPersonal.png Coffee.png ClothesPin.png; do
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
