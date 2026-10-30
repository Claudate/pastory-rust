#!/bin/zsh
# 把一台干净的 Mac 补到「能构建、能签名、能上传」。
#
# 为什么单独一个脚本：干净机器缺的东西是分层的——CLT → Homebrew → Rust/fastlane，
# 每一层缺失时的报错都指向下一层，堆在构建脚本里会让人分不清先修哪个。
# 每一步都「先探测、再动手」，可重复跑；不为了流程继续而假装成功。
#
# 用法：
#   ./scripts/mas-bootstrap.sh              补齐缺的工具（需要密码或点「安装」时会提示）
#   ./scripts/mas-bootstrap.sh --check      只检查，不安装
#   ./scripts/mas-bootstrap.sh --mirror cn  用国内镜像装 rustup（第三方镜像，随时可关）
#
# 退出码：0 就绪；1 仍有缺项
set -u
setopt NO_NOMATCH 2>/dev/null || true

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CHECK=0
MIRROR=""
for a in "$@"; do
  case "$a" in
    --check) CHECK=1 ;;
    --mirror) MIRROR="cn" ;;
    --mirror=*) MIRROR="${a#--mirror=}" ;;
    -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$a（只接受 --check / --mirror cn）" >&2; exit 2 ;;
  esac
done

FAILED=0
say() { printf '→ %s\n' "$*" }
ok()  { printf '  ✓ %s\n' "$*" }
bad() { printf '  ✗ %s\n' "$*"; FAILED=1 }
skip(){ printf '  ⤼ %s\n' "$*" }

# ── 1. Command Line Tools ───────────────────────────────────
# clang/SDK/productbuild/git 都来自它。productbuild 没有 --version，
# 用「xcrun 能解析 + 会打印 usage」判定可用。
clt_ok() {
  [[ -n "$(/usr/bin/xcode-select -p 2>/dev/null || true)" ]] \
    && /usr/bin/xcrun -f productbuild >/dev/null 2>&1 \
    && /usr/bin/productbuild 2>&1 | /usr/bin/grep -q "Usage: productbuild"
}
if clt_ok; then
  ok "Command Line Tools：$(/usr/bin/xcode-select -p 2>/dev/null)"
elif [[ $CHECK == 1 ]]; then
  bad "缺 Command Line Tools（xcode-select --install）"
else
  say "缺 Command Line Tools：现在执行 xcode-select --install"
  say "屏幕上会弹「要安装命令行工具吗」，点「安装」；这一步只有你能点"
  /usr/bin/xcode-select --install >/dev/null 2>&1 || true
  say "等待安装（最长 20 分钟；可 Ctrl-C 后重跑本脚本）…"
  for _ in {1..240}; do
    clt_ok && break
    sleep 5
  done
  if clt_ok; then
    ok "Command Line Tools 已就绪"
  else
    bad "20 分钟内没装完。装好后重跑本脚本；若弹窗没出现，去「系统设置 → 通用 → 软件更新」手动装命令行工具"
  fi
fi

# ── 2. Homebrew ─────────────────────────────────────────────
if command -v brew >/dev/null 2>&1; then
  ok "Homebrew：$(command -v brew)"
elif [[ $CHECK == 1 ]]; then
  bad "缺 Homebrew（官方安装脚本）"
else
  say "安装 Homebrew（会要一次管理员密码，下载几十 MB）…"
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" || true
  # Apple Silicon 装在 /opt/homebrew，Intel 在 /usr/local；装完要进 PATH 才能用
  if [[ -x /opt/homebrew/bin/brew ]]; then eval "$(/opt/homebrew/bin/brew shellenv)"; fi
  if [[ -x /usr/local/bin/brew ]]; then eval "$(/usr/local/bin/brew shellenv)"; fi
  if command -v brew >/dev/null 2>&1; then
    ok "Homebrew：$(command -v brew)"
  else
    bad "Homebrew 没装成功。常见原因是网络（国内直连 GitHub 会超时）——挂代理后重跑，或手动装好再来"
  fi
fi

# ── 3. Rust 工具链 + 两个架构目标 ───────────────────────────
export PATH="$HOME/.cargo/bin:$PATH"
if [[ "$MIRROR" == "cn" ]]; then
  # 第三方镜像（rsproxy），只在显式 --mirror cn 时启用；只加速下载，不改产物
  export RUSTUP_DIST_SERVER="https://rsproxy.cn"
  export RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"
  say "使用 rustup 国内镜像（rsproxy.cn），本次生效"
fi

if command -v rustup >/dev/null 2>&1; then
  rustup toolchain install stable --no-self-update >/dev/null 2>&1 || true
elif [[ $CHECK == 1 ]]; then
  bad "缺 Rust 工具链（rustup）"
else
  say "安装 Rust（rustup，装到 ~/.cargo，不需要管理员密码）…"
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path || true
  export PATH="$HOME/.cargo/bin:$PATH"
fi

if command -v cargo >/dev/null 2>&1; then
  ok "Rust：$(cargo --version 2>/dev/null | head -1)"
  if command -v rustup >/dev/null 2>&1; then
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
      if rustup target list --installed 2>/dev/null | /usr/bin/grep -qx "$t"; then
        skip "目标 $t 已在"
      else
        say "添加编译目标 $t …"
        if rustup target add "$t" >/dev/null 2>&1; then ok "目标 $t 已加"; else bad "添加目标 $t 失败"; fi
      fi
    done
  fi
else
  [[ $CHECK == 1 ]] || bad "cargo 仍不可用（装完要新开终端或 source ~/.cargo/env）"
fi

# ── 4. fastlane ─────────────────────────────────────────────
# sigh 的 MAC_APP_STORE 常量与 deliver 的 --pkg 都需要较新版本。
FL=$(command -v fastlane 2>/dev/null || true)
FL_VER=""
if [[ -n "$FL" ]]; then
  # 同 doctor：只认「以 fastlane 开头」那一行，别拿到依赖的版本号
  FL_VER=$("$FL" --version 2>/dev/null | /usr/bin/sed -n 's/^fastlane \([0-9][0-9.]*\).*/\1/p' | head -1)
  [[ -z "$FL_VER" ]] && FL_VER=$("$FL" --version 2>/dev/null | /usr/bin/grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
fi
if [[ -n "$FL_VER" ]] && [[ "$(printf '%s\n%s\n' "2.220.0" "$FL_VER" | sort -V | head -1)" == "2.220.0" ]]; then
  ok "fastlane：$FL_VER"
elif [[ $CHECK == 1 ]]; then
  bad "fastlane ${FL_VER:-缺}"
elif command -v brew >/dev/null 2>&1; then
  say "brew install fastlane（首次几分钟，含 Ruby 依赖）…"
  brew install fastlane || true
  FL=$(command -v fastlane 2>/dev/null || true)
  # 同 doctor：只认「以 fastlane 开头」那一行，别拿到依赖的版本号
  FL_VER=$("$FL" --version 2>/dev/null | /usr/bin/sed -n 's/^fastlane \([0-9][0-9.]*\).*/\1/p' | head -1)
  [[ -z "$FL_VER" ]] && FL_VER=$("$FL" --version 2>/dev/null | /usr/bin/grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)
  if [[ -n "$FL_VER" ]]; then ok "fastlane：$FL_VER"; else bad "fastlane 没装上"; fi
else
  bad "没有 Homebrew，装不了 fastlane"
fi

# ── 5. git / gh（私库 clone 用）──────────────────────────────
if command -v git >/dev/null 2>&1; then ok "git：$(git --version)"; else bad "缺 git"; fi
if command -v gh >/dev/null 2>&1; then
  if gh auth status >/dev/null 2>&1; then
    ok "gh 已登录（私库 clone 可用）"
  else
    skip "gh 未登录：私库 clone 前先跑 gh auth login"
  fi
else
  skip "没有 gh：私库 clone 需要 PAT（或在 GitHub 网页用 token 克隆）"
fi

# ── 6. 汇总 ─────────────────────────────────────────────────
echo
if [[ -x "$ROOT/scripts/mas-doctor.sh" ]]; then
  say "当前还缺什么（doctor --quiet）："
  "$ROOT/scripts/mas-doctor.sh" --quiet || true
  echo
fi
if [[ $FAILED == 0 ]]; then
  # doctor 还会报「缺 profile / 缺凭据」这类账号侧的东西——那不是本脚本能补的，
  # 所以这里只说工具链就绪，并把下一步说清楚，避免「环境就绪」听起来像全齐了。
  if [[ -x "$ROOT/scripts/mas-doctor.sh" ]] && ! "$ROOT/scripts/mas-doctor.sh" --quiet >/dev/null 2>&1; then
    echo "工具链就绪；账号侧还缺（见上）。下一步：./MAS-上架.command（会带你补）"
  else
    echo "环境就绪。下一步：./MAS-上架.command"
  fi
  exit 0
fi
echo "仍有缺项（见上）。补齐后重跑本脚本即可——每步都会先探测，已装好的不会重复装。"
exit 1
