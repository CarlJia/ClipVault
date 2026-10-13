# ClipVault — Windows 打包脚本（U8）
#
# 与 packaging/build.sh 对称：那个负责 macOS 的 universal2 + ad-hoc 签名 + DMG，
# 这个负责 Windows 的 NSIS 安装包。两者共用同一份 tauri.conf.json 与前端产物。
#
# 流程：构建前端 → cargo tauri build --bundles nsis → 打印产物路径。
#
# 用法（PowerShell）：
#   powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1
#
# 前置：
#   - Rust（MSVC 工具链，rustup 默认的 x86_64-pc-windows-msvc）
#   - cargo-tauri：cargo install tauri-cli --version "^2.11" --locked
#   - Node.js（构建前端）
#
# 两点需要知道：
#   1. **只打 NSIS，不打 MSI。** tauri.conf.json 里 `targets: "all"` 在 Windows 上会连带
#      产出 MSI，而 MSI 需要额外下载 WiX 工具链 —— 那是这条链路上最容易失败的环节，
#      而且 MSI 对本项目（每用户安装的托盘小工具）没有额外价值。
#   2. **产物未做 Authenticode 签名**（需商业代码签名证书），首次运行会被 SmartScreen
#      拦一下。放行步骤见 README 的 Windows 一节。

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 用 $PSScriptRoot 而不是 $MyInvocation.MyCommand.Path：后者在
# `powershell -File packaging\build-windows.ps1` 这种相对路径调用下可能仍是相对路径，
# Split-Path 会剥成空串，后面的 Set-Location 直接失败。$PSScriptRoot 一定是绝对路径。
$rootDir = Split-Path -Parent $PSScriptRoot
Set-Location $rootDir

# PowerShell 不会因为原生命令返回非零退出码而中止（$ErrorActionPreference 只管 cmdlet），
# 所以每一步都得自己查 $LASTEXITCODE —— 否则中途失败的 npm/cargo 会被当成成功继续往下跑。
function Invoke-Step {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][scriptblock]$Action
    )
    Write-Host "==> $Label"
    & $Action
    if ($LASTEXITCODE -ne 0) {
        throw "$Label 失败（退出码 $LASTEXITCODE）"
    }
}

Invoke-Step '[1/2] 构建前端' {
    npm --prefix frontend ci
    if ($LASTEXITCODE -ne 0) { throw "npm ci 失败" }
    npm --prefix frontend run build
}

Invoke-Step '[2/2] cargo tauri build --bundles nsis' {
    cargo tauri build --bundles nsis
}

$outDir = Join-Path $rootDir 'target\release\bundle\nsis'
$installers = Get-ChildItem -Path $outDir -Filter *.exe -ErrorAction SilentlyContinue

Write-Host '==> 全部完成。'
if ($installers) {
    foreach ($file in $installers) {
        Write-Host ("    安装包 → {0} ({1:N1} MB)" -f $file.FullName, ($file.Length / 1MB))
    }
} else {
    # 打包命令返回 0 却没有产物，说明 bundler 行为和预期不一致，值得当面报出来。
    throw "打包报告成功，但 $outDir 下没有 .exe —— 请检查 bundler 输出。"
}
