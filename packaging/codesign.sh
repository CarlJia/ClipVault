#!/usr/bin/env bash
# ClipVault — ad-hoc 代码签名脚本 (U11 / KTD6)
#
# 用途：对 cargo tauri build 产出的 .app 做 ad-hoc 签名（无苹果开发者身份）。
# ad-hoc 签名不消除 Gatekeeper 拦截，但让 .app 具备稳定的代码签名（Signature=adhoc），
# 首次启动仍需用户手动放行（见 README「Gatekeeper 放行」一节）。
#
# 用法：
#   packaging/codesign.sh [App路径]
# 默认对 target/release/bundle/macos/ClipVault.app 签名。

set -euo pipefail

# 定位仓库根目录（脚本位于 <root>/packaging/）。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

APP_PATH="${1:-${ROOT_DIR}/target/release/bundle/macos/ClipVault.app}"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "错误：ad-hoc 签名依赖 macOS 的 codesign，只能在 macOS 上执行。" >&2
  exit 1
fi

if [[ ! -d "${APP_PATH}" ]]; then
  echo "错误：找不到 .app：${APP_PATH}" >&2
  echo "请先运行 cargo tauri build（或 packaging/build.sh）生成 bundle。" >&2
  exit 1
fi

echo "==> 对 ${APP_PATH} 执行 ad-hoc 签名"
# --force 覆盖已有签名；--deep 递归签名内嵌 framework/dylib；--sign - 表示 ad-hoc。
codesign --force --deep --sign - "${APP_PATH}"

echo "==> 验证签名"
# codesign -dv 把详情写到 stderr；捕获一次后既用于展示也用于断言，避免二次调用。
SIGN_INFO="$(codesign -dv "${APP_PATH}" 2>&1)"
echo "${SIGN_INFO}" | sed 's/^/    /'

# 断言签名确实是 ad-hoc。
if echo "${SIGN_INFO}" | grep -q "Signature=adhoc"; then
  echo "==> OK：Signature=adhoc"
else
  echo "警告：未在 codesign 输出中检测到 'Signature=adhoc'，请人工核对上方输出。" >&2
fi

echo "==> 完成。"
