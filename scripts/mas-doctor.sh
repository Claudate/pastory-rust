#!/bin/zsh
# PasteJar 上架环境的只读诊断：只说事实，不改任何东西。
#
# 为什么单独一个脚本：环境缺项在「构建到一半」「上传被拒」时暴露最费时间，
# 而在干净 Mac 上缺的可能是 CLT、证书、fastlane 三种完全不同的东西。
# 这里把它们一次列清楚，并给出下一条该跑的命令；mas-bootstrap.sh / mas-auto.sh
# 都先调它，避免三处各写一套判断必然漂移。
#
# 用法：
#   ./scripts/mas-doctor.sh            人读：可以做 / 卡在哪 / 下一步
#   ./scripts/mas-doctor.sh --probe    额外打印探测到的原始事实（版本、允许值、身份）
#   ./scripts/mas-doctor.sh --json     给脚本读
#   ./scripts/mas-doctor.sh --quiet    只打「卡在哪」与「下一步」
#
# 退出码：0 全部就绪；2 缺 profile；3 缺环境工具；4 缺签名证书
set -u
setopt NO_NOMATCH 2>/dev/null || true

BUNDLE_ID="com.cici.snipclip"
TEAM_ID="HA7KN8S95K"
REQUIRED_FASTLANE_MIN="2.220.0"

MODE="human"
JSON=0
case "${1:-}" in
  --probe) MODE="probe" ;;
  --json) JSON=1 ;;
  --quiet) MODE="quiet" ;;
  "") ;;
  -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) echo "未知参数：$1（只接受 --probe / --json / --quiet）" >&2; exit 2 ;;
esac

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INFO="$ROOT/Resources/Info.plist"
ENT="$ROOT/Resources/PasteJar.entitlements"

# 收集三组事实：ok / missing / next。最后按缺项决定退出码。
typeset -a OKS MISSINGS NEXTS
ok() { OKS+=("$1") }
miss() { MISSINGS+=("$1") }
next() { NEXTS+=("$1") }

have() { command -v "$1" >/dev/null 2>&1 }

# ── 工程本身 ────────────────────────────────────────────────
VERSION=""; BUILD=""
if [[ -f "$INFO" ]]; then
  VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$INFO" 2>/dev/null || true)
  BUILD=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$INFO" 2>/dev/null || true)
  BID=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$INFO" 2>/dev/null || true)
  if [[ "$BID" == "$BUNDLE_ID" ]]; then
    ok "工程：$BUNDLE_ID $VERSION ($BUILD)"
  else
    miss "Info.plist 的 bundle id 是「$BID」，不是 $BUNDLE_ID"
    next "确认你在 pastory 仓库根目录（当前 $ROOT）"
  fi
else
  miss "找不到 Resources/Info.plist（当前 $ROOT 不是 pastory 仓库根？）"
fi
if [[ -f "$ENT" ]]; then
  ok "entitlements：$(/usr/bin/grep -c 'com.apple.security' "$ENT" 2>/dev/null) 项"
else
  miss "找不到 Resources/PasteJar.entitlements"
fi

# ── 命令行工具 ──────────────────────────────────────────────
# productbuild 没有 --version（会把参数当输入文件、静默 exit 1），
# 所以「可用」按两条判：xcrun 能解析到它，且它自己会打印 usage。
CLT_PATH=$(/usr/bin/xcode-select -p 2>/dev/null || true)
if [[ -n "$CLT_PATH" ]] && /usr/bin/xcrun -f productbuild >/dev/null 2>&1 \
   && /usr/bin/productbuild 2>&1 | /usr/bin/grep -q "Usage: productbuild"; then
  ok "Command Line Tools：$CLT_PATH"
else
  miss "没有可用的 Command Line Tools（productbuild 不可用，.pkg 签不出来）"
  next "跑 ./scripts/mas-bootstrap.sh，或手动 xcode-select --install 后在弹窗点「安装」"
fi

BREW=$(command -v brew 2>/dev/null || true)
if [[ -n "$BREW" ]]; then
  ok "Homebrew：$BREW"
else
  miss "没有 Homebrew（干净 Mac 常见）"
  next "跑 ./scripts/mas-bootstrap.sh（用官方脚本安装，需要一次管理员密码）"
fi

CARGO=$(command -v cargo 2>/dev/null || true)
if [[ -n "$CARGO" ]]; then
  CARGO_VER=$("$CARGO" --version 2>/dev/null | head -1)
  # cargo 在 PATH 里却跑不起来（新装 rustup 没配 toolchain、或 HOME 不是原来的）——
  # 这时打一个空的 ✓ 会让人以为没事，必须按「缺」报。
  if [[ -z "$CARGO_VER" ]]; then
    miss "Rust 工具链不可用（cargo 在，但执行失败）"
    next "跑 ./scripts/mas-bootstrap.sh（会装 stable 工具链并加两个架构目标）"
  else
    ok "Rust：$CARGO_VER"
  fi
  if [[ -n "$CARGO_VER" ]] && have rustup; then
    TARGETS=$(rustup target list --installed 2>/dev/null | tr '\n' ' ')
    case "$TARGETS" in
      *aarch64-apple-darwin*x86_64-apple-darwin*) ok "Rust 目标：arm64 + x86_64 都在（可出 universal）" ;;
      *) miss "缺少 universal 需要的一个架构目标（现有：$TARGETS）"
         next "rustup target add aarch64-apple-darwin x86_64-apple-darwin" ;;
    esac
  fi
else
  miss "没有 Rust 工具链（cargo）"
  next "跑 ./scripts/mas-bootstrap.sh 装 rustup"
fi

FASTLANE=$(command -v fastlane 2>/dev/null || true)
FL_VER=""
if [[ -n "$FASTLANE" ]]; then
  # 新版 `fastlane --version` 先打「installation at path」再打版本号，
  # 取第一行会得到一句话而不是版本号，版本比较随之失效。
  # `fastlane --version` 会打好几行（安装路径 + 依赖版本），只按数字正则取 last
  # 会拿到依赖的版本号（实测取到 0.9.44）。先认「以 fastlane 开头」那一行。
  FL_VER=$("$FASTLANE" --version 2>/dev/null | /usr/bin/sed -n 's/^fastlane \([0-9][0-9.]*\).*/\1/p' | head -1)
  [[ -z "$FL_VER" ]] && FL_VER=$("$FASTLANE" --version 2>/dev/null | /usr/bin/grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
  ok "fastlane：${FL_VER:-（版本未识别）}（$FASTLANE）"
  if [[ -n "$FL_VER" ]] && [[ "$(printf '%s\n%s\n' "$REQUIRED_FASTLANE_MIN" "$FL_VER" | sort -V | head -1)" != "$REQUIRED_FASTLANE_MIN" ]]; then
    miss "fastlane 版本 $FL_VER 低于 $REQUIRED_FASTLANE_MIN"
    next "brew upgrade fastlane"
  fi
else
  miss "没有 fastlane（建 App ID / profile / 上传都要它）"
  next "跑 ./scripts/mas-bootstrap.sh，或 brew install fastlane"
fi

GIT=$(command -v git 2>/dev/null || true)
if [[ -n "$GIT" ]]; then
  ok "git：$("$GIT" --version 2>/dev/null)"
else
  miss "没有 git"
  next "xcode-select --install 会一并装 git"
fi

# ── 签名身份（不带 -p codesigning：Installer 证书的 EKU 不是 code signing）──
IDS_RAW=$(/usr/bin/security find-identity -v 2>/dev/null || true)
# find-identity 末尾有一行「N valid identities found」，没有引号，
# 下面按引号取 CN 的解析会自然跳过它。
HAS_DIST=0; HAS_INSTALLER=0; EXPIRIES=""
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  cn=$(printf '%s' "$line" | /usr/bin/sed -n 's/.*"\(.*\)"/\1/p')
  [[ -z "$cn" ]] && continue
  end=$(/usr/bin/security find-certificate -c "$cn" -p 2>/dev/null | /usr/bin/openssl x509 -noout -enddate 2>/dev/null | sed 's/^notAfter=//')
  EXPIRIES+="$cn → ${end:-未知}"$'\n'
  case "$cn" in
    "Apple Distribution:"*) HAS_DIST=1 ;;
    "3rd Party Mac Developer Installer:"*) HAS_INSTALLER=1 ;;
  esac
done <<< "$IDS_RAW"

if [[ "$HAS_DIST" == "1" ]]; then
  ok "证书：Apple Distribution 在（可签 app）"
else
  miss "钥匙串里没有 Apple Distribution 证书（干净 Mac 必然如此）"
  next "跑 ./scripts/mas-auto.sh（用 ASC 接口 + 本地 CSR 建一张并导入钥匙串）"
fi
if [[ "$HAS_INSTALLER" == "1" ]]; then
  ok "证书：3rd Party Mac Developer Installer 在（可签 .pkg）"
else
  miss "钥匙串里没有 3rd Party Mac Developer Installer 证书"
  next "跑 ./scripts/mas-auto.sh（建 MAC_INSTALLER_DISTRIBUTION 并导入）"
fi

# ── provisioning profile ────────────────────────────────────
PROFILE_DIR="$HOME/Library/MobileDevice/Provisioning Profiles"
PROFILE_OK=0; PROFILE_LIST=""

# 解 profile 里的 application-identifier。
# security cms -D 在受限会话里会被拒（本机 IDE 会话就是这样），
# 所以退到 strings：profile 是 CMS 包装的 plist，字符串仍可读。
profile_app_id() {
  local p="$1" out=""
  out=$(/usr/bin/security cms -D -i "$p" 2>/dev/null) || out=""
  if [[ -n "$out" ]]; then
    printf '%s' "$out" | /usr/bin/plutil -extract Entitlements.application-identifier raw -o - - 2>/dev/null && return 0
  fi
  /usr/bin/strings -a "$p" 2>/dev/null | /usr/bin/grep -oE '[A-Z0-9]{10}\.com\.[A-Za-z0-9._-]+' | head -1
}

if [[ -d "$PROFILE_DIR" ]]; then
  for p in "$PROFILE_DIR"/*(N); do
    [[ -f "$p" ]] || continue
    appid=$(profile_app_id "$p" | head -1)
    [[ -z "$appid" ]] && appid="（读不出）"
    PROFILE_LIST+="$(basename "$p") → $appid"$'\n'
    [[ "$appid" == "$TEAM_ID.$BUNDLE_ID" ]] && PROFILE_OK=1
  done
else
  PROFILE_LIST="（目录不存在：$PROFILE_DIR）"$'\n'
fi

if [[ "$PROFILE_OK" == "1" ]]; then
  ok "profile：找到绑定 $TEAM_ID.$BUNDLE_ID 的（可上传）"
else
  miss "没有绑定 $TEAM_ID.$BUNDLE_ID 的 profile"
  next "跑 ./scripts/mas-auto.sh（建 MAC_APP_STORE profile 并装到 $PROFILE_DIR）"
fi

# ── 上传凭据 ────────────────────────────────────────────────
P8_LIST=$(/bin/ls -1 "$HOME/.appstoreconnect/private_keys"/AuthKey_*.p8 2>/dev/null | head -3 || true)
if [[ -n "${ASC_KEY_ID:-}" && -n "${ASC_ISSUER_ID:-}" ]]; then
  ok "上传凭据：ASC_KEY_ID=${ASC_KEY_ID:0:4}… / ASC_ISSUER_ID 已设置"
elif [[ -n "$P8_LIST" ]]; then
  ok "上传凭据：找到 $(echo "$P8_LIST" | wc -l | tr -d ' ') 个 .p8（另需 ASC_KEY_ID / ASC_ISSUER_ID）"
else
  miss "没有 ASC API Key（也可改用 Apple ID + 2FA）"
  next "ASC → 用户和访问 → 集成 → App Store Connect API → 生成密钥，.p8 放进 ~/.appstoreconnect/private_keys/"
fi

SESSION_DIR="$HOME/.fastlane/spaceship"
if [[ -d "$SESSION_DIR" ]] && [[ -n "$(/bin/ls -A "$SESSION_DIR" 2>/dev/null)" ]]; then
  ok "Apple ID 会话：$SESSION_DIR 里有缓存（可能已过期）"
else
  miss "没有 Apple ID 登录会话（路径 A 需要它；路径 B 不需要）"
fi

# ── Transporter（只在兜底路径需要）────────────────────────────
if [[ -x "/Applications/Transporter.app/Contents/itms/bin/iTMSTransporter" ]]; then
  ok "Transporter：已安装（兜底上传路径可用）"
else
  miss "没有 Transporter.app（只有 deliver 失败时才需要）"
fi

# ── 产物 ────────────────────────────────────────────────────
PKGS=$(/bin/ls -1 "$ROOT/dist"/*.pkg 2>/dev/null | head -3 || true)
if [[ -n "$PKGS" ]]; then
  ok "产物：$(echo "$PKGS" | tr '\n' ' ')"
else
  ok "产物：dist/ 里还没有 pkg（正常，构建后才有）"
fi

# ── 汇总 ────────────────────────────────────────────────────
MISSING_COUNT=${#MISSINGS[@]}

if [[ "$JSON" == "1" ]]; then
  # 自己拼 JSON：只有一个消费方（mas-auto.sh），不引入 jq 依赖。
  print_list() { printf '"%s",' "$@"; }
  printf '{"bundleId":"%s","version":"%s","build":"%s","fastlane":"%s","ready":%s,"missing":[%s],"next":[%s]}\n' \
    "$BUNDLE_ID" "$VERSION" "$BUILD" "$FL_VER" \
    "$([[ $MISSING_COUNT -eq 0 ]] && echo true || echo false)" \
    "$(print_list "${MISSINGS[@]}" | sed 's/,$//')" \
    "$(print_list "${NEXTS[@]}" | sed 's/,$//')"
  exit 0
fi

if [[ "$MODE" != "quiet" ]]; then
  echo "PasteJar 上架环境（$BUNDLE_ID $VERSION ($BUILD) · 团队 $TEAM_ID）"
  echo
  for o in "${OKS[@]}"; do echo "  ✓ $o"; done
fi

if [[ $MISSING_COUNT -gt 0 ]]; then
  echo
  echo "卡在这些地方："
  for m in "${MISSINGS[@]}"; do echo "  ✗ $m"; done
  echo
  echo "下一步（按顺序）："
  i=0
  for n in "${NEXTS[@]}"; do i=$((i+1)); echo "  $i) $n"; done
fi

if [[ "$MODE" == "probe" ]]; then
  echo
  echo "—— 探测细节 ——"
  echo "fastlane: ${FL_VER:-（没有）}"
  if [[ -n "$FASTLANE" ]]; then
    echo "sigh 的 platform 说明：$("$FASTLANE" sigh --help 2>&1 | /usr/bin/grep -m1 -o 'i.e\. ios[^)]*' || echo '（读不到）')"
  fi
  echo "签名身份："
  printf '%s' "$IDS_RAW" | sed 's/^/  /'
  echo "证书到期："
  printf '%s' "$EXPIRIES" | sed 's/^/  /'
  echo "profile："
  printf '%s' "$PROFILE_LIST" | sed 's/^/  /'
  echo "Rust 目标：$(rustup target list --installed 2>/dev/null | tr '\n' ' ')"
fi

# 0 = 全部就绪；2 = 缺 profile；3 = 缺环境；4 = 缺证书
if [[ "$HAS_DIST" != "1" || "$HAS_INSTALLER" != "1" ]]; then exit 4; fi
if [[ -z "$CLT_PATH" || -z "$FASTLANE" || -z "$CARGO" ]]; then exit 3; fi
if [[ "$PROFILE_OK" != "1" ]]; then exit 2; fi
if [[ $MISSING_COUNT -eq 0 ]]; then echo; echo "READY：可以出可上传的 pkg"; fi
exit 0
