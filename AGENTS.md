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
  `arm64` / `x64`）；Windows：`build-windows.ps1`（NSIS 安装包）、`check-windows.sh`（本机对
  Windows 目标做整树 `check` / `clippy`，桩掉 C 侧，**不链接、不执行**）；`make_icons.py`（从
  `icon-source.png` 生成 `app/icons/` 下全部图标，纯 stdlib，无 PIL 依赖、不依赖 macOS，
  改图标只改源图后重跑：`python3 packaging/make_icons.py`）。
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

# 改到 cfg(target_os = "windows") 的分支时必跑：本机整树类型检查 + lint（桩掉 C 侧；不链接、不执行）
packaging/check-windows.sh
packaging/check-windows.sh clippy

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
- **诊断日志与查看入口**：U1–U6 完成（见 `docs/plans/2026-10-15-1642-feat-diagnostics-log-viewer-plan.md`）——
  `app/src/log.rs` 纯函数 + 文件 sink、轮转（main + .1）、`log` crate facade、`Level`
  持久化到 `settings` 表、设置面板「诊断日志」区、托盘「查看日志…」在系统文件管理器
  中显示。R13 仍为空（`log` 0.4.34 早就在依赖树内,升为直接依赖零新 crate）。
- **Windows 支持**：U1–U10 完成 —— 采集 / 热键 / 回贴 / 凭据、命令收口、托盘、前端跨平台化、
  打包与 CI、文档均已落地，外加 **U10 单实例守卫**（重复启动只激活已有实例，见 RW10 / KTD18）。
  剩余的是 **Windows 真机验收**（Windows plan 的 AEW1–AEW9：托盘在浅/深色任务栏下的可见性、
  `Ctrl+X` 文案与 SVG 字形、无授权横幅、加密区端到端、重复启动不产生第二个实例等）。
- **多屏摆放与设置面板适配**：U1–U6 完成（见
  `docs/plans/2026-10-14-1818-feat-panel-display-placement-and-settings-fit-plan.md`）——
  面板按「焦点屏」（前台窗口所在屏；前台是自己时取鼠标屏）定位、Windows 侧同源实现，
  唤出入口收进 `app/src/panel.rs`；设置模态在固定的 560×420 面板内滚动、透明度滑块可达；
  透明度改由 `--panel-alpha` 单一变量驱动（面板背景与设置遮罩同源）。剩余：macOS 真机
  AE1–AE6、Windows 真机 AE1–AE3 与托盘自指场景。
- **测试基线**：app 219 passed + core 27 passed（合计 246），1 个 trigram 测试 ignored；
  增量来自 `app/src/log.rs`（U1 16 + U2 11 = 27 个新单测）、`app/src/single_instance.rs`
  （1 个纯函数测试）与 `app/src/watcher_test.rs`（2 条：重复指纹文案的格式契约 +
  库层 `content_hash` 去重被吞掉）。Windows-only 的 `win_pump_tests` /
  `single_instance::win_tests` 只在 CI 的 `windows` job 与真机上跑，不计入本机基线。
  `frontend/src/logview.test.ts` 6 例由 `npm --prefix frontend test` 覆盖。
  （依赖编译期 `SQLITE_FTS5_TOKENIZER_TRIGRAMS` 标志，见 KTD3）。
- **Windows 分支的本地检查与 CI 分工**：`cfg(target_os = "windows")` 的分支在 macOS 上**不参与**
  本地 `cargo check`，所以改到它时必须跑 `packaging/check-windows.sh`（桩掉 C 侧后拿到整树类型检查
  与 clippy，几秒；**不链接、不执行**，边界写在脚本头部注释里）。**构建与执行**仍然只有 CI 能验：
  `.github/workflows/ci.yml` 在**每个 PR** 上跑（macOS：fmt / clippy / `cargo test` / 前端构建 +
  vitest / R13 网络出口审计；Windows：clippy + `cargo test`，真实 MSVC 工具链），所以涉及 Windows
  分支的改动提 PR 即自动验证。`.github/workflows/package.yml` 只在推 `v*` tag 或手动触发时做打包
  （DMG / NSIS / Release），不做校验。
