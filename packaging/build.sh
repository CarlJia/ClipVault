#!/usr/bin/env bash
# ClipVault — 一键打包编排脚本 (U11 / KTD6)
#
# 流程：构建前端 → cargo tauri build → ad-hoc 签名 → 打 DMG。
# 全程只用本地工具，不连接网络。
#
# 用法：
#   packaging/build.sh              # 当前机器架构（开发日常）
#   packaging/build.sh arm64        # Apple Silicon（aarch64-apple-darwin）
#   packaging/build.sh x64          # Intel（x86_64-apple-darwin）
#
# 为什么按架构分开打、不做 universal2 `(session-settled: user-directed)`：
# universal2 要把两份二进制 lipo 合并成一个 .app，产物体积近似翻倍，而对这个
# 托盘小工具来说没有任何收益（用户只会在自己机器的架构上跑）；CI 上还得在同一个
# job 里串行编两遍。拆开之后每个架构一个独立 DMG（文件名带架构后缀），Release 里
# 各下各的，清单更清楚。代价：两个架构要各跑一次打包（CI 上用 matrix 并行）。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "错误：打包只能在 macOS 上执行。" >&2
  exit 1
fi

# ARCH：native（默认，不传 --target）/ arm64 / x64。`--native` 是历史写法，继续接受。
ARCH="${1:-native}"
[[ "${ARCH}" == "--native" ]] && ARCH="native"

case "${ARCH}" in
  native | arm64 | x64) ;;
  *)
    echo "错误：未知架构 '${ARCH}'。用法：packaging/build.sh [native|arm64|x64]" >&2
    exit 1
    ;;
esac

case "${ARCH}" in
  arm64)
    RUST_TARGET="aarch64-apple-darwin"
    ARCH_LABEL="arm64"
    ;;
  x64)
    RUST_TARGET="x86_64-apple-darwin"
    ARCH_LABEL="x64"
    ;;
  native)
    RUST_TARGET=""
    # 不传 --target 时产物跟着宿主架构走，标签也照宿主取，保证 DMG 文件名不会骗人。
    case "$(uname -m)" in
      arm64) ARCH_LABEL="arm64" ;;
      x86_64) ARCH_LABEL="x64" ;;
      *) ARCH_LABEL="$(uname -m)" ;;
    esac
    ;;
esac

# 版本号从 tauri.conf.json 读，用于 DMG 命名：Release 里两个架构的包必须一眼能区分。
# 用 node 而不是 sed：node 本来就是构建前端的前置依赖，而 JSON 用 sed 抓字段迟早出错。
VERSION="$(node -p "require('./app/tauri.conf.json').version")"

echo "==> [1/4] 构建前端"
(cd frontend && npm ci && npm run build)

echo "==> [2/4] cargo tauri build（${ARCH_LABEL}${RUST_TARGET:+ / ${RUST_TARGET}}）"
# `--bundles app` 是刻意的：tauri.conf.json 里是 `targets: "all"`，在 macOS 上会连带给
# bundler 自己的 DMG（`ClipVault_<版本>_<tauri 的架构名>.dmg`），与我们第 4 步用 dmg.sh
# 打的 DMG 挤在同一个 dmg/ 目录里，还各有各的架构后缀写法（aarch64 vs arm64）。
# 只要 .app，DMG 统一交给 dmg.sh —— 与 build-windows.ps1 显式写 `--bundles nsis` 同理。
if [[ -n "${RUST_TARGET}" ]]; then
  # 交叉编译也走这条路：Apple Silicon 上编 x86_64 由 SDK 自带的两份 slice 支撑，
  # 不需要额外的 linker 垫片，只要求 rustup 里装了这个 target。
  if ! rustup target list --installed | grep -qx "${RUST_TARGET}"; then
    echo "错误：未安装 target ${RUST_TARGET}。先执行：rustup target add ${RUST_TARGET}" >&2
    exit 1
  fi
  cargo tauri build --target "${RUST_TARGET}" --bundles app
  BUNDLE_DIR="${ROOT_DIR}/target/${RUST_TARGET}/release/bundle"
else
  echo "    未指定架构 → 构建宿主架构（$(uname -m)）"
  cargo tauri build --bundles app
  BUNDLE_DIR="${ROOT_DIR}/target/release/bundle"
fi

# 每个 --target 各有自己的 bundle 目录，两个架构的产物天然互不覆盖。
APP_PATH="${BUNDLE_DIR}/macos/ClipVault.app"
DMG_PATH="${BUNDLE_DIR}/dmg/ClipVault_${VERSION}_${ARCH_LABEL}.dmg"

echo "==> [3/4] ad-hoc 签名"
"${SCRIPT_DIR}/codesign.sh" "${APP_PATH}"

echo "==> [4/4] 打 DMG"
"${SCRIPT_DIR}/dmg.sh" "${APP_PATH}" "${DMG_PATH}"

echo "==> 全部完成。"
echo "    .app  → ${APP_PATH}"
echo "    .dmg  → ${DMG_PATH}"
if [[ -z "${RUST_TARGET}" ]]; then
  echo "    提示：正式发版要两个架构都打 —— packaging/build.sh arm64 && packaging/build.sh x64"
fi
