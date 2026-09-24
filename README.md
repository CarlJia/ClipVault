# ClipVault — macOS 剪贴板管理器

> MVP 开发中。详见 [docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md](./docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md)

## 开发状态

- [x] U1 — 项目脚手架 (进行中)
- [ ] U2 — 存储层 (SQLite + FTS5)
- [ ] U3 — 粘贴板监听 + 自动分类
- [ ] U4 — 前端浮层 + 菜单栏 + 全局快捷键
- [ ] U5 — 搜索 + Pin 优先排序
- [ ] U6 — 备注 & 标签编辑器
- [ ] U7 — Pin 切换 + UI 徽标
- [ ] U8 — Accessibility 权限 + 回贴
- [ ] U9 — 加密区 (AES-GCM + Keychain)
- [ ] U10 — 访客模式 + 本次不记录
- [ ] U11 — 打包 (.app + ad-hoc 签名 + DMG)

## 快速开始

```bash
cargo install tauri-cli --version "^2.0" --locked
cargo tauri dev
```

## 验证

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```
