#!/bin/zsh
# Mac App Store variant: cargo --features mas → bundle → provisioning →
# sandbox entitlements → (pkg when the store certs are present).
# Behaviour fork lives in rust/src (feature `mas`, docs/MAS_PLAN.md §2-3).
#
# Required for an UPLOADABLE product:
#   PROVISION_PROFILE=<path>   App Store provisioning profile for the bundle
#                              id (portal/Xcode/scripts/mas-auto.sh; embedded at
#                              Contents/embedded.provisionprofile)
#   keychain "Apple Distribution" identity + an installer identity for
#   productbuild (auto-detected, override with SIGN_ID / INSTALLER_SIGN_ID).
# Without them the script still emits build/PasteJar.app ad-hoc-signed with
# the sandbox entitlements — local smoke testing only, Transporter will reject
# it (that is expected, not a bug).
#
# 2026-10-30 起这份脚本做两件新事：
#   1) 嵌 profile 之前先校验它确实绑这个 bundle id、且不是 Development/Ad Hoc。
#      以前是「你说什么就嵌什么」，拿错 profile 要等到上传被拒才发现。
#   2) 默认出 universal（arm64 + x86_64）。ARCHS=host 只出本机架构，用于快速冒烟。
#
# NOTE the product name is deliberately the same as the direct build's: macOS
# files Accessibility / Screen Recording per *filename*, so a second name
# (`PasteJar-mas.app`) creates a second permission entry for the same bundle
# id and the user has to grant twice. Running this script therefore overwrites
# build/PasteJar.app with the sandboxed variant — re-run ./build-rs.sh to get
# the direct build back. Override with APP_FOLDER= to keep both side by side.
#
# Sandboxed binaries cannot be run from a shell (verified 2026-10-13): the
# `app-sandbox` entitlement makes libsystem_secinit reject a direct exec of
# Contents/MacOS/PasteJar, trapping in dyld before main — so `--selftest` is
# NOT available in this variant. Launch it through launchd instead:
#   open build/PasteJar.app
set -euo pipefail
cd "$(dirname "$0")/rust"

BUNDLE_ID="com.cici.snipclip"
TEAM_ID="HA7KN8S95K"

CONFIG="${CONFIG:-release}"

# profile 里的 application-identifier。security cms -D 在受限会话里会被拒，
# 退到 strings（profile 是 CMS 包的 plist，字符串仍可读）。
profile_app_id() {
    local p="$1" out=""
    out=$(security cms -D -i "$p" 2>/dev/null) || out=""
    if [ -n "$out" ]; then
        printf '%s' "$out" | /usr/bin/plutil -extract Entitlements.application-identifier raw -o - - 2>/dev/null && return 0
    fi
    strings -a "$p" 2>/dev/null | grep -oE '[A-Z0-9]{10}\.com\.[A-Za-z0-9._-]+' | head -1
}

# 嵌之前先确认这份 profile 就是给这个 app 的：拿错 profile 时不构建，
# 而不是编两分钟再在上传时被 Apple 拒。
check_profile() {
    local p="$1"
    if [ ! -f "$p" ]; then
        echo "✗ PROVISION_PROFILE 指向的文件不存在：$p" >&2
        return 1
    fi
    local appid
    appid="$(profile_app_id "$p")"
    if [ -z "$appid" ]; then
        echo "✗ 读不出这份 profile 的 application-identifier：$p" >&2
        echo "  → 确认它是 Mac App Store 的 .provisionprofile；不行就跑 ./scripts/mas-auto.sh 重新生成" >&2
        return 1
    fi
    if [ "$appid" != "$TEAM_ID.$BUNDLE_ID" ]; then
        echo "✗ 这份 profile 绑的是「$appid」，不是 $TEAM_ID.$BUNDLE_ID" >&2
        echo "  → 你下的是别的 app / 别的团队的 profile；跑 ./scripts/mas-auto.sh 生成正确的" >&2
        return 1
    fi
    if strings -a "$p" 2>/dev/null | grep -q "ProvisionedDevices"; then
        echo "✗ 这份 profile 带设备列表（Development / Ad Hoc），上传会被拒" >&2
        echo "  → 需要 Mac App Store 类型；跑 ./scripts/mas-auto.sh 重新生成" >&2
        return 1
    fi
    echo "  ✓ profile 绑定 $appid"
    return 0
}


# universal 是默认：只有单一架构的包在部分校验/下载路径上会被当成残缺；
# ARCHS=host 只出本机架构（冒烟更快），ARCHS="arm64 x86_64" 也可显式给。
ARCHS="${ARCHS-universal}"
case "$ARCHS" in
  universal) ARCHS="arm64 x86_64" ;;
  host) ARCHS="" ;;
esac

# 拿错 profile 时不构建：编两分钟才报错是最费时间的一种失败。
if [ -n "${PROVISION_PROFILE:-}" ]; then
    echo "→ 校验 provisioning profile"
    check_profile "$PROVISION_PROFILE"
fi

if [ -z "${SIGN_ID:-}" ]; then
    IDS="$(security find-identity -v -p codesigning 2>/dev/null || true)"
    DIST="$(echo "$IDS" | grep -o '"Apple Distribution: [^"]*"' | head -1 | tr -d '"' || true)"
    SIGN_ID="${DIST:--}"
fi
INSTALLER_ID="${INSTALLER_SIGN_ID:-}"
if [ -z "$INSTALLER_ID" ] && [ "$SIGN_ID" != "-" ]; then
    # NOT -p codesigning: the installer cert's EKU is not code signing, so a
    # codesigning-policy query filters it out and this line silently skips
    # productbuild even when the identity is present. Verified 2026-10-22:
    # `find-identity -v` lists 2 identities, `-v -p codesigning` lists 1.
    INSTALLER_ID="$( (security find-identity -v 2>/dev/null || true) \
        | grep -o '"3rd Party Mac Developer Installer: [^"]*"' | head -1 | tr -d '"' || true)"
fi

REPO="$(pwd)/.."

# PASTEJAR_FEED_HOST is baked into the client exactly like build-rs.sh does it
# (cargo inherits the shell env). A store build should point at the feed so
# the daily check stays mainland-friendly.
if [ -n "${ARCHS:-}" ]; then
    SLICES=()
    for a in $=ARCHS; do
        # Rust 的 aarch64 目标名不是 arm64：写 arm64-apple-darwin 会直接
        # 「could not find specification for target」——universal 这条路以前就是坏的。
        case "$a" in
            arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
            x86_64) TARGET="x86_64-apple-darwin" ;;
            *) TARGET="$a-apple-darwin" ;;
        esac
        cargo build -r --features mas --target "$TARGET"
        SLICES+=("target/$TARGET/$CONFIG/pastory")
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
APP="../build/${APP_FOLDER:-PasteJar}.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/PasteJar"
cp "$REPO/Resources/Info.plist" "$APP/Contents/Info.plist"
[ -d "$REPO/Resources/Fonts" ] && cp -R "$REPO/Resources/Fonts" "$APP/Contents/Resources/Fonts"
for f in AppIcon.icns Logo.png Pushpin.png MenuIcon.png MenuIcon@2x.png WeChatGroup.png Coffee.png; do
    [ -f "$REPO/Resources/$f" ] && cp "$REPO/Resources/$f" "$APP/Contents/Resources/$f"
done

if [ -n "${PROVISION_PROFILE:-}" ]; then
    check_profile "$PROVISION_PROFILE"
    cp "$PROVISION_PROFILE" "$APP/Contents/embedded.provisionprofile"
    echo "→ embedded provisioning: $PROVISION_PROFILE"
else
    echo "→ PROVISION_PROFILE unset: app is not uploadable (local smoke only)"
    echo "  → 要出可上传的包：./scripts/mas-auto.sh"
fi

# Store apps: sandbox entitlements, secure timestamp, no hardened runtime
# (incompatible with App Sandbox signing for MAS).
if ! codesign --force --sign "$SIGN_ID" --timestamp --entitlements "$REPO/Resources/PasteJar.entitlements" "$APP" 2>/dev/null; then
    codesign --force --sign "$SIGN_ID" --entitlements "$REPO/Resources/PasteJar.entitlements" "$APP"
fi
echo "→ $APP (signed: $SIGN_ID)"

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
if [ "$SIGN_ID" != "-" ] && [ -n "${PROVISION_PROFILE:-}" ] && [ -n "$INSTALLER_ID" ]; then
    # PKG_DIR 可覆盖输出目录：验证脚本用它把测试产物引到别处，不污染 dist/
    PKG_DIR="${PKG_DIR:-../dist}"
    mkdir -p "$PKG_DIR"
    PKG="$PKG_DIR/PasteJar-$VERSION.pkg"
    rm -f "$PKG"
    productbuild --component "$APP" /Applications --sign "$INSTALLER_ID" "$PKG" || {
        echo "✗ productbuild 失败（上面是它的原话）" >&2
        exit 1
    }
    # 出包后立刻自检。放在 build-mas 里，是为了「直接 PROVISION_PROFILE=… ./build-mas.sh」
    # 这条路径（不走 mas-auto）也被同样把关：签名链断了、entitlements 少了、profile 嵌错了，
    # 现在就在本地报出来，而不是等 Apple 处理二十分钟后拒。
    if /usr/sbin/pkgutil --check-signature "$PKG" 2>&1 | /usr/bin/grep -q "Apple Root CA"; then
        echo "  ✓ pkg 签名链完整（链到 Apple Root CA）"
    else
        echo "✗ pkg 签名链不完整：Installer 证书或钥匙串有问题" >&2
        echo "  → 跑 ./scripts/mas-doctor.sh 看证书状态" >&2
        exit 1
    fi
    ENT_KEYS=$(/usr/bin/codesign -d --entitlements :- "$APP" 2>/dev/null | /usr/bin/grep -o 'com.apple.security[a-z.-]*' | sort -u | tr '\n' ' ')
    for k in com.apple.security.app-sandbox com.apple.security.network.client com.apple.security.accessibility; do
        case " $ENT_KEYS " in
            *" $k "*) : ;;
            *) echo "✗ app 缺 entitlements：$k（现有：$ENT_KEYS）" >&2; exit 1 ;;
        esac
    done
    echo "  ✓ 三份 sandbox entitlements 都在"
    EMB_ID=$(/usr/bin/strings -a "$APP/Contents/embedded.provisionprofile" 2>/dev/null | /usr/bin/grep -oE '[A-Z0-9]{10}\.com\.[A-Za-z0-9._-]+' | head -1)
    if [ "$EMB_ID" != "$TEAM_ID.$BUNDLE_ID" ]; then
        echo "✗ 嵌入的 profile 是「$EMB_ID」，不是 $TEAM_ID.$BUNDLE_ID" >&2
        exit 1
    fi
    echo "  ✓ 嵌入的 profile 正确（$EMB_ID）"
    echo "→ $PKG (installer signed: $INSTALLER_ID) — upload via Transporter / App Store Connect"
    echo "  → 上传：./MAS-上架.command --upload"
else
    echo "→ pkg skipped: needs Apple Distribution + PROVISION_PROFILE + installer identity"
    if [ -z "$INSTALLER_ID" ]; then
        echo "  → 缺 Installer 证书：跑 ./scripts/mas-auto.sh（会用 ASC 接口建一张并导入钥匙串）"
    fi
fi
