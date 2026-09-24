#!/usr/bin/env bash
# ClipVault — DMG 打包脚本 (U11 / KTD6)
#
# 用途：把已签名的 ClipVault.app 打进可分发的 DMG，内含 /Applications 符号链接，
# 用户拖拽即可安装。优先使用 homebrew 的 create-dmg（带背景/布局）；
# 未安装时回退到系统自带的 hdiutil。
#
# 注意：cargo tauri build 在 bundle.targets 含 "dmg"/"all" 时也会自行产出一个 DMG
# （target/release/bundle/dmg/）。本脚本提供自定义/独立的打包路径，二者可任选其一。
#
# 用法：
#   packaging/dmg.sh [App路径] [输出DMG路径]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

APP_PATH="${1:-${ROOT_DIR}/target/release/bundle/macos/ClipVault.app}"
DMG_PATH="${2:-${ROOT_DIR}/target/release/bundle/dmg/ClipVault.dmg}"
VOL_NAME="ClipVault"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "错误：DMG 打包依赖 macOS 工具（create-dmg / hdiutil），只能在 macOS 上执行。" >&2
  exit 1
fi

if [[ ! -d "${APP_PATH}" ]]; then
  echo "错误：找不到 .app：${APP_PATH}" >&2
  echo "请先运行 cargo tauri build（或 packaging/build.sh）生成 bundle，并执行 codesign.sh 签名。" >&2
  exit 1
fi

mkdir -p "$(dirname "${DMG_PATH}")"
rm -f "${DMG_PATH}"

if command -v create-dmg >/dev/null 2>&1; then
  echo "==> 使用 create-dmg 生成 DMG"
  create-dmg \
    --volname "${VOL_NAME}" \
    --window-size 540 380 \
    --icon-size 128 \
    --icon "ClipVault.app" 140 190 \
    --app-drop-link 400 190 \
    --hide-extension "ClipVault.app" \
    "${DMG_PATH}" \
    "${APP_PATH}"
else
  echo "==> 未检测到 create-dmg，回退到 hdiutil"
  STAGE_DIR="$(mktemp -d)"
  trap 'rm -rf "${STAGE_DIR}"' EXIT

  # 拷贝 .app 并创建 /Applications 符号链接，让用户能拖拽安装。
  cp -R "${APP_PATH}" "${STAGE_DIR}/"
  ln -s /Applications "${STAGE_DIR}/Applications"

  hdiutil create \
    -volname "${VOL_NAME}" \
    -srcfolder "${STAGE_DIR}" \
    -ov \
    -format UDZO \
    "${DMG_PATH}"
fi

echo "==> DMG 已生成：${DMG_PATH}"
ls -lh "${DMG_PATH}" | sed 's/^/    /'
echo "==> 完成。"
