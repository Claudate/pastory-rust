#!/bin/zsh
# Mac App Store 上传：把 build-mas.sh 出的 .pkg 送进 App Store Connect。
#
# 默认只做「生成 pkg + 打印将执行的命令」（dry run），加 --upload 才真传。
# 这样在凭据还没配好时也能先把签名链验证出来。
#
# 主路径现在是 fastlane deliver（不需要 Transporter.app，也不需要 .p8 之外的东西）；
# 这里保留 iTMSTransporter 作为兜底——deliver 在某些账号/网络下会 401/超时。
#
# 用法：
#   PROVISION_PROFILE=~/Library/.../PasteJar\ MAS.provisionprofile ./upload-mas.sh --upload
#   ./upload-mas.sh --via-transporter --upload     # 强制走 iTMSTransporter
#
# 凭据（二选一，环境变量，不落盘）：
#   A. ASC_KEY_ID=<Key ID>  ASC_ISSUER_ID=<Issuer ID>
#      私钥 ~/.appstoreconnect/private_keys/AuthKey_<Key ID>.p8
#   B. ASC_USERNAME=<apple id>  ASC_PASSWORD=<App 专用密码>
#
# 物料文案/截图/隐私标签 → store-assets/README.md
# 沙盒行为分叉与上架计划 → docs/MAS_PLAN.md
# 一键走完全程 → ./MAS-上架.command
set -uo pipefail
cd "$(dirname "$0")"

TRANSPORTER="/Applications/Transporter.app/Contents/itms/bin/iTMSTransporter"
DO_UPLOAD=0
VIA="auto"
PKG_ARG=""
while (( $# )); do
  case "$1" in
    --upload) DO_UPLOAD=1 ;;
    --via-transporter) VIA="transporter" ;;
    --via-deliver) VIA="deliver" ;;
    --pkg) PKG_ARG="${2:-}"; shift ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$1（--upload / --via-deliver / --via-transporter / --pkg PATH）" >&2; exit 2 ;;
  esac
  shift
done

die() { echo "✗ $*" >&2; exit 1; }
note() { echo "→ $*"; }

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' Resources/Info.plist)"
BUILD="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' Resources/Info.plist)"
BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' Resources/Info.plist)"
PKG="${PKG_ARG:-dist/PasteJar-$VERSION.pkg}"

# ── 1. 前置：缺什么先说清楚（不猜、不留到最后一句 401）────────
if [[ -z "${PROVISION_PROFILE:-}" ]]; then
  echo "✗ 未设置 PROVISION_PROFILE——没有它构建出来的包上传必被拒。" >&2
  echo >&2
  ./scripts/mas-doctor.sh --quiet >&2 || true
  echo >&2
  echo "  → 一条命令搞定：./MAS-上架.command --upload" >&2
  exit 1
fi
[[ -f "$PROVISION_PROFILE" ]] || die "PROVISION_PROFILE 指向的文件不存在：$PROVISION_PROFILE"

INSTALLER_CHECK="${INSTALLER_SIGN_ID:-}"
if [[ -z "$INSTALLER_CHECK" ]]; then
  # 不带 -p codesigning：installer 证书的 EKU 不是 code signing，会被该 policy 滤掉
  INSTALLER_CHECK="$(security find-identity -v 2>/dev/null | grep -o '"3rd Party Mac Developer Installer: [^"]*"' | head -1 | tr -d '"' || true)"
fi
[[ -n "$INSTALLER_CHECK" ]] || die "钥匙串里没有 3rd Party Mac Developer Installer 身份，productbuild 签不出 .pkg。
  → 跑 ./MAS-上架.command（会用 ASC 接口建一张并导入）"

note "目标：$BUNDLE_ID  $VERSION ($BUILD)"

# ── 2. 没有 pkg 就先构建 ────────────────────────────────────
if [[ ! -f "$PKG" ]]; then
  note "还没有 $PKG，先构建"
  PROVISION_PROFILE="$PROVISION_PROFILE" ./build-mas.sh || die "构建失败（见上面的输出）"
fi
[[ -f "$PKG" ]] || die "构建后仍没有 $PKG"

if ! pkgutil --check-signature "$PKG" 2>&1 | grep -q "Apple Root CA"; then
  pkgutil --check-signature "$PKG" 2>&1 | sed 's/^/    /'
  die "$PKG 的签名链不完整（安装包必须由 Installer 身份签）"
fi
note "pkg：$PKG ($(du -h "$PKG" | cut -f1))"

# ── 3. 上传 ────────────────────────────────────────────────
if (( ! DO_UPLOAD )); then
  echo
  echo "dry run：pkg 已就绪，未上传。真实上传："
  echo "  PROVISION_PROFILE='$PROVISION_PROFILE' $0 --upload"
  echo
  echo "  或一条命令走完（推荐）：./MAS-上架.command --upload"
  exit 0
fi

# deliver：官方接口，不需要 Transporter；凭据用 API Key 或 Apple ID 会话
try_deliver() {
  command -v fastlane >/dev/null 2>&1 || return 1
  local args=(deliver --pkg "$PKG" --platform osx --skip_metadata --skip_screenshots)
  if [[ -n "${ASC_KEY_ID:-}" && -n "${ASC_ISSUER_ID:-}" ]]; then
    local keypath="${ASC_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_$ASC_KEY_ID.p8}"
    [[ -f "$keypath" ]] || { echo "  ! 找不到 $keypath" >&2; return 1; }
    local json; json="$(mktemp -t pastejar-asc-key).json"
    {
      printf '{"key_id":"%s","issuer_id":"%s","key":"' "$ASC_KEY_ID" "$ASC_ISSUER_ID"
      awk 'NF {printf "%s\n", $0}' "$keypath"
      printf '","in_house":false}'
    } > "$json"
    chmod 600 "$json" 2>/dev/null || true
    args+=(--api_key_path "$json")
  fi
  note "fastlane deliver 上传中…"
  fastlane "${args[@]}"
  local rc=$?
  [[ -n "${json:-}" && -f "${json:-}" ]] && rm -f "$json" 2>/dev/null
  return $rc
}

try_transporter() {
  [[ -x "$TRANSPORTER" ]] || { echo "  ! 没有 Transporter.app（$TRANSPORTER）" >&2; return 1; }
  local auth=()
  if [[ -n "${ASC_KEY_ID:-}" && -n "${ASC_ISSUER_ID:-}" ]]; then
    auth=(-apiKey "$ASC_KEY_ID" -apiIssuer "$ASC_ISSUER_ID")
  elif [[ -n "${ASC_USERNAME:-}" && -n "${ASC_PASSWORD:-}" ]]; then
    # 密码经 @env 传入，不出现在进程命令行里
    auth=(-u "$ASC_USERNAME" -p "@env:ASC_PASSWORD")
  else
    echo "  ! 没有可用凭据（ASC_KEY_ID/ASC_ISSUER_ID 或 ASC_USERNAME/ASC_PASSWORD）" >&2
    return 1
  fi
  note "iTMSTransporter 上传中…（大包要几分钟）"
  local log; log="$(mktemp -t pastejar-transporter)"
  set +e
  "$TRANSPORTER" -m upload -assetFile "$PKG" -distribution AppStore -v informational "${auth[@]}" >"$log" 2>&1
  local rc=$?
  set -e
  # 接 grep 会把退出码换成 grep 的；这里先存日志再过滤
  grep -viE "DEBUG: |DBG-X|Setting System property|Configuring logging|SMART-CLIENT" "$log" || true
  rm -f "$log"
  return $rc
}

case "$VIA" in
  deliver) try_deliver || die "deliver 上传失败（看上面的错误）" ;;
  transporter) try_transporter || die "Transporter 上传失败（看上面的错误）" ;;
  auto)
    if try_deliver; then :; else
      echo "  ! deliver 没成功，改用 Transporter 兜底" >&2
      try_transporter || die "两条上传路径都失败了。
  常见原因：bundle id 在 ASC 里还没有 app 记录 / build 号 $BUILD 已用过 / 账号角色不足"
    fi
    ;;
esac

echo
echo "✓ 上传完成。构建进入「处理中」，几分钟后在 ASC → 你的 App → 构建 里选它。"
echo "  之后只剩三件人做的事：选构建、填审核联系人姓名电话、点「提交以供审核」。"
