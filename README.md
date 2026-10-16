# ClipVault — 剪贴板管理器（macOS / Windows）

ClipVault 常驻菜单栏（macOS）/ 通知区（Windows），持续记录你复制过的内容（文本 / 图片 / 文件 / RTF），通过全局快捷键（macOS 默认 `⌥⌘V`、Windows 默认 `Ctrl+Alt+V`）呼出 Raycast 风格命令面板，即时搜索、按 `Enter` 回贴到当前光标。支持自动类别（URL / Email / 路径 / 代码 / JSON / 纯文本）、自由标签、备注、逐条 Pin 置顶、AES-GCM 加密区与访客模式。**默认完全离线，不连接网络、不上传任何数据。**

单一事实来源（需求 / 设计 / 验收）：macOS 见 [`docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md`](./docs/plans/2026-09-24-0931-feat-clipvault-mvp-plan.md)，Windows 见 [`docs/plans/2026-10-13-1716-feat-windows-platform-support-plan.md`](./docs/plans/2026-10-13-1716-feat-windows-platform-support-plan.md)。

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

### macOS

1. 从 DMG 安装：双击对应架构的 DMG（Apple Silicon → `ClipVault_<版本>_arm64.dmg`，Intel → `ClipVault_<版本>_x64.dmg`），把 `ClipVault.app` 拖入 `Applications`。
2. 首次启动会被 Gatekeeper 拦截（因为是 ad-hoc 签名）。按下面「Gatekeeper 放行」放行一次即可，以后正常双击。
3. 首次运行会引导你授予 **Accessibility 权限**（用于回贴，见下文）。

### Windows（10 22H2 / 11，x64）

1. 运行 `ClipVault_<版本>_x64-setup.exe`（NSIS 安装包，默认**按当前用户**安装，不需要管理员权限）。
2. 首次运行会被 **SmartScreen** 拦一下：安装包**未做 Authenticode 签名**（需商业代码签名证书）。
   点「更多信息」→「仍要运行」即可放行；Windows 11 上这个入口在小字链接里。
3. **不需要任何系统授权**，装完即用：Windows 侧靠 `SendInput` 注入按键，没有 macOS 那种
   「辅助功能」授权门槛，也没有授权横幅。全局快捷键是 `Ctrl+Alt+V`（面板内其余快捷键同理：
   `Ctrl+E` 备注、`Ctrl+P` 置顶、`Ctrl+O` 打开 URL……界面上的提示条会按平台显示）。

#### Windows 已知限制（请先读）

- **不承诺 ARM64**：可编译，但未做验收（Windows plan 的 Scope Boundaries）。
- **目标进程以管理员（高完整性）运行时无法自动回贴**：`SendInput` 受 UIPI 限制，注入会被系统
  静默丢弃 —— 这是 Windows 的安全设计，ClipVault 不做绕过。此时降级为「内容已写入剪贴板，
  请手动 `Ctrl+V`」，不是报错。
- **不产生 color 类型条目**：Win32 剪贴板没有颜色类型（颜色通常以文本或私有格式出现），
  采集侧永远不会构造 `ContentKind::Color`。界面上 color 行的渲染分支保留不动（它服务历史库
  与跨平台共享的库文件）。
- **加密区的密钥不跨平台互通**：见下文「加密区与密钥托管」。

### Gatekeeper 放行（两种方法，仅 macOS）

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

## Accessibility 首次授权引导（**仅 macOS**）

> **Windows 用户请跳过本节**：Windows 侧用 `SendInput` 注入按键，**不需要任何系统授权**，
> ClipVault 也不会在 Windows 上显示任何授权横幅或系统设置深链（Windows plan 的 RW8）。
> 下面讲的全是 macOS 的 TCC 授权模型。

ClipVault 的「回贴到当前光标」功能——`Enter` 选中回贴、`⌘1–9` 序号直选回贴、双击列表行回贴——**全部**依赖 macOS **Accessibility（辅助功能）** 权限来模拟 ⌘V 按键。**写系统剪贴板本身不需要该权限**（`NSPasteboard.setString` 始终会执行），但按键模拟必须有授权，未授权时只能回退到「内容已在剪贴板，请手动 ⌘V」。

### 打包后的 `.app`（正式使用）

1. 首次触发回贴时，ClipVault 面板顶部出现 **Accessibility 横幅**，点「打开系统设置」深链跳转。
2. 在 **「系统设置 → 隐私与安全性 → 辅助功能」** 列表里找到 **ClipVault**，打开右侧开关。
   - 首次可能需要点 🔓 解锁并输入密码 / 指纹。
3. **无需重启**——开关打开后下次回贴自动生效（每次回贴前用 `AXIsProcessTrusted()` 重新校验，防止系统升级后 TCC 缓存失效静默丢权）。

**未授权时** 浏览、搜索、加标签、加备注、Pin 置顶、访客模式、加密区**全部可用**；仅「自动回贴」不可用，此时 ClipVault 会**先把内容写入剪贴板**，再明确提示「已复制到剪贴板，请手动 ⌘V 粘贴；开启「辅助功能」后可自动回贴」，你随时可以手动 ⌘V 拿到内容——所以**未授权不是死路**。

**焦点归还**：面板是普通窗口，唤出时 macOS 会把键盘焦点交给 ClipVault（你正在编辑的应用会暂时失去焦点）。因此面板隐藏时——无论回贴成功还是按 `Esc` 放弃——ClipVault 都会**显式把焦点交还唤出面板前的那个应用**（热键与托盘呼出都会先记下它），并且回贴前会**等到它真的回到前台**再模拟 `⌘V`。所以回到编辑器 / 浏览器后光标还在你原来那个输入框里，无需再点一次。

**面板怎么关、怎么拖**：`Esc`、回贴（成功即自动收起）、以及**点击面板之外**都会收起面板。前两种是 ClipVault 主动隐藏，走上面的「焦点归还」；**点击面板之外不归还焦点**——那次点击本身已经把激活权交给了你点的对象（别的应用、桌面……），再抢回来等于跟你刚做的选择对着干。面板没有标题栏，**顶部那条搜索栏整条就是拖拽区**，按住空白处即可把面板拖到任意位置（搜索框内部仍照常选词、输入）。

**面板开在哪块屏**：多显示器下，每次唤出都会把面板放到**当前有键盘焦点的那块屏**的工作区正中央（macOS 取键窗口所在屏、Windows 取前台窗口所在屏；从菜单栏 / 通知区点击唤出时改取鼠标所在屏；都取不到时回退主屏）。工作区 = 屏幕扣掉菜单栏 / Dock / 任务栏，所以面板不会压在这些系统元素上。两点要知道：**每次唤出都会重新定位**，手动拖动的位置只影响这一次；以及 macOS 上如果那块屏正被全屏应用占着，面板要等你退出全屏才看得见（与改动前一致）。

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

**macOS**：授予 Accessibility 权限后，**任何应用在系统 TCC 权限模型下技术上都能模拟任意键盘事件**——这是 macOS 权限边界本身的性质，不是 ClipVault 特有的。就 ClipVault 而言：

- 代码路径**仅模拟 ⌘V** 一个组合键用于回贴，不记录、不模拟其他按键，不读取其他应用内容。
- 请仅在**受信任的个人环境**下运行本应用。
- 在**企业 MDM 管控**的 Mac 上安装前，请先确认组织安全策略允许具备 Accessibility 权限的第三方（未 notarized）应用。

**Windows**：没有需要预先授予的系统授权（`SendInput` 不需要授权），但仍有一条系统边界：注入**只能到达同权限级别的进程**，对以管理员（高完整性）运行的目标会被 UIPI 静默丢弃 —— ClipVault 因此降级为「写入剪贴板 + 提示手动 `Ctrl+V`」，不做绕过。反过来同样成立：ClipVault 以普通用户权限运行，不会也无法注入到提权进程中。

---

## 隐私

ClipVault 默认全部离线，不做云同步、不上传任何数据（见下方「网络出口审计」的静态验证）。但请理解本地存储的安全边界：

### 数据存储位置

- macOS：`$HOME/Library/Application Support/ClipVault/clipvault.db`
- Windows：`%APPDATA%\ClipVault\clipvault.db`（即 `C:\Users\<你>\AppData\Roaming\ClipVault\`）

两边的库文件结构相同（同一份 schema），但**数据密钥不互通**，见下文。

诊断日志（`clipvault.log`）落在同目录，单文件 ≤ 1 MiB，轮转后保留一个 `.1` 备份，
总占用 ≤ 2 MiB。仅记诊断事实（级别、模块、错误文本、条目 kind/mime/字节数/指纹前缀），
**不**记剪贴板正文、文件路径或图片字节。安全边界与 `clipvault.db` 相同：
明文落盘，依赖会话与文件系统权限。托盘「查看日志…」或设置面板「诊断日志」区可访问。

### 非加密条目的安全边界（默认）

- 非加密区条目以**明文**存储在上述 SQLite 文件中，安全边界是**操作系统用户登录会话 + 用户家目录 / `%APPDATA%` 的文件系统权限**。
- ClipVault **不使用** SQLCipher，也**不使用** `NSFileProtectionComplete`（macOS）。原因：`NSFileProtectionComplete` 会让数据在设备锁定时不可读，与「随时检索历史剪贴板」的核心场景冲突。
- 因此：**在未启用磁盘加密的设备上（macOS 的 FileVault / Windows 的 BitLocker），能物理访问磁盘的人即可读取这些明文条目。**
- 强烈建议开启 **FileVault 全盘加密**（系统设置 → 隐私与安全性 → FileVault）或 **BitLocker** 作为整机层面的保护。
- 「加密区」（AES-GCM）是针对**敏感条目的纵深防御**，而非对整个数据库加密。

### 加密区与密钥托管（不可导出）

- 加密区条目用 **AES-256-GCM** 加密后存储，数据密钥交给**系统**托管：macOS 是 **Keychain**
  （`service = com.clipvault.data_key`），Windows 是**凭据管理器**（`TargetName = ClipVault/data_key`，
  `CRED_TYPE_GENERIC` + `CRED_PERSIST_LOCAL_MACHINE`）。
- **该密钥不可导出**。这意味着：如果你**更换机器**（换新 Mac、或 Time Machine 恢复未能带回 Keychain 项），旧的加密条目将**永久无法解密**——它们会在 UI 上显示「无法解密」徽标（`is_decryptable=0`），并提供批量清除入口。
- **密钥不跨平台、不跨机器互通**：macOS 与 Windows 各自托管自己的密钥，两边**不共享**加密区内容 ——
  即便把 `clipvault.db` 从一台机器拷到另一台，其中的加密条目也解不开（这正是设计意图：密钥从不离开
  系统凭据库，也就无法随库文件一起泄露）。非加密条目与元数据不受影响。
- 这是「零密码、首次启动零摩擦」设计（KTD4）的代价。元数据（标签、类别、备注、时间戳）仍为明文可见；仅密文内容不可恢复。
- 未来可能加入「用户口令派生备份密钥」作为可选项，MVP 不包含。

---

## 快捷键自定义（设置 → 快捷键）

主面板底部那条快捷键提示条已移除（U5），所有快捷键改在「设置」面板里集中管理与重绑。

入口：**主面板左下角的「设置」按钮**（或面板内按 `⌘,` / `Ctrl+,`）。

设置内容（透明度滑块 + 快捷键列表）比面板高时**在面板内滚动**，标题栏与底部提示常驻；每次打开都回到顶部，点「重绑」进入录制态的那一行会自动滚入可见区。拖动透明度滑块时，面板背景与设置遮罩一起变化——不用先关掉设置就能看出效果。

### 可重绑的快捷键

- **全局**（仅 macOS / Windows；Linux 上整组隐藏）：`OpenPanel` 唤出主面板（默认 `⌥⌘V` / `Ctrl+Alt+V`）。
- **面板内**（13 项）：`Paste`、`EditNote`、`TogglePin`、`MoveToVault`、`OpenUrl`、`OpenVault`、`ToggleGuestMode`、`SkipOnce`、`OpenSettings`、`Close`、`MoveUp`、`MoveDown`。
- **不在范围**：`⌘1-9` 序号直选（用户范围协商结果未选）；`⌥` 双击「本次不记录」修饰键（watcher 侧机制不同）。

### 重绑流程

1. 在设置 → 快捷键列表里点对应行的「重绑」按钮。
2. 该行进入录制态（`<kbd>` 闪烁），全局监听 `keydown`。
3. 按下任一非修饰键单键（`Escape` 取消；只按 `Cmd` / `Shift` / `Alt` / `Ctrl` 视为无效，等下一键）。
4. 后端 `set_hotkey` IPC 持久化到 `settings` 表 `hotkeys` JSON 行 + 重新注册全局热键（若改的是 `OpenPanel`）。
5. 成功时行立即更新；**录到的组合已被另一条快捷键占用**时不直接写入，而是弹确认框：「`⌘E` 已被「编辑备注 / 标签」占用」并说明后果 —— 点「强行换绑」则组合归本行、原来占着它的那条变成「未绑定」（若被解绑的是全局热键，弹框会额外警告：面板只剩托盘 / 菜单栏图标能唤出），点「取消」或按 `Esc` 则维持原绑定；被系统或别的程序占用的组合（Carbon / `RegisterHotKey` 返回非 0）仍然是 toast「已被占用」并保持原绑定。
6. 重启 ClipVault 仍然生效（从 `settings` 表恢复，「未绑定」也一并保留）。

### 字形对照

- macOS 显示 `⌘⇧V` 风格（U+2318 ⌘、U+21E7 ⇧、U+21A5 ↩ 等）。
- Windows 通过 `platform.ts` 的 `keys()` 翻译为 `Ctrl+Shift+V` 纯文本。
- Linux 仍按 macOS 字形渲染（不进行平台翻译），但全局热键组隐藏。

存储后端是 `settings` 表新增 `key=hotkeys` 单行 JSON（与 `guest_mode` / `panel_opacity` 同表同 schema），见 plan `docs/plans/2026-10-14-0927-feat-shortcut-customization-plan.md`。

---

## 开发

前置：Rust（edition 2024 工具链）、Node.js（前端）、`cargo install tauri-cli --version "~2.11" --locked`。

- **macOS**：13+，另需 Xcode 命令行工具（`xcode-select --install`）。
- **Windows**：10 22H2 / 11（x64），另需 **MSVC 工具链**（Visual Studio Build Tools 里勾「使用 C++
  的桌面开发」）与 **WebView2 运行时**（Windows 11 自带；Windows 10 通常随 Edge 一起装好）。
  开发期**不需要**任何系统授权（对比 macOS 那节辅助功能授权）。

```bash
# 起开发模式（空浮层 + 菜单栏 / 托盘 + 全局快捷键）
cargo tauri dev

# 全量测试（app + clipvault-core）
cargo test --workspace

# Lint / 格式
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

## 打包 / 发布

### macOS（DMG）

两个架构**分别**打（不再做 universal2）：universal2 是把两份二进制 `lipo` 合并成一个 `.app`，对本项目只会让包体积近似翻倍；拆开后每个架构一个 DMG，用户按自己的机器下。（两种做法都得编两遍，matrix 只是让它并行。）

```bash
packaging/build.sh arm64     # Apple Silicon（aarch64-apple-darwin）
packaging/build.sh x64       # Intel（x86_64-apple-darwin）
packaging/build.sh           # 不传参数 = 当前机器架构（开发日常）
```

每个架构一条命令走完：前端构建 → `cargo tauri build --target <triple>` → ad-hoc 签名 → 打 DMG。

产物（`<triple>` = `aarch64-apple-darwin` 或 `x86_64-apple-darwin`，`<arch>` = `arm64` 或 `x64`）：
- `.app` → `target/<triple>/release/bundle/macos/ClipVault.app`
- `.dmg` → `target/<triple>/release/bundle/dmg/ClipVault_<版本>_<arch>.dmg`

或分步执行（下面以 arm64 为例）：

```bash
cargo tauri build --target aarch64-apple-darwin --bundles app   # 产 target/aarch64-apple-darwin/release/bundle/macos/ClipVault.app
packaging/codesign.sh target/aarch64-apple-darwin/release/bundle/macos/ClipVault.app   # ad-hoc 签名 + 验证 Signature=adhoc
packaging/dmg.sh target/aarch64-apple-darwin/release/bundle/macos/ClipVault.app ClipVault_0.1.0_arm64.dmg   # create-dmg / hdiutil（输出名随便取，build.sh 会带版本号+架构）
```

（`--bundles app` 只出 `.app`：`tauri.conf.json` 的 `targets: "all"` 会让 bundler 自己也产一个 DMG，
和我们用 `dmg.sh` 打的混在同一个目录里，架构后缀写法还不一样。DMG 统一交给 `dmg.sh`。）

交叉编译：Apple Silicon 上编 x86_64 需要 `rustup target add x86_64-apple-darwin`（Intel 上编 arm64 同理）。Apple SDK 自带两份架构 slice，不需要额外工具链。

### Windows（NSIS 安装包）

```powershell
powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1
```

一条命令走完：安装前端依赖 → `cargo tauri build --bundles nsis` → 打印产物路径。
产物 → `target\release\bundle\nsis\ClipVault_<版本>_x64-setup.exe`。

只打 NSIS、**不打 MSI**：`targets: "all"` 在 Windows 上会连带给需要 WiX 工具链的 MSI，
那是这条链路上最容易失败的一环，而 MSI 对「按用户安装的托盘小工具」没有额外价值。
安装包**未做 Authenticode 签名**，首次运行会被 SmartScreen 拦一下（放行步骤见「安装 → Windows」）。

### CI 发版

走 `.github/workflows/package.yml`：macOS job 用 matrix 并行跑这两个架构（产物 Artifact 分别是 `ClipVault-macos-arm64` / `ClipVault-macos-x64`），Windows job 跑 `packaging\build-windows.ps1` 出 NSIS 安装包，`release` job 把两个 DMG 与 `.exe` 一起挂到 `v*` tag 的 Release 上。

> **版本约束**：`app/Cargo.toml` 的 `tauri` 与 `frontend/package.json` 的 `@tauri-apps/api` 必须停在**同一个 major.minor**——`cargo tauri build` 启动时会交叉校验，不一致直接报 `Found version mismatched Tauri packages` 并中止（Windows job 就栽在这儿）。所以两边都写成 `~2.11`（`^` 会浮到 2.12+，而 npm 侧锁的是 2.11.1），升级 tauri 时两处一起抬。`Cargo.lock` 因此纳入版本控制：CI 与开发机用同一棵依赖树。

---

## 网络出口审计（R13）

ClipVault 承诺默认不联网。静态审计证据（本仓库当前状态）：

```bash
# 依赖树中不存在 HTTP 客户端 / 服务端
cargo tree | grep -E 'reqwest|hyper'          # → 无输出

# 源码中不存在对外 socket 调用
grep -rE 'reqwest::|hyper::|TcpStream|std::net::' app/src clipvault-core/src   # → 无输出
```

两条检查均为**零命中**：无 `reqwest` / `hyper` 依赖，无 `TcpStream` / `std::net` / `tokio::net` 调用点。ClipVault 只碰本地剪贴板、系统凭据库与本地 SQLite 文件，没有任何外发 socket：

- **macOS**：`NSPasteboard`、Keychain（`security-framework`）、CGEvent；
- **Windows**：Win32 剪贴板 API、凭据管理器（`CredReadW` / `CredWriteW`）、`SendInput` —— 新增的
  `windows` crate 只用于这些，不引入任何网络能力。该目标的整树编译与 lint 由 CI 的 `windows` job
  覆盖（本地开发机是 macOS，`cfg(target_os = "windows")` 的分支根本不参与编译）。

