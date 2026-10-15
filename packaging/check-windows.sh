#!/usr/bin/env bash
# ClipVault — Windows 目标的本地类型检查 / lint（**不链接、不执行**）
#
# 用法：
#   packaging/check-windows.sh            # cargo check --workspace --all-targets
#   packaging/check-windows.sh clippy     # cargo clippy … -- -D warnings（CI 口径）
#
# 为什么需要它
# ------------
# `#[cfg(target_os = "windows")]` 的分支在 macOS 开发机上**根本不参与编译**：写错一个 Win32
# 常量路径、参数类型，或漏掉一个 `Send` 约束，本地 cargo check 一个字都不会说，只有等问题
# 漂到真机（或 CI）才暴露 —— Windows 全局热键的唤醒缺陷就是这么漏掉的。本脚本把这类
# 类型 / lint 反馈提前到本地，成本只有几秒。
#
# 它是怎么工作的
# --------------
# 本机（macOS）编译 `x86_64-pc-windows-msvc` 会卡在两个 C 侧步骤：`libsqlite3-sys` 要 MSVC
# 工具链编译 `sqlite3.c`，`tauri-winres`/`embed_resource` 要 Windows 的 `llvm-rc` 编译 `.rc`。
# 但 `cargo check` **不链接**，这两个产物其实没人要。脚本在临时目录生成三个桩放到 PATH 顶上，
# 让这两步「成功但无意义」地过去，从而拿到整树的真实类型检查与 clippy（含 `--all-targets`，
# 即 Windows-only 的测试代码也会被编译）。
#
# 它证明什么、不证明什么
# ----------------------
# 证明：`cfg(windows)` 分支的类型正确、clippy 干净、feature 齐全、Windows-only 测试代码能编译。
# 不证明：**链接与运行** —— 桩不产出真实目标文件，所以这里既不能 `cargo test`、也不能
# `cargo build`。那两件事只能交给 CI 的 `windows` job（真实 MSVC 工具链）与真机验收
# （Windows plan 的 AEW1–AEW9）。**CI 跑绿之前不得声明 Windows 支持已完成。**
#
# 前置：`rustup target add x86_64-pc-windows-msvc`（本机已装则跳过）。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

TARGET="x86_64-pc-windows-msvc"
MODE="${1:-check}"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "错误：桩按 macOS 宿主编译器搭建，请在 macOS 上运行。" >&2
  exit 1
fi

if ! rustup target list --installed | grep -qx "${TARGET}"; then
  echo "错误：未安装 ${TARGET}。请先执行：rustup target add ${TARGET}" >&2
  exit 1
fi

CLANG="$(xcrun --find clang 2>/dev/null || command -v clang || true)"
if [[ -z "${CLANG}" ]]; then
  echo "错误：找不到 clang（需要 Xcode Command Line Tools）。" >&2
  exit 1
fi

# SDKROOT 必须显式导出：`xcrun --find clang` 解析到的是「当前 Xcode 工具链里的 clang」，它不像
# `/usr/bin/clang` 那个 shim 会自动向 xcrun 要 sysroot —— 不设 SDKROOT 时连 `<stdio.h>` 都找不到，
# 报错形如 "sqlite3.c: fatal error: 'stdio.h' file not found"（plan 里记的「缺 stdlib.h」就是它）。
export SDKROOT="$(xcrun --show-sdk-path)"

STUB_DIR="$(mktemp -d)"
trap 'rm -rf "${STUB_DIR}"' EXIT

# cc：丢掉 --target=windows，用宿主 clang 编出 .o（check 不链接，产物无意义也无妨）。
cat >"${STUB_DIR}/cc" <<EOF
#!/bin/sh
args=""
for a in "\$@"; do
  case "\$a" in --target=*) ;; *) args="\$args \$a" ;; esac
done
exec "${CLANG}" \$args
EOF

# ar：cc-rs 对 msvc 目标发 lib.exe 风格参数（-out:X -nologo），翻译成宿主 ar。
cat >"${STUB_DIR}/ar" <<'EOF'
#!/bin/sh
out=""; objs=""
while [ $# -gt 0 ]; do
  case "$1" in
    -out:*) out="${1#-out:}" ;;
    -*) ;;
    *) objs="$objs $1" ;;
  esac
  shift
done
exec /usr/bin/ar -crs "$out" $objs
EOF

# llvm-rc：embed_resource 先嗅探版本，要求 stdout 以 OVERVIEW 开头且含 no-preprocess。
cat >"${STUB_DIR}/llvm-rc" <<'EOF'
#!/bin/sh
for a in "$@"; do
  case "$a" in
    -V|"/?"|"-?") echo "OVERVIEW: LLVM Resource Converter (no-preprocess)"; exit 0 ;;
  esac
done
exit 0
EOF

chmod +x "${STUB_DIR}"/cc "${STUB_DIR}"/ar "${STUB_DIR}"/llvm-rc

export PATH="${STUB_DIR}:${PATH}"
export CC_x86_64_pc_windows_msvc="${STUB_DIR}/cc"
export AR_x86_64_pc_windows_msvc="${STUB_DIR}/ar"

case "${MODE}" in
  check)
    exec cargo check --workspace --all-targets --target "${TARGET}"
    ;;
  clippy)
    exec cargo clippy --workspace --all-targets --target "${TARGET}" -- -D warnings
    ;;
  *)
    echo "错误：未知模式 '${MODE}'（可用：check / clippy）。" >&2
    exit 1
    ;;
esac
