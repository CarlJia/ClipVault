# AGENTS.md — ClipVault 工作区入口

本文件是团队成员与未来 AI agent 的入口指引。

## 单一事实来源

需求（R1–R17）、验收示例（AE1–AE8）、技术决策（KTD1–KTD6）、实施单元（U1–U11）、
验证契约与 Definition of Done 全部定义在：

**[`docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md`](./docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md)**

任何改动前请以该 plan 为准；本仓库其余文档不得与之冲突。

## 项目概览

ClipVault 是 macOS 上的纯文本剪贴板管理器：Tauri 2.x 单进程桌面应用，菜单栏常驻 +
全局快捷键（`⌥⌘V`）呼出 Raycast 风格命令面板，SQLite + FTS5 搜索，Accessibility 模拟
⌘V 回贴，Keychain 托管 AES-GCM 加密区。**默认离线，不联网。**

## 工作区结构

- `clipvault-core/` — 跨 binary 共享的类型、schema、分类启发式、加密算法（无 Keychain）。
- `app/` — Tauri 主 binary（storage / watcher / search / notes / pin / pasteback / crypto / privacy / commands）。
- `frontend/` — WebView 静态资源（vanilla TS + Vite）。
- `packaging/` — `codesign.sh`（ad-hoc 签名）、`dmg.sh`（DMG）、`build.sh`（编排）。
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

# 打包
packaging/build.sh
```

## 关键约束

- **R13 网络出口**：不得引入 `reqwest` / `hyper` 或任何对外 socket。提交前审计：
  `cargo tree | grep -E 'reqwest|hyper'`（须空）、
  `grep -rE 'reqwest::|hyper::|TcpStream|std::net::' app/src clipvault-core/src`（须空）。
- **macOS 专用**：原生绑定（`objc2` 家族、`security-framework`）仅在 `cfg(target_os = "macos")` 下编译。
- **签名与分发**：ad-hoc 签名，不上 Mac App Store；Gatekeeper 放行步骤见 `README.md`。

## ⚠️ 开发期「辅助功能」授权 —— 容易踩的第一个坑

`cargo tauri dev` 跑的是 debug 版二进制，macOS 把 Accessibility（辅助功能）授权**绑定到具体的可执行文件路径 + 签名**上。所以**需要授权的不是 ClipVault 本体，而是启动它的宿主进程——通常是你的终端 App**（Terminal / iTerm2 / Warp / VS Code 等，debug 二进制是终端 fork 出来的）。

如果你不授权 → 回贴不会自动执行；回贴后 ClipVault 会**先把内容写入剪贴板**，再提示「已复制到剪贴板，请手动 ⌘V 粘贴」——未授权不是死路，但自动回贴失效。

正确做法（细节 + 排障见 `README.md`）：
1. 打开 **「系统设置 → 隐私与安全性 → 辅助功能」**，在列表里找你**正在用的终端**，打开它的开关。
2. 回到终端按 `cargo tauri dev` 启动 ClipVault 即可。
3. 常见坑：**找 ClipVault → 找不到**（dev 期不在 ClipVault 名字下找，要在终端名字下找）。
4. clean rebuild / 切 target 后授权偶尔失效，重新勾一下即可；**打包后的 `.app` 没有这个问题**。

任何接手这个仓库的 agent,**第一件事就是确认终端在辅助功能列表里**,不然调试回贴时会浪费 15 分钟。

## 实施进度

U1–U11 全部完成（`git log` 可见各 U 的 commit）。当前测试基线：app 94 passed + core 22 passed，
1 个 trigram 测试 ignored（依赖编译期 `SQLITE_FTS5_TOKENIZER_TRIGRAMS` 标志，见 KTD3）。
