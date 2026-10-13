# AGENTS.md — ClipVault 工作区入口

本文件是团队成员与未来 AI agent 的入口指引。

## 单一事实来源

macOS 侧的需求（R1–R17）、验收示例（AE1–AE8）、技术决策（KTD1–KTD6）、实施单元（U1–U11）、
验证契约与 Definition of Done 定义在：

**[`docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md`](./docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md)**

Windows 支持的需求（RW1–RW9）、验收示例（AEW1–AEW8）、技术决策（KTD10–KTD17）、
实施单元（U1–U9）、平台差异与限制定义在：

**[`docs/plans/2026-10-13-1716-feat-windows-platform-support-plan.md`](./docs/plans/2026-10-13-1716-feat-windows-platform-support-plan.md)**

两份 plan 的分工：MVP plan 仍是 macOS 侧产品契约的事实来源，但它「跨平台 / Windows 明确不在
范围内」的声明**已被 Windows plan 取代** —— 任何涉及 Windows 的改动以 Windows plan 为准。
多类型采集（R18–R21）另见 `docs/plans/2026-09-27-1038-feat-multi-type-clipboard-monitoring-plan.md`。

任何改动前请以上述 plan 为准；本仓库其余文档不得与之冲突。

## 项目概览

ClipVault 是 **macOS / Windows 双平台**的剪贴板管理器：Tauri 2.x 单进程桌面应用，
菜单栏（macOS）/ 通知区托盘（Windows）常驻 + 全局快捷键呼出 Raycast 风格命令面板
（macOS `⌥⌘V` / Windows `Ctrl+Alt+V`），SQLite + FTS5 搜索，模拟按键回贴
（macOS Accessibility + CGEvent `⌘V` / Windows `SendInput` `Ctrl+V`），AES-256-GCM 加密区，
数据密钥交给系统托管（macOS Keychain / Windows 凭据管理器）。采集 text / image / file / rtf
四种类型（Windows 采集侧不产生 color 条目，见 Windows plan U1）。**默认离线，不联网。**

## 工作区结构

- `clipvault-core/` — 跨 binary 共享的类型、schema、分类启发式、加密算法（无 Keychain）。
- `app/` — Tauri 主 binary（storage / watcher / search / notes / pin / pasteback / crypto / privacy / commands）。
- `frontend/` — WebView 静态资源（vanilla TS + Vite）。
- `packaging/` — macOS：`codesign.sh`（ad-hoc 签名）、`dmg.sh`（DMG）、`build.sh`（按架构编排
  `arm64` / `x64`）；Windows：`build-windows.ps1`（NSIS 安装包）；`make_icons.py`（生成应用图标、
  ICO 与 Windows 托盘图标，纯 stdlib，无 PIL 依赖）。
- `docs/plans/` — 权威 plan。

## 常用命令

```bash
# 开发
cargo tauri dev

# 验证（提交前全部应通过）
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
(cd frontend && npm run build)

# 打包（macOS：arm64 与 x64 分别出包）
packaging/build.sh arm64
packaging/build.sh x64

# 打包（Windows，PowerShell）
powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1
```

## 关键约束

- **R13 网络出口**：不得引入 `reqwest` / `hyper` 或任何对外 socket。提交前审计：
  `cargo tree | grep -E 'reqwest|hyper'`（须空）、
  `grep -rE 'reqwest::|hyper::|TcpStream|std::net::' app/src clipvault-core/src`（须空）。
  Windows 侧同理 —— 新增的 `windows` crate 只用于剪贴板 / 热键 / 凭据 / 按键注入，不引入 socket；
  该目标的整树编译与 lint 由 CI 的 `windows` job 覆盖。
- **平台专用绑定**：macOS 的 `objc2` 家族与 `security-framework` 只在 `cfg(target_os = "macos")` 下
  编译；Windows 的 `windows` crate 只在 `cfg(target_os = "windows")` 下编译。平台差异要么走
  `#[cfg]` 分支，要么（更好）抽成**吃显式平台参数的纯函数**，让另一平台的分支也能在 macOS 上被单测
  （既有先例：`storage::resolve_data_dir`、`pasteback::modifier_label`、`tray::TrayState::tooltip`）。
- **签名与分发**：macOS 走 ad-hoc 签名、不上 Mac App Store，Gatekeeper 放行步骤见 `README.md`；
  Windows 的 NSIS 安装包**未做 Authenticode 签名**（需商业证书），首次运行会被 SmartScreen 拦一下，
  放行步骤同样见 `README.md`。

## ⚠️ 开发期「辅助功能」授权 —— 容易踩的第一个坑（**仅 macOS**）

**Windows 上没有这一节**：`SendInput` 注入不需要任何系统授权，`hotkey::check_accessibility` 恒为
已授权，前端也不渲染授权横幅（RW8）。以下是 macOS 专属内容。

`cargo tauri dev` 跑的是 debug 版二进制，macOS 把 Accessibility（辅助功能）授权**绑定到具体的可执行文件路径 + 签名**上。所以**需要授权的不是 ClipVault 本体，而是启动它的宿主进程——通常是你的终端 App**（Terminal / iTerm2 / Warp / VS Code 等，debug 二进制是终端 fork 出来的）。

如果你不授权 → 回贴不会自动执行；回贴后 ClipVault 会**先把内容写入剪贴板**，再提示「已复制到剪贴板，请手动 ⌘V 粘贴」——未授权不是死路，但自动回贴失效。

正确做法（细节 + 排障见 `README.md`）：
1. 打开 **「系统设置 → 隐私与安全性 → 辅助功能」**，在列表里找你**正在用的终端**，打开它的开关。
2. 回到终端按 `cargo tauri dev` 启动 ClipVault 即可。
3. 常见坑：**找 ClipVault → 找不到**（dev 期不在 ClipVault 名字下找，要在终端名字下找）。
4. clean rebuild / 切 target 后授权偶尔失效，重新勾一下即可；**打包后的 `.app` 没有这个问题**。

任何接手这个仓库的 agent,**第一件事就是确认终端在辅助功能列表里**,不然调试回贴时会浪费 15 分钟。

## 实施进度

- **macOS MVP**：U1–U11 全部完成（`git log` 可见各 U 的 commit）。
- **多类型采集**：U1–U6 完成（见多类型 plan）。
- **Windows 支持**：U1–U9 完成 —— 采集 / 热键 / 回贴 / 凭据、命令收口、托盘、前端跨平台化、
  打包与 CI、文档均已落地。剩余的是 **Windows 真机验收**（Windows plan 的 AEW1–AEW8：
  托盘在浅/深色任务栏下的可见性、`Ctrl+X` 文案与 SVG 字形、无授权横幅、加密区端到端等）。
- **测试基线**：app 131 passed + core 22 passed，1 个 trigram 测试 ignored
  （依赖编译期 `SQLITE_FTS5_TOKENIZER_TRIGRAMS` 标志，见 KTD3）。
- **Windows 目标的整树编译与 lint 只能在 CI 上验证**：本地开发机是 macOS，
  `cfg(target_os = "windows")` 的分支根本不参与编译。`.github/workflows/package.yml` 的
  `windows` job 跑 `clippy -D warnings` + `cargo test` + NSIS 打包；改动涉及 Windows 分支时
  务必推上去跑一次，别只看本机绿。
