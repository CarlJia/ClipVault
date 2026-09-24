#!/usr/bin/env bash
# ClipVault — 一键打包编排脚本 (U11 / KTD6)
#
# 流程：构建前端 → cargo tauri build（尽量 universal2）→ ad-hoc 签名 → 打 DMG。
# 全程只用本地工具，不连接网络。
#
# 用法：
#   packaging/build.sh              # 自动选择 universal2 或原生架构
#   packaging/build.sh --native     # 强制只构建当前机器架构

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "错误：打包只能在 macOS 上执行。" >&2
  exit 1
fi

FORCE_NATIVE=0
[[ "${1:-}" == "--native" ]] && FORCE_NATIVE=1

echo "==> [1/4] 构建前端"
(cd frontend && npm ci && npm run build)

echo "==> [2/4] cargo tauri build"
# 若两个架构 target 都已安装，走 universal2（Apple Silicon + Intel 通吃）。
HAS_ARM=$(rustup target list --installed 2>/dev/null | grep -c '^aarch64-apple-darwin$' || true)
HAS_X86=$(rustup target list --installed 2>/dev/null | grep -c '^x86_64-apple-darwin$' || true)

if [[ "${FORCE_NATIVE}" -eq 0 && "${HAS_ARM}" -ge 1 && "${HAS_X86}" -ge 1 ]]; then
  echo "    两个架构 target 均可用 → universal-apple-darwin"
  cargo tauri build --target universal-apple-darwin
  APP_PATH="${ROOT_DIR}/target/universal-apple-darwin/release/bundle/macos/ClipVault.app"
  DMG_DIR="${ROOT_DIR}/target/universal-apple-darwin/release/bundle/dmg"
else
  echo "    仅构建原生架构（如需 universal2 发布：rustup target add x86_64-apple-darwin aarch64-apple-darwin）"
  cargo tauri build
  APP_PATH="${ROOT_DIR}/target/release/bundle/macos/ClipVault.app"
  DMG_DIR="${ROOT_DIR}/target/release/bundle/dmg"
fi

echo "==> [3/4] ad-hoc 签名"
"${SCRIPT_DIR}/codesign.sh" "${APP_PATH}"

echo "==> [4/4] 打 DMG"
"${SCRIPT_DIR}/dmg.sh" "${APP_PATH}" "${DMG_DIR}/ClipVault.dmg"

echo "==> 全部完成。"
echo "    .app  → ${APP_PATH}"
echo "    .dmg  → ${DMG_DIR}/ClipVault.dmg"
