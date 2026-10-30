#!/bin/zsh
# 双击运行：从 Finder 的终端启动，这样钥匙串可写、2FA 弹窗也正常。
# 直接跑 ./MAS-上架.command 等价；参数原样转给 scripts/mas-auto.sh。
cd "$(dirname "$0")" || exit 1
exec ./scripts/mas-auto.sh "$@"
