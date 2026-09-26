#!/usr/bin/env bash
# Builds "Pastory.app". Swift 版构建脚本已冻结归档（1.0.6 起由 docs/RUST_REWRITE.md M7 替换，仓库切换到 Rust 管线）。
# 本文件只作向后兼容 shim，委托给 build-rs.sh；SIGN_ID=<Developer ID> 会穿透。
set -euo pipefail
cd "$(dirname "$0")"
exec ./build-rs.sh "$@"
