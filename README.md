# ClipVault — macOS 纯文本剪贴板管理器

ClipVault 常驻菜单栏，持续记录你复制过的纯文本，通过全局快捷键（默认 `⌥⌘V`）呼出 Raycast 风格命令面板，即时搜索、按 `Enter` 回贴到当前光标。支持自动类别（URL / Email / 路径 / 代码 / JSON / 纯文本）、自由标签、备注、逐条 Pin 置顶、AES-GCM 加密区与访客模式。**默认完全离线，不连接网络、不上传任何数据。**

单一事实来源（需求 / 设计 / 验收）：[`docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md`](./docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md)

---

> ## ⚠️ macOS 14+（Sonoma / Sequoia）Apple Silicon 特别声明
>
> ClipVault 采用 **ad-hoc 签名**（无苹果开发者账号、未经 notarize）。在 macOS 14+ 的 Apple Silicon 设备上，Gatekeeper 对未 notarized 应用的拦截更严格，双击可能直接报「已损坏 / 无法打开」，且**右键「打开」不一定出现放行选项**。
>
> 遇到这种情况，用命令行移除隔离属性即可放行（一次性）：
>
> ```bash
> xattr -d com.apple.quarantine /Applications/ClipVault.app
> ```
>
> 若提示 `No such xattr: com.apple.quarantine`（属性本就不存在），可忽略并直接打开；如仍失败，递归清一遍：
>
> ```bash
> xattr -cr /Applications/ClipVault.app
> ```
>
> 之后正常双击启动即可。这是 ad-hoc 分发的已知限制；正式 notarize 需要苹果开发者账号，不在本 MVP 范围内。

---

## 安装

1. 从 DMG 安装：双击 `ClipVault.dmg`，把 `ClipVault.app` 拖入 `Applications`。
2. 首次启动会被 Gatekeeper 拦截（因为是 ad-hoc 签名）。按下面「Gatekeeper 放行」放行一次即可，以后正常双击。
3. 首次运行会引导你授予 **Accessibility 权限**（用于回贴，见下文）。

### Gatekeeper 放行（两种方法）

**方法 A — 右键打开（推荐，macOS 13 及大多数场景）**

1. 在 `Applications` 里找到 `ClipVault.app`。
2. 按住 `Control` 点击（或右键）图标 → 选择「打开」。
3. 弹窗里再次点「打开」。此后系统记住该选择，正常双击即可。

**方法 B — 系统设置放行**

1. 双击 `ClipVault.app`，让系统弹出「无法打开」提示，先关掉。
2. 打开「系统设置 → 隐私与安全性」，滚动到底部的「安全性」区域。
3. 会看到「已阻止 ClipVault…」的一行，点右侧「仍要打开 / Open Anyway」。
4. 再次确认「打开」。

> macOS 14+ Apple Silicon 上如果以上两种方法都不出现放行入口，请用本文件顶部的 `xattr -d com.apple.quarantine` 命令。

---

## Accessibility 首次授权引导

ClipVault 的「回贴到当前光标」功能——`Enter` 选中回贴、`⌘1–9` 序号直选回贴、双击列表行回贴——**全部**依赖 macOS **Accessibility（辅助功能）** 权限来模拟 ⌘V 按键。**写系统剪贴板本身不需要该权限**（`NSPasteboard.setString` 始终会执行），但按键模拟必须有授权，未授权时只能回退到「内容已在剪贴板，请手动 ⌘V」。

### 打包后的 `.app`（正式使用）

1. 首次触发回贴时，ClipVault 面板顶部出现 **Accessibility 横幅**，点「打开系统设置」深链跳转。
2. 在 **「系统设置 → 隐私与安全性 → 辅助功能」** 列表里找到 **ClipVault**，打开右侧开关。
   - 首次可能需要点 🔓 解锁并输入密码 / 指纹。
3. **无需重启**——开关打开后下次回贴自动生效（每次回贴前用 `AXIsProcessTrusted()` 重新校验，防止系统升级后 TCC 缓存失效静默丢权）。

**未授权时** 浏览、搜索、加标签、加备注、Pin 置顶、访客模式、加密区**全部可用**；仅「自动回贴」不可用，此时 ClipVault 会**先把内容写入剪贴板**，再明确提示「已复制到剪贴板，请手动 ⌘V 粘贴；开启「辅助功能」后可自动回贴」，你随时可以手动 ⌘V 拿到内容——所以**未授权不是死路**。

**焦点归还**：面板是普通窗口，唤出时 macOS 会把键盘焦点交给 ClipVault（你正在编辑的应用会暂时失去焦点）。因此面板隐藏时——无论回贴成功还是按 `Esc` 放弃——ClipVault 都会**显式把焦点交还唤出面板前的那个应用**（热键与托盘呼出都会先记下它），并且回贴前会**等到它真的回到前台**再模拟 `⌘V`。所以回到编辑器 / 浏览器后光标还在你原来那个输入框里，无需再点一次。

**面板怎么关、怎么拖**：`Esc`、回贴（成功即自动收起）、以及**点击面板之外**都会收起面板。前两种是 ClipVault 主动隐藏，走上面的「焦点归还」；**点击面板之外不归还焦点**——那次点击本身已经把激活权交给了你点的对象（别的应用、桌面……），再抢回来等于跟你刚做的选择对着干。面板没有标题栏，**顶部那条搜索栏整条就是拖拽区**，按住空白处即可把面板拖到任意位置（搜索框内部仍照常选词、输入）。

### 开发期（`cargo tauri dev`）—— 关键区别

`cargo tauri dev` 跑的是 debug 版可执行文件，macOS 把 Accessibility 授权**绑定到具体的可执行文件路径 + 签名**上——所以**需要授权的不是 ClipVault 本体，而是启动它的宿主进程**（通常是你的**终端 App**：Terminal / iTerm2 / Warp / VS Code 等，因为 debug 二进制是终端 fork 出来的）。

正确授权方式：

1. 触发一次回贴，面板会弹 Accessibility 横幅。
2. 手动打开 **「系统设置 → 隐私与安全性 → 辅助功能」**。
3. 在列表里找你**正在用的终端**（点 **`+`** 添加 `/Applications/Utilities/Terminal.app` 或 `/Applications/iTerm.app` 等），打开它的开关。
4. 不用重启，回到终端按 `cargo tauri dev` 重新启动 ClipVault 即可。

容易踩的坑：
- **找 ClipVault → 找不到**：dev 期不在 ClipVault 名字下找，要在终端名字下找。
- **dev 二进制路径变化**：每次 clean rebuild / 切换 `target/` 偶尔会让授权失效，重新勾一下终端的开关即可；**打包后的 `.app` 没有这个问题**（路径固定 + ad-hoc 签名稳定）。

### 验证是否已授权

- 真的授权了 → 回贴成功后**目标应用**会收到内容，ClipVault 面板**自动隐藏**。
- 误以为授权了但实际没 → 回贴后 ClipVault **面板不隐藏**（因为后端会重新 `window.show()`），**横幅 + toast 再次出现**告诉你「已复制到剪贴板」。

更直接的命令验证（需要 SIP 关闭的环境才能读到 TCC.db，否则跳过）：

```bash
sqlite3 "$HOME/Library/Application Support/com.apple.TCC/TCC.db" \
  "SELECT client,auth_value FROM access WHERE service='kTCCServiceAccessibility';" 2>/dev/null \
  || echo "TCC.db 受 SIP 保护,直接看系统设置界面核对即可"
```

`auth_value=2` 表示已授权。读不到很正常，**直接看系统设置界面最稳**。

### 仍不生效时的排障

1. 确认**给对了进程**——dev 期是终端、不是 ClipVault。
2. 在辅助功能列表里**关掉再打开**该进程开关。
3. 完全退出 ClipVault（菜单栏 → 退出）再重开。
4. 重置该项后重新授权（打包后）：
   ```bash
   tccutil reset Accessibility com.clipvault.app
   ```

### 安全边界披露（请务必阅读）

授予 Accessibility 权限后，**任何应用在系统 TCC 权限模型下技术上都能模拟任意键盘事件**——这是 macOS 权限边界本身的性质，不是 ClipVault 特有的。就 ClipVault 而言：

- 代码路径**仅模拟 ⌘V** 一个组合键用于回贴，不记录、不模拟其他按键，不读取其他应用内容。
- 请仅在**受信任的个人环境**下运行本应用。
- 在**企业 MDM 管控**的 Mac 上安装前，请先确认组织安全策略允许具备 Accessibility 权限的第三方（未 notarized）应用。

---

## 隐私

ClipVault 默认全部离线，不做云同步、不上传任何数据（见下方「网络出口审计」的静态验证）。但请理解本地存储的安全边界：

### 数据存储位置

- SQLite 数据库：`$HOME/Library/Application Support/ClipVault/clipvault.db`

### 非加密条目的安全边界（默认）

- 非加密区条目以**明文**存储在上述 SQLite 文件中，安全边界是 **macOS 用户登录会话 + 用户家目录的文件系统权限**。
- ClipVault **不使用** SQLCipher，也**不使用** `NSFileProtectionComplete`。原因：`NSFileProtectionComplete` 会让数据在设备锁定时不可读，与「随时检索历史剪贴板」的核心场景冲突。
- 因此：**在未启用 FileVault 的设备上，能物理访问磁盘的人即可读取这些明文条目。**
- 强烈建议开启 **FileVault 全盘加密**（系统设置 → 隐私与安全性 → FileVault）作为整机层面的保护。
- 「加密区」（AES-GCM）是针对**敏感条目的纵深防御**，而非对整个数据库加密。

### 加密区与 Keychain 密钥（不可导出）

- 加密区条目用 **AES-256-GCM** 加密后存储，数据密钥由 macOS **Keychain** 托管（`service = com.clipvault.data_key`）。
- **该 Keychain 密钥不可导出**。这意味着：如果你**更换机器**（换新 Mac、或 Time Machine 恢复未能带回 Keychain 项），旧的加密条目将**永久无法解密**——它们会在 UI 上显示「无法解密」徽标（`is_decryptable=0`），并提供批量清除入口。
- 这是「零密码、首次启动零摩擦」设计（KTD4）的代价。元数据（标签、类别、备注、时间戳）仍为明文可见；仅密文内容不可恢复。
- 未来可能加入「用户口令派生备份密钥」作为可选项，MVP 不包含。

---

## 开发

前置：macOS 13+、Rust（edition 2024 工具链）、Node.js（前端）、`cargo install tauri-cli --version "^2.0" --locked`。

```bash
# 起开发模式（空浮层 + 菜单栏 + 全局快捷键）
cargo tauri dev

# 全量测试（app + clipvault-core）
cargo test --workspace

# Lint / 格式
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## 打包 / 发布

```bash
# 一键：前端构建 → cargo tauri build → ad-hoc 签名 → 打 DMG
packaging/build.sh

# 或分步执行：
cargo tauri build                                  # 产 target/release/bundle/macos/ClipVault.app（+ dmg/）
packaging/codesign.sh                              # ad-hoc 签名 + 验证 Signature=adhoc
packaging/dmg.sh                                   # 用 create-dmg / hdiutil 打 DMG
```

产物：
- `.app` → `target/release/bundle/macos/ClipVault.app`
- `.dmg` → `target/release/bundle/dmg/ClipVault.dmg`

### universal2（Intel + Apple Silicon 通吃）发布步骤

发布正式版本时构建 universal2 二进制：

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo tauri build --target universal-apple-darwin
# 产物在 target/universal-apple-darwin/release/bundle/ 下
```

`packaging/build.sh` 会在两个架构 target 均已安装时自动走 universal2，否则退回当前机器的原生架构（并打印提示）。

---

## 网络出口审计（R13）

ClipVault 承诺默认不联网。静态审计证据（本仓库当前状态）：

```bash
# 依赖树中不存在 HTTP 客户端 / 服务端
cargo tree | grep -E 'reqwest|hyper'          # → 无输出

# 源码中不存在对外 socket 调用
grep -rE 'reqwest::|hyper::|TcpStream|std::net::' app/src clipvault-core/src   # → 无输出
```

两条检查均为**零命中**：无 `reqwest` / `hyper` 依赖，无 `TcpStream` / `std::net` / `tokio::net` 调用点。ClipVault 仅通过 `NSPasteboard`、Keychain 与本地 SQLite 文件工作，无任何外发 socket。

