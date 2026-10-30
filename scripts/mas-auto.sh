#!/bin/zsh
# 一条命令走完：证书 → App ID → ASC 记录 → profile → 构建 → pkg，可选上传。
#
# 为什么要有它：上架真正费时间的不是命令本身，而是「这一步该做什么、做完怎么知道对」。
# 判断逻辑在 mas-doctor.sh 与 mas-signing.rb 里各有一份，这里只负责按顺序串起来、
# 每一步先说清在做什么，失败时给出下一条该跑的命令（不是英文堆栈）。
#
# 用法：
#   ./MAS-上架.command                生成 dist/PasteJar-<版本>.pkg
#   ./MAS-上架.command --upload       再送到 App Store Connect
#   ./MAS-上架.command --dry-run      只打印将执行的命令（不联网、不构建）
#   ./MAS-上架.command --apple-id EMAIL  用 Apple ID（2FA 会弹一次）
#   ./MAS-上架.command --api-key      强制走 ASC API Key
#   ./MAS-上架.command --yes          需要补环境时不再询问
#
# 退出码：0 完成；1 停下（会说明原因与下一步）
set -u
setopt NO_NOMATCH 2>/dev/null || true

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUNDLE_ID="com.cici.snipclip"
TEAM_ID="HA7KN8S95K"
ASC_APP_NAME="PasteJar"
PROFILE_NAME="PasteJar MAS"
PROFILE_DIR="$HOME/Library/MobileDevice/Provisioning Profiles"
PROFILE_PATH="$PROFILE_DIR/$PROFILE_NAME.provisionprofile"

UPLOAD=0; DRY=0; YES=0; FORCE_API=0; APPLE_ID=""; PROFILE_OVERRIDE=""
while (( $# )); do
  case "$1" in
    --upload) UPLOAD=1 ;;
    --dry-run) DRY=1 ;;
    --yes|-y) YES=1 ;;
    --api-key) FORCE_API=1 ;;
    --apple-id) APPLE_ID="${2:-}"; shift ;;
    --profile) PROFILE_OVERRIDE="${2:-}"; shift ;;  # 手动下载了 profile 时直接指定
    -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$1（--upload / --dry-run / --apple-id EMAIL / --api-key / --yes）" >&2; exit 2 ;;
  esac
  shift
done

# --profile 要在解析完参数之后再套用：写在解析之前会永远读到空值。
[[ -n "${PROFILE_OVERRIDE:-}" ]] && PROFILE_PATH="$PROFILE_OVERRIDE"

say() { printf '\n→ %s\n' "$*" }
ok()  { printf '  ✓ %s\n' "$*" }
warn(){ printf '  ! %s\n' "$*" }
die() { printf '\n✗ %s\n' "$1"; [[ -n "${2:-}" ]] && printf '  → %s\n' "$2"; exit 1; }
run() {  # dry-run 时只打印，不执行
  if (( DRY )); then printf '  $ %s\n' "${(j: :)@}"; return 0; fi
  "$@"
}
ask() {
  (( YES )) && return 0
  [[ -t 0 ]] || return 1
  local a; read -r "a?$1 [y/N] "
  [[ "$a" == [yY]* ]]
}

# ── 0. 环境 ─────────────────────────────────────────────────
say "检查环境"
NEED_BOOT=0
command -v fastlane >/dev/null 2>&1 || NEED_BOOT=1
command -v cargo >/dev/null 2>&1 || NEED_BOOT=1
if ! ( /usr/bin/xcrun -f productbuild >/dev/null 2>&1 && /usr/bin/productbuild 2>&1 | /usr/bin/grep -q "Usage: productbuild" ); then
  NEED_BOOT=1
fi
if (( NEED_BOOT )); then
  "$ROOT/scripts/mas-doctor.sh" --quiet || true
  if ask "环境不全，现在跑 scripts/mas-bootstrap.sh 补齐？"; then
    run "$ROOT/scripts/mas-bootstrap.sh" || die "环境补齐失败（见上面的输出）" "修好后重跑本命令即可，已装好的不会重复装"
  else
    die "环境不全" "先跑 ./scripts/mas-bootstrap.sh"
  fi
else
  ok "工具链齐（fastlane / cargo / productbuild）"
fi

# ── 1. 修 Ruby 环境：fastlane 的 gems 不在默认 GEM_PATH 里 ──
resolve_libexec() {
  local fl; fl="$(command -v fastlane 2>/dev/null)" || return 1
  local real="${fl:A}"            # zsh 会解析符号链接：.../libexec/bin/fastlane
  print -r -- "${real:h:h}"
}
RUBY_BIN=""; FASTLANE_LIBEXEC=""
setup_ruby() {
  FASTLANE_LIBEXEC="$(resolve_libexec)" || die "找不到 fastlane" "brew install fastlane"
  local cand
  for cand in "$(command -v ruby 2>/dev/null)" /opt/homebrew/opt/ruby/bin/ruby /usr/bin/ruby; do
    [[ -n "$cand" && -x "$cand" ]] || continue
    if GEM_HOME="$FASTLANE_LIBEXEC" GEM_PATH="$FASTLANE_LIBEXEC" "$cand" -e 'require "spaceship"' >/dev/null 2>&1; then
      RUBY_BIN="$cand"; return 0
    fi
  done
  die "加载不到 fastlane 的 Ruby 库（spaceship）" "brew reinstall fastlane 后重试"
}
signing() {
  GEM_HOME="$FASTLANE_LIBEXEC" GEM_PATH="$FASTLANE_LIBEXEC" \
    BUNDLE_ID="$BUNDLE_ID" ASC_APP_NAME="$ASC_APP_NAME" "$RUBY_BIN" "$ROOT/scripts/mas-signing.rb" "$@"
}

# 从 signing 的一行 JSON 里取值。布尔要单独判：JSON 里 false 不带引号，
# 只用字符串正则会把 {"ok":false} 读成空 —— 失败就被当成成功了。
json_str() {  # $1=json $2=key
  printf '%s' "$1" | /usr/bin/sed -n "s/.*\"$2\":\"\([^\"]*\)\".*/\1/p" | head -1
}
json_bool() {  # 输出 true / false / 空
  case "$1" in
    *"\"$2\":true"*)  printf 'true' ;;
    *"\"$2\":false"*) printf 'false' ;;
    *) printf '' ;;
  esac
}
# 失败时先把 signing 报的 error/hint 打出来，再 die ——
# 否则 $(...) 把 stdout 吞掉，日志上只剩一句「看上面」，而上面什么都没有。
fail_from() {  # $1=json $2=标题
  local e h
  e=$(json_str "$1" error); h=$(json_str "$1" hint)
  [[ -n "$e" ]] && warn "$e"
  [[ -n "$h" ]] && warn "$h"
  die "$2" "修好上面那条再重跑；想先看机器状态就 ./scripts/mas-doctor.sh --probe"
}

# ── 2. 判断要不要碰网络 ─────────────────────────────────────
keychain_has() { /usr/bin/security find-identity -v 2>/dev/null | /usr/bin/grep -Fq "$1"; }
profile_ok() {
  [[ -f "$PROFILE_PATH" ]] || return 1
  local appid
  appid=$(/usr/bin/strings -a "$PROFILE_PATH" 2>/dev/null | /usr/bin/grep -oE '[A-Z0-9]{10}\.com\.[A-Za-z0-9._-]+' | head -1)
  [[ "$appid" == "$TEAM_ID.$BUNDLE_ID" ]]
}

HAS_DIST=0; HAS_INSTALLER=0
keychain_has "Apple Distribution:" && HAS_DIST=1
keychain_has "3rd Party Mac Developer Installer:" && HAS_INSTALLER=1
PROFILE_OK=0; profile_ok && PROFILE_OK=1

if (( DRY )); then
  # dry-run 不联网：只把「真跑时会做什么」按顺序列出来，
  # 这样在没有账号、没有证书的机器上也能看清整条路径。
  say "dry-run：将要执行的步骤（不联网、不构建）"
  (( HAS_DIST ))      && ok "证书：Apple Distribution 已在"      || echo "  · 申请 Apple Distribution 证书（本地生成私钥 + CSR）并导入登录钥匙串"
  (( HAS_INSTALLER )) && ok "证书：Installer 已在"                 || echo "  · 申请 MAC_INSTALLER_DISTRIBUTION 证书并导入登录钥匙串"
  echo "  · 确认 App ID：$BUNDLE_ID"
  echo "  · 确认 ASC app 记录（无此接口时打印表单并打开网页）"
  (( PROFILE_OK ))    && ok "profile：已在本机"                    || echo "  · 建 MAC_APP_STORE profile → $PROFILE_PATH"
  echo "  · PROVISION_PROFILE=$PROFILE_PATH ./build-mas.sh"
  echo "  · 自检：pkgutil --check-signature / lipo -info / 嵌入 profile 的 bundle id"
  (( UPLOAD )) && echo "  · fastlane deliver --pkg dist/PasteJar-<版本>.pkg --platform osx"
  exit 0
fi

if (( HAS_DIST && HAS_INSTALLER && PROFILE_OK )); then
  say "证书与 profile 都已在，跳过所有联网步骤"
else
  # 需要登录：选凭据
  MODE=""
  if (( FORCE_API )); then MODE=api
  elif [[ -n "$APPLE_ID" ]]; then MODE=apple
  elif [[ -n "${ASC_KEY_ID:-}" && -n "${ASC_ISSUER_ID:-}" ]]; then MODE=api
  elif [[ -n "${FASTLANE_USER:-}" ]]; then MODE=apple
  else
    # 有 .p8 但没设 Key ID：给一次输入机会；没终端就按 Apple ID 走
    P8=$(/bin/ls -1 "$HOME/.appstoreconnect/private_keys"/AuthKey_*.p8 2>/dev/null | head -1 || true)
    if [[ -n "$P8" && -t 0 ]]; then
      read -r "kid?找到 $(basename "$P8")。Key ID（回车改用 Apple ID）: "
      if [[ -n "$kid" ]]; then
        export ASC_KEY_ID="$kid"
        read -r "iss?Issuer ID: "; export ASC_ISSUER_ID="$iss"
        MODE=api
      fi
    fi
    [[ -z "$MODE" ]] && MODE=apple
  fi

  if [[ "$MODE" == "apple" ]]; then
        [[ -n "$APPLE_ID" ]] || APPLE_ID="${FASTLANE_USER:-}"
    if [[ -z "$APPLE_ID" ]]; then
      # 不能猜账号：写死一个别人的邮箱，会让人拿自己的密码去登一个陌生账号。
      if [[ -t 0 ]]; then
        read -r "APPLE_ID?你的 Apple ID 邮箱（团队 $TEAM_ID 的成员）: "
      fi
    fi
    [[ -n "$APPLE_ID" ]] || die "没有 Apple ID" "用 --apple-id you@example.com 指定；或改用 ASC API Key（推荐，无需 2FA）"
    export FASTLANE_USER="$APPLE_ID"
    if [[ -z "${FASTLANE_PASSWORD:-}" ]]; then
      if [[ -t 0 ]]; then
        read -rs "FASTLANE_PASSWORD?$APPLE_ID 的 Apple ID 密码（只在本进程用，不落盘）: "; echo
        export FASTLANE_PASSWORD
      else
        die "没有 Apple ID 密码（非交互环境）" "export FASTLANE_PASSWORD=… 后再跑；或改用 ASC_KEY_ID / ASC_ISSUER_ID"
      fi
    fi
    say "用 Apple ID 登录（$APPLE_ID）；如果弹 2FA 验证码，输一次即可"
  else
    say "用 ASC API Key 访问（Key ID ${ASC_KEY_ID:-?}…）"
    [[ -f "${ASC_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_${ASC_KEY_ID:-}.p8}" ]] \
      || die "找不到 API 私钥" "把 AuthKey_<Key ID>.p8 放进 ~/.appstoreconnect/private_keys/，或设 ASC_KEY_PATH"
  fi

  setup_ruby

  # ── 3. 证书 ───────────────────────────────────────────────
  if (( ! HAS_DIST )); then
    say "钥匙串里没有 Apple Distribution 证书，正在用本地生成的私钥申请一张"
    OUT=$(signing create-cert --type DISTRIBUTION --out-dir "$HOME/.appstoreconnect/tmp-certs") \
      || fail_from "$OUT" "建 Apple Distribution 证书失败"
    CN=$(json_str "$OUT" commonName)
    ok "已导入：${CN:-Apple Distribution}"
    keychain_has "Apple Distribution:" && HAS_DIST=1
  else
    ok "Apple Distribution 证书已在"
  fi

  if (( ! HAS_INSTALLER )); then
    say "钥匙串里没有 Installer 证书（.pkg 签名要用），正在申请一张"
    OUT=$(signing create-cert --type MAC_INSTALLER_DISTRIBUTION --out-dir "$HOME/.appstoreconnect/tmp-certs") \
      || fail_from "$OUT" "建 Installer 证书失败"
    CN=$(json_str "$OUT" commonName)
    ok "已导入：${CN:-3rd Party Mac Developer Installer}"
    keychain_has "3rd Party Mac Developer Installer:" && HAS_INSTALLER=1
  else
    ok "Installer 证书已在"
  fi

  # ── 4. App ID 与 ASC 记录 ─────────────────────────────────
  say "确认 App ID（$BUNDLE_ID）"
  OUT=$(signing ensure-bundle-id) || fail_from "$OUT" "App ID 检查/创建失败"
  STATUS=$(json_str "$OUT" status)
  [[ "$STATUS" == "created" ]] && ok "App ID 已新建" || ok "App ID 已存在"

  say "确认 App Store Connect 里的 app 记录"
  APP_OK=0
  for attempt in 1 2 3; do
    OUT=$(signing ensure-app)
    if [[ "$(json_bool "$OUT" ok)" == "true" ]]; then
      ST=$(json_str "$OUT" status)
      [[ "$ST" == "created" ]] && ok "app 记录已新建" || ok "app 记录已存在"
      APP_OK=1
      break
    fi
    ERR=$(json_str "$OUT" error); HINT=$(json_str "$OUT" hint)
    warn "${ERR:-建 app 记录失败}（第 $attempt 次）"
    [[ -n "$HINT" ]] && warn "$HINT"
    (( DRY )) && break
    cat <<FORM
  在网页上建一次（30 秒），字段照抄：
    页面：https://appstoreconnect.apple.com/apps   → 左上「+」→ 新建 App
    平台      macOS
    名称      $ASC_APP_NAME
    主要语言  简体中文
    Bundle ID $BUNDLE_ID（下拉里选；没有就是 App ID 还没生效，等 1 分钟刷新）
    SKU       pastejar-mas
    用户访问  全选
FORM
    /usr/bin/open "https://appstoreconnect.apple.com/apps" 2>/dev/null || true
    if [[ -t 0 ]]; then
      read -r "?建好后回车复查（直接回车=先跳过，pkg 照出，上传前必须补上）: "
      [[ -z "${REPLY:-}" ]] && break
    else
      break
    fi
  done
  if (( ! APP_OK )); then
    warn "ASC 里还没有这个 app 的记录：pkg 可以出，但上传前必须在网页建好"
  fi

  # ── 5. profile ────────────────────────────────────────────
  if (( ! PROFILE_OK )); then
    say "建 Mac App Store profile 并装到本机"
    /bin/mkdir -p "$PROFILE_DIR" 2>/dev/null || true
    OUT=$(signing create-profile --name "$PROFILE_NAME" --out "$PROFILE_PATH") \
      || fail_from "$OUT" "建 profile 失败"
    ok "profile 已写入 $PROFILE_PATH"
    profile_ok || die "profile 校验没通过（bundle id 不是 $TEAM_ID.$BUNDLE_ID）" \
      "删掉 $PROFILE_PATH 重跑本命令；若还不行，检查 App ID 是否属于 $TEAM_ID"
  fi
fi

# ── 6. 构建 ─────────────────────────────────────────────────
say "构建（universal）+ 嵌 profile + 签名 + 出 pkg"
if (( DRY )); then
  printf '  $ PROVISION_PROFILE=%q %s/build-mas.sh\n' "$PROFILE_PATH" "$ROOT"
else
  PROVISION_PROFILE="$PROFILE_PATH" "$ROOT/build-mas.sh" || die "构建失败（见上面的输出）" \
    "如果是签名报错，确认这份 profile 与钥匙串里的 Apple Distribution 是同一张证书"
fi

VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$ROOT/Resources/Info.plist" 2>/dev/null)
PKG="$ROOT/dist/PasteJar-${VERSION}.pkg"

if (( DRY )); then
  say "dry-run 结束：以上命令会被依次执行；--upload 时再加 fastlane deliver"
  exit 0
fi

[[ -f "$PKG" ]] || die "没有产出 $PKG" "看 build-mas.sh 的最后几行：最常见是证书与 profile 不匹配"
ok "产物：$PKG"

# ── 7. 自检：签名链 / 架构 / 嵌入的 profile ─────────────────
say "自检 pkg 与 app"
if /usr/sbin/pkgutil --check-signature "$PKG" 2>&1 | /usr/bin/grep -q "Apple Root CA"; then
  ok "pkg 签名链完整（链到 Apple Root CA）"
else
  die "pkg 签名链不完整" "多半是 Installer 证书没签上：./scripts/mas-doctor.sh 看证书，再重跑构建"
fi

APP_BUNDLE="$ROOT/build/PasteJar.app"
BIN="$APP_BUNDLE/Contents/MacOS/PasteJar"
if [[ -f "$BIN" ]]; then
  ARCHS_INFO=$(/usr/bin/lipo -info "$BIN" 2>/dev/null || true)
  if [[ "$ARCHS_INFO" == *arm64* && "$ARCHS_INFO" == *x86_64* ]]; then
    ok "二进制 universal（x86_64 + arm64）"
  else
    die "出包用的不是 universal（$ARCHS_INFO）" "ARCHS=host 只用于本地冒烟；正式出包用默认值"
  fi
fi

ENT_KEYS=$(/usr/bin/codesign -d --entitlements :- "$APP_BUNDLE" 2>/dev/null | /usr/bin/grep -o 'com.apple.security[a-z.-]*' | sort -u | tr '\n' ' ')
for k in com.apple.security.app-sandbox com.apple.security.network.client com.apple.security.accessibility; do
  case " $ENT_KEYS " in
    *" $k "*) : ;;
    *) die "app 缺 entitlements：$k（现有：$ENT_KEYS）" "确认 Resources/PasteJar.entitlements 没被改动，再重跑构建" ;;
  esac
done
ok "三份 sandbox entitlements 都在"

if [[ -f "$APP_BUNDLE/Contents/embedded.provisionprofile" ]]; then
  EMB=$(/usr/bin/strings -a "$APP_BUNDLE/Contents/embedded.provisionprofile" | /usr/bin/grep -oE '[A-Z0-9]{10}\.com\.[A-Za-z0-9._-]+' | head -1)
  [[ "$EMB" == "$TEAM_ID.$BUNDLE_ID" ]] || die "嵌入的 profile 是「$EMB」" "重新生成 profile：./MAS-上架.command"
  ok "app 里嵌的 profile 正确（$EMB）"
else
  die "app 里没有 embedded.provisionprofile" "用 PROVISION_PROFILE=… 重新构建"
fi

# ── 8. 上传 ─────────────────────────────────────────────────
if (( UPLOAD )); then
  say "上传到 App Store Connect"
  KEY_JSON=""
  if [[ -n "${ASC_KEY_ID:-}" && -n "${ASC_ISSUER_ID:-}" ]]; then
    KEY_JSON="$(mktemp -t pastejar-asc-key).json"
    # deliver 需要 key 内容在 json 里；文件权限 600，用完即删
    KEYPATH="${ASC_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_$ASC_KEY_ID.p8}"
    {
      printf '{"key_id":"%s","issuer_id":"%s","key":"' "$ASC_KEY_ID" "$ASC_ISSUER_ID"
      /usr/bin/awk 'NF {printf "%s\\n", $0}' "$KEYPATH"
      printf '","in_house":false}'
    } > "$KEY_JSON"
    chmod 600 "$KEY_JSON" 2>/dev/null || true
  fi

  DELIVER_ARGS=(deliver --pkg "$PKG" --platform osx --skip_metadata --skip_screenshots)
  [[ -n "$KEY_JSON" ]] && DELIVER_ARGS+=(--api_key_path "$KEY_JSON")

  # 409（build 号已被占用）是上传最常见的失败：同一版本第二次传必然撞上。
  # 这里自动把 CFBundleVersion +1、重出包、重传，最多两次，并把每次的 build 号打出来。
  UPLOAD_OK=0
  for attempt in 1 2 3; do
    LOG="$(mktemp -t pastejar-deliver)"
    if fastlane "${DELIVER_ARGS[@]}" 2>&1 | /usr/bin/tee "$LOG"; then
      UPLOAD_OK=1
      /bin/rm -f "$LOG" 2>/dev/null || true
      break
    fi
    if /usr/bin/grep -qiE "already been used|Redundant Binary Upload|ITMS-4238|status code: 409|(^|[^0-9])409([^0-9]|$)" "$LOG"; then
      CUR=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$ROOT/Resources/Info.plist" 2>/dev/null || echo 0)
      NEXT=$((CUR + 1))
      warn "build 号 $CUR 已被占用 → 改成 $NEXT，重出包再传（第 $attempt 次）"
      /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $NEXT" "$ROOT/Resources/Info.plist" \
        || die "改不动 Info.plist 的 CFBundleVersion" "手动把它加一，再跑 ./MAS-上架.command --upload"
      PROVISION_PROFILE="$PROFILE_PATH" "$ROOT/build-mas.sh" || die "重出包失败" "看上面的构建输出"
      /bin/rm -f "$LOG" 2>/dev/null || true
      continue
    fi
    /bin/rm -f "$LOG" 2>/dev/null || true
    break
  done

  if (( UPLOAD_OK )); then
    ok "已上传（构建进入「处理中」，几分钟后在 ASC 的「构建」里选它）"
  else
    warn "fastlane deliver 没成功，改用 Transporter 兜底"
    PROVISION_PROFILE="$PROFILE_PATH" "$ROOT/upload-mas.sh" --upload \
      || die "上传失败" "401/403 是凭据或角色问题；网络类错误看上面最后一行；修好重跑 ./MAS-上架.command --upload"
  fi
  [[ -n "$KEY_JSON" && -f "$KEY_JSON" ]] && /bin/rm -f "$KEY_JSON" 2>/dev/null || true
else
  say "下一步"
  cat <<NEXT
  ✓ 可上传的 pkg 已就绪：$PKG
    上传：./MAS-上架.command --upload
    或在 Transporter.app 里拖进去（用 Apple ID 登录一次即可）

  上传之后只剩三件事需要人做（脚本改不了）：
    1) ASC → 该 App → 构建，选这次的版本
    2) 填「审核联系人」姓名与电话（草稿里这两项是空的）
    3) 点「提交以供审核」
NEXT
fi
