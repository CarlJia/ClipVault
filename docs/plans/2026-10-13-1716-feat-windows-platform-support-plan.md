---
title: "Windows 平台支持 - Plan"
type: feat
date: 2026-10-13
topic: windows-platform-support
artifact_contract: ce-unified-plan/v1
product_contract_source: user-directed
execution: code
---

# Windows 平台支持 - Plan

## Goal Capsule

**Objective.** 让 ClipVault 在 Windows 10/11 上成为与 macOS 版**功能对等**的客户端:持续采集剪贴板
(文本/图片/文件/RTF/颜色)、`Ctrl+Alt+V` 全局呼出面板、选中后自动回贴到刚才的前台应用、FTS5 搜索 /
标签 / 备注 / Pin 全可用、AES-256-GCM 加密区由 Windows 凭据管理器托管密钥,并产出可安装的 NSIS 安装包。

**Means.** 复用既有扁平化架构:平台无关的决策层(`pasteback::perform_paste` + `PasteBackend` /
`AccessibilityChecker` 两个 trait、`capture::Captured`、`storage`/`search`/`thumbnail`/`privacy`)
**一行不改**,只为每个 `#[cfg(not(target_os = "macos"))]` 降级桩补一份 `#[cfg(target_os = "windows")]`
实现。原生绑定用 `windows` crate 0.61(已在依赖树内),与 macOS 侧 objc2 家族对称且同样只在
对应 target 下编译。`KTD10`–`KTD17`。

**Product authority.** 本 plan **显式取代** MVP plan 与多类型 plan 中「跨平台客户端 out of scope」
的范围声明(`2026-09-24-0931-...md:19,207`、`2026-09-27-1038-...md:161`)。macOS 行为是**回归基线**,
不得改变;Windows 是新增的第二个目标平台。Linux 不在本 plan 范围内。

**Stop conditions.** 当 `U1`–`U9` 各自 Verification 全项通过、`AEW1`–`AEW8` 在 Windows 10/11 真机上
跑通、且 macOS 既有测试基线(app 94 + core 22)不退化时,视为可交付;在此之前不得声明 Windows 可用。

---

## Product Contract

> **Product Contract preservation:** 现有 R1–R17 与 AE1–AE8(及多类型 plan 的 AE9–AE12)**语义不变**,
> 但**作用域从「macOS 专属」扩展为「macOS 与 Windows 双平台」**。本 plan 新增 `RW1`–`RW9` 描述
> Windows 侧的平台等价物与差异,新增 `AEW1`–`AEW8` 作为 Windows 真机验收清单,不改动任何既有 ID。

### Summary

Windows 版与 macOS 版共享同一套数据模型、同一套 UI、同一套加密算法,差异**只**落在四件事上:
剪贴板读写、全局热键、按键注入、密钥托管。前三件是 Win32 API 的直接映射,第四件由 Windows 凭据
管理器承担 macOS Keychain 的角色。除此之外,用户看到的产品形态(常驻托盘 + 快捷键唤出 Raycast
风格浮层 + 搜索 + 回贴)在两端一致。

### Problem Frame

仓库当前对 Windows 的实现状态是**「编译得过的空壳」**:`#![cfg_attr(not(debug_assertions),
windows_subsystem = "windows")]`(`app/src/main.rs:17`)表明作者早就预留了 Windows 目标,所有平台相关
代码也都规规矩矩写在 `#[cfg]` 分支里,但**非 macOS 的那一半全部是 no-op 或 `Err("仅支持 macOS")`**:
`capture::poll_first_non_empty` 返回 `None`(空转,一条都记不下来)、`hotkey::register` 什么都不做
(没有唤出入口)、`pasteback` 只有 stub(回贴报错)、`KeychainKeyProvider` 类型在非 macOS 下根本不存在
(加密区整块不可用)。前端则 100% 绑定 macOS 语汇:快捷键提示写死 `⌘`、4 处图标用了 SF Symbols 私有区
字形(Windows 上是豆腐块)、授权深链是 `x-apple.systempreferences:` scheme。

这不是「写得不好」,而是**产品范围本来只声明了 macOS**。本 plan 把这个范围扩到 Windows,于是上述
空壳必须逐个填成真实现。

### Key Decisions

1. **Windows 是 macOS 的等价移植,不是重新设计。** 决策层/存储层/UI 层不动,只填平台实现。
   `(user-directed: user-approved — chosen over 重构统一抽象层: 现有 trait 接缝足够,PasteBackend /
   AccessibilityChecker 已经就是平台抽象,再抽一层是白付迁移成本)`。
2. **原生绑定用 `windows` crate 而非 `windows-sys` 或第三方封装(如 `clipboard-win` / `arboard`)。**
   `(KTD10)`。
3. **全局热键用 `RegisterHotKey` + 专用线程消息泵,不用 `tauri-plugin-global-shortcut`。**
   这是 KTD2「不引入任何 shortcut 插件」约束在 Windows 上的延续。`(KTD11)`。
4. **热键组合定为 `Ctrl+Alt+V`。** `(user-directed: user-approved — chosen over Win+Shift+V /
   Ctrl+Shift+V: 与 macOS 的 ⌥⌘V 同为「双修饰键+V」语义,且不撞 Win+V 系统剪贴板历史与
   Ctrl+Shift+V 编辑器「粘贴为纯文本」)`。
5. **文件类型在 Windows 上做「原生」而非照搬 macOS 的退化路径。** macOS 侧 `write_clipboard` 的
   `File` 分支把路径拼成换行文本(受 objc2 0.3 未暴露 `writeObjects` 所限,`pasteback.rs:501-516`
   自己注释承认)。Windows 无此限制:`CF_HDROP` + `DROPFILES` 是正解,不做退化。`(KTD15)`。
6. **密钥托管用 Windows 凭据管理器(Credential Manager)。** 它是 Keychain generic password 的直接
   对应物:同样按 (target, user) 寻址、同样由系统加密、同样不需要自管密钥文件。`(KTD13)`。
7. **交付方式为分单元增量交付。** `(user-directed: user-approved — chosen over 一次性大改动:
   9 个单元各自可独立验证,中途可调整)`。

### Actors

- **Windows 主用户.** Windows 10/11 上希望长期管理剪贴板历史的个体使用者。与 macOS 主用户诉求一致。
- **Windows 系统.** 通过剪贴板 API(`OpenClipboard` / `SetClipboardData` 家族)提供内容,通过
  `SendInput` 接受按键注入。**Windows 没有 Accessibility 授权这一前置门槛** —— 这是与 macOS 最重要的
  产品差异(见 R3 与 AEW3)。

### Key Flows

#### F1. 监听与捕获

macOS 侧每 100ms 读 `NSPasteboard.changeCount`,Windows 侧读 `GetClipboardSequenceNumber()` ——
两者都是「单调递增的剪贴板序列号」,语义等价,R1 的 < 200ms 延迟约束同样满足。差异在读取内容:
Windows 一次 `OpenClipboard` 是**进程独占的临界区**,期间不能阻塞(否则锁死整机剪贴板),
所以采集必须在锁内完成全部读取并在任何 return 路径上 `CloseClipboard`。

#### F2. 回贴

`perform_paste` 的编排(写剪贴板 → 检查授权 → 还焦点 → 注入 → 比对前台)完全复用。平台差异:
- macOS 注入「V + Command flag」2 个事件;Windows 必须注入完整 4 事件序列
  (Ctrl↓ / V↓ / V↑ / Ctrl↑),并在注入前**等待物理修饰键释放**,否则用户仍按着的 Alt 会把
  `Ctrl+V` 污染成 `Ctrl+Alt+V`。
- macOS 还焦点靠 `activateWithOptions`;Windows 受**前台锁**限制:只有当前拥有前台权的进程才能
  `SetForegroundWindow`,所以「发出请求 + 有界等待」要重写,时间常数需在真机重新标定。
- 前台应用标识:macOS 用 bundle id,Windows 用**进程 exe 全路径**(小写)作为等价物。

#### F3. 加密区

密钥来源从 Keychain 换成凭据管理器,`VaultCipher` / `Encryptor` / `Decryptor` / `salted_content_hash` /
`verify_decryptable_on_startup` 全部复用。跨平台**不共享**密钥(各存各的系统凭据库),但**共享密码学**
(同 AES-256-GCM、同 12 字节 nonce、同 v1 密文布局),因此同一份 SQLite 库在两台机器间手工拷贝后
**互相解不开** —— 这是刻意的,必须在 README 写明(见 U9)。

### Requirements

#### 平台等价(Platform parity)

- **RW1.** Windows 上持续采集剪贴板变化,支持 text / image / file / rtf / color 五种 `ContentKind`,
  与 macOS 侧 `Captured` 契约完全一致(width/height/size_bytes/paths/resolved/rgba 字段同义)。
- **RW2.** Windows 上 `Ctrl+Alt+V` 全局呼出/切换主面板,走与 macOS 同一个入口 `tray::show_panel`
  (保证「先记录回贴目标、再显示面板」的顺序不漂移)。
- **RW3.** Windows 上选中条目回车/双击立即回贴到呼出面板前的那个前台窗口;失败时**降级而非报错**
  —— 内容始终先写入剪贴板,并按 `Pasted` / `WindowSwitched` / `ClipboardOnly` 三态给出对应 toast。
- **RW4.** Windows 上搜索 / 标签 / 类别 / 备注 / Pin / 访客模式 / 本次不记录 / 面板透明度全部可用
  (这些本就是平台无关实现,Windows 上不得因平台分支而失效)。
- **RW5.** Windows 上加密区可用:条目可移入加密区、密文以 AES-256-GCM 存储、解密后才回贴;
  数据密钥由 Windows 凭据管理器托管且不落明文磁盘。
- **RW6.** Windows 上 `⌘O 打开 URL` 等价物为 `Ctrl+O`,通过系统默认浏览器打开,且**沿用既有
  scheme 白名单**(仅 `http://` / `https://`)。
- **RW7.** Windows 上托盘图标在任何主题(浅色/深色任务栏)下可见,四态(监听中/访客/暂停/异常)
  有可观测的视觉或 tooltip 区分。

#### 平台差异(Platform divergence,须在 UI 与文档中如实表达)

- **RW8.** Windows 无「辅助功能授权」概念,`check_accessibility` 恒为已授权,**授权横幅与
  `no_accessibility` 降级路径在 Windows 上不得出现**,也不得出现指向 macOS 系统设置的文案或深链。
- **RW9.** 前端在 Windows 上显示 `Ctrl+X` 而非 `⌘X`,且所有图标字形在 Windows 默认字体下可正常
  渲染(不得使用 SF Symbols 私有区码点)。

### Acceptance Examples

> 以下为 **Windows 真机**验收(Windows 10 22H2 与 Windows 11 各跑一次)。既有 AE1–AE12 的语义在
> Windows 上同样适用,这里只列 Windows 特有的或需要重新确认的部分。

#### AEW1. 采集五种类型 (Covers RW1)
依次复制:一段文本 → `https://example.com/x` → 截图工具复制一张图 → 资源管理器复制两个文件 →
从 Word 复制一段带格式文本。面板「最近」tab 出现 5 条,`content_kind` 分别为 text/url 文本/image/
file/rtf,图片条目带 64×64 缩略图,文件条目显示文件名与路径。

#### AEW2. 序列号不因占用而漏采 (Covers RW1)
在另一程序持续占用剪贴板(如频繁复制的下载工具)的干扰下,ClipVault 不得卡死或崩溃;
`OpenClipboard` 失败即跳过本轮,下一轮(100ms 后)能正常采到。

#### AEW3. 全局热键 + 回贴闭环 (Covers RW2, RW3)
在记事本里光标落位 → 按 `Ctrl+Alt+V` → 面板弹出(记事本失焦)→ 选一条 → 回车 →
记事本重新获得焦点且内容已粘贴,面板消失,无 toast(即 `Pasted`)。

#### AEW4. 无授权门槛 (Covers RW8)
全新 Windows 用户态账户、未做过任何系统设置改动时,AEW3 直接成功;设置页**不出现**任何
授权横幅;`Ctrl+Alt+V` 之外的任何回贴也不应提示「需要授权」。

#### AEW5. 修饰键未释放时不污染 (Covers RW3)
按住 `Ctrl+Alt+V` 不放(热键触发后继续按住 Alt)→ 用方向键选条目 → 回车 →
粘贴结果必须是**纯文本条目内容**,不得是 `Ctrl+Alt+V` 组合的副作用。

#### AEW6. 目标窗口切换降级 (Covers RW3)
呼出面板后,用鼠标点击另一个程序使之前的目标窗口不再是前台 → 回车 →
内容已写入剪贴板,面板重新显示,toast 文案提示「目标窗口已切换,内容已写入剪贴板,请手动 Ctrl+V」。

#### AEW7. 文件与图片回贴 (Covers RW3)
- 复制两个文件 → 选中该条 → 在资源管理器窗口回车 → 目标目录出现两个文件副本(`CF_HDROP` 生效,
  而非粘出两行路径文本)。
- 复制一张图 → 选中 → 在画图/Mail 正文回车 → 图片粘贴成功(现代应用走注册格式 `PNG`,
  传统应用走 `CF_DIB`)。

#### AEW8. 加密区端到端 (Covers RW5)
选中一条 → `Ctrl+L` 移入加密区 → 面板显示「🔒 内容以 AES-256-GCM 加密」且正文不可见 →
重启应用后仍可解密 → 在目标应用回车可正常回贴明文 →
用 `credential manager` 工具(`cmdkey /list` 或凭据管理器 UI)能看到 `ClipVault/data_key` 条目
且其内容不是可直接读出的密钥文本 → 手工删除该凭据后重启,加密区条目显示为「不可解密」而**不崩溃**。

### Scope Boundaries

#### In this product

- Windows 10 22H2 / Windows 11(x64)。ARM64 Windows 不承诺(可编译但未验收)。
- NSIS 安装包(`.exe`)。MSI 不做(需额外拉 WiX,失败面大)。

#### Outside this product's identity

- **Linux.** 沿用范围外。X11/Wayland 两套剪贴板模型、全局热键方案差异、Secret Service 托管
  都不是本 plan 的工作。
- **Windows ARM64 与 Windows 7/8.** 不验收。
- **代码签名与 SmartScreen 信誉.** 不做 Authenticode 签名(需商业证书),安装包未签名,
  SmartScreen 会警告,README 如实说明。
- **跨平台密钥互通 / 库同步.** 见 F3。

### Dependencies

- `windows` crate 0.61(已在 `Cargo.lock` 内,由 tauri/wry/webview2-com 传递引入)。
- Windows 10 SDK 提供的 `RegisterHotKey` / `SendInput` / 剪贴板 / 凭据管理器 API(均为 Win32 稳定接口,
  无额外 SDK 安装要求)。
- `image` 0.25(已在依赖树):DIB ↔ PNG 转换与缩略图共用。
- 既有 `packaging/` 脚本与 tauri-cli(打 Windows 包用 `cargo tauri build --bundles nsis`)。

### Assumptions

- **A1.** `windows` crate 的 API 形态(结构体字段、`Result` 化返回值、`CLIPBOARD_FORMAT` newtype)
  已由编译期探针在 `x86_64-pc-windows-msvc` 目标上验证通过,不是凭记忆写的。见「验证契约 → Build & Lint」。
- **A2.** 非 macOS 侧**只**需要替换 `capture` / `watcher` / `hotkey` / `pasteback` / `crypto` 五处
  平台实现 + 5 个 `commands` 平台分支 + `storage::default_db_path` 的路径分派。其余模块经查无平台符号。
- **A3.** Windows 上 `SendInput` 对**同权限级别**的进程有效。目标进程以管理员(高完整性)运行时注入
  会被 UIPI 静默丢弃 —— 这是 Windows 的安全设计,不做绕过,归入 `ClipboardOnly` 降级。
- **A4.** `transparent: true` + `decorations: false` 的窗口形态在 WebView2 上可用(既有 `styles.css`
  中「禁用 backdrop-filter」的结论是 WebKit 专属,Windows 上无需遵守,但也**不主动加**毛玻璃)。

### Outstanding Questions

- 面板无边框 + 透明在 Windows 上会失去系统阴影与 Snap;是否需要补一层 CSS 阴影作为视觉补偿,
  留到 U6 真机看过再定。

---

## Planning Contract

### Key Technical Decisions

#### KTD10. Windows 原生绑定用 `windows` crate 0.61

与 macOS 侧 `objc2` 家族对称:在同一份 `[target.'cfg(target_os = ...)'.dependencies]` 里声明,
只在该 target 下编译。选它而非 `windows-sys` 的理由:(a) 提供 `Result` 化的返回值,省掉逐处
`GetLastError` 判定;(b) 提供 `INPUT` / `CREDENTIALW` / `DROPFILES` / `MSG` 等结构体定义,
不必手写 `#[repr(C)]` 布局(手写布局正是 FFI 最容易错的地方);(c) 0.61.3 已在锁文件里,
不引入新版本解析。**不用** `clipboard-win` / `arboard`:它们的封装层级会挡住 `CF_DIBV5` 与
注册格式 `PNG` 这类必须精确控制的细节,而仓库既有风格就是直接绑原生 API。

#### KTD11. 全局热键 = `RegisterHotKey` + 专用线程消息泵

`tauri-plugin-global-shortcut` 被 KTD2 排除,Windows 侧沿用这个决定。`RegisterHotKey(None, id, mods, vk)`
把热键绑定到**调用线程的消息队列**,因此需要一条专用线程:`RegisterHotKey` → `GetMessageW` 循环 →
命中 `WM_HOTKEY` 时调 `tray::show_panel(&AppHandle)`。卸载时不能从别的线程直接
`UnregisterHotKey`(MSDN 要求同线程),故用 `PostThreadMessageW(tid, WM_APP, ...)` 唤醒该线程自行注销并
`PostQuitMessage`。`AppHandle` 是 `Send + Sync + 'static`,存 `OnceLock` 即可,与 macOS 侧同一手法。

#### KTD12. 按键注入 = `SendInput` 四事件序列 + 物理修饰键释放等待

macOS 只发「V 按下 + V 抬起」并在事件上挂 Command flag;`SendInput` 没有等价的 flag 机制,
必须显式注入 Ctrl 的按下/抬起。序列:`Ctrl↓ V↓ V↑ Ctrl↑`。注入前用 `GetAsyncKeyState` 检查
`VK_MENU` / `VK_CONTROL` / `VK_SHIFT` / `VK_LWIN` 是否仍被物理按住,按住则等到松开(有上界,
超时即降级为 `ClipboardOnly`),否则热键本身的修饰键会污染注入结果(AEW5)。

#### KTD13. 数据密钥托管 = Windows 凭据管理器

`CredWriteW` 写入 `CRED_TYPE_GENERIC` + `CRED_PERSIST_LOCAL_MACHINE`(不随漫游配置文件走),
TargetName = `ClipVault/data_key`,UserName = `%USERNAME%`,Blob = 32 字节原始密钥。
读取用 `CredReadW`,`ERROR_NOT_FOUND`(1168)等价于 macOS 的 `errSecItemNotFound` → 首次使用需创建。
进程内缓存策略照搬 macOS:`static Mutex<Option<[u8; 32]>>`,**只缓存成功结果**,失败不缓存,
使用户修复凭据后下次调用即可恢复。凭据管理器在无桌面会话的服务上下文不可用 —— 此时
`data_key()` 返回 `CryptoError::KeyUnavailable`,加密区显示横幅,与 macOS Keychain 不可用时行为一致。

#### KTD14. 前台应用标识 = 进程 exe 全路径(小写)

`target_bundle_id()` 的语义是「回贴前那个应用的稳定标识,用于事后判断窗口是否被切走」。
Windows 等价物取 `GetWindowThreadProcessId` → `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` →
`QueryFullProcessImageNameW`,结果小写化。取不到(进程已退出/权限不足)时返回 `None`,
沿用既有「两侧都 `None` 视为未切换」的乐观策略(`pasteback.rs:239-243`)。**另需**存 `HWND`,
因为还焦点必须用窗口句柄而非 exe 路径;`should_replace_target(front_pid, self_pid)` 照常复用
(pid 概念两端通用)。

#### KTD15. 图片与文件的 Windows 原生语义

- **采集**:优先读注册格式 `"PNG"`(浏览器/截图工具常放,字节即合法 PNG,零转换) →
  回退 `CF_DIBV5` → 回退 `CF_DIB`。后两者在 Rust 侧解析 `BITMAPINFOHEADER` + 位数据转 RGBA 再编码 PNG,
  **保持 `mime = "image/png"`**,使缩略图与 FTS 通路零改动。转换函数是纯函数(输入字节、输出字节),
  可在 macOS 上单测 —— 这是本 plan 里 Windows 代码唯一能被自动化测试覆盖的部分。
- **写入**:图片同时写注册格式 `"PNG"`(现代应用)与 `CF_DIB`(传统应用);
  文件写 `CF_HDROP`(`DROPFILES` 头 + 双 NUL 结尾的 UTF-16 路径表),不做 macOS 式的文本退化;
  RTF 写注册格式 `"Rich Text Format"`;颜色沿用文本(与 macOS 一致)。

#### KTD16. 平台差异由后端的 `platform_info` 命令驱动,不在前端做检测

前端当前零平台检测(无 `plugin-os`、无 `navigator.userAgent`),而「后端通过 IPC 返回值驱动平台差异」
已是既成事实(`check_accessibility` 返回 bool 即一例)。新增一个 `platform_info() -> { os }` 命令,
前端据此渲染修饰键提示与文案。**不**在前端猜平台:WebView2 的 UA 串会随版本漂移,而后端 `cfg!` 是事实。
SF Symbols 私有区字形换成内联 SVG(仓库已有 `PIN_SVG` 先例,`frontend/src/panel.ts`)。

#### KTD17. R13 网络出口审计扩展到 Windows 目标

新增的 `windows` crate 不得引入任何对外 socket。审计命令在既有两条外增加 Windows 目标的交叉检查;
`AGENTS.md` 的约束段同步更新为双平台措辞。

### High-Level Technical Design

```
                      ┌─────────────────────────────────────────┐
   平台无关(不改)     │ capture::Captured  storage  search        │
                      │ thumbnail  privacy  pin  classify         │
                      │ pasteback::perform_paste + 2 traits       │
                      │ clipvault-core::crypto(AES-GCM)          │
                      └─────────────────────────────────────────┘
                                       ▲
              ┌────────────────────────┴────────────────────────┐
              │                                                 │
   #[cfg(macos)] 既有实现(回归基线,不动)      #[cfg(windows)] 本次新增
   ────────────────────────────────────       ────────────────────────────────
   capture:  NSPasteboard 5 类型探测            capture:  注册格式 PNG→CF_DIBV5→CF_DIB
             NSFilenamesPboardType plist                  CF_HDROP / CF_RTF / CF_UNICODETEXT
   watcher:  NSPasteboard.changeCount           watcher:  GetClipboardSequenceNumber
   hotkey:   Carbon RegisterEventHotKey ⌥⌘V      hotkey:   RegisterHotKey Ctrl+Alt+V + 线程泵
   pasteback: MacosBackend(NSPasteboard+CGEvent) pasteback: WindowsBackend(剪贴板+SendInput)
             RealAccessibilityChecker                       WindowsAccessibilityChecker(恒 true)
   crypto:   KeychainKeyProvider                crypto:   CredentialKeyProvider(凭据管理器)
   commands: 5 个平台分支                        commands: 同样 5 个分支补 Windows 实现
   storage:  ~/Library/Application Support       storage:  %APPDATA%\ClipVault
```

### Output Structure

```
app/Cargo.toml                        [改] + [target.'cfg(target_os = "windows")'.dependencies]
app/src/capture.rs                    [改] + cfg(windows) 探测 + DIB→PNG 纯函数 + 内联单测
app/src/watcher.rs                    [改] + cfg(windows) poll_change
app/src/hotkey.rs                     [改] + cfg(windows) mod imp(RegisterHotKey + 线程泵)
app/src/pasteback.rs                  [改] + cfg(windows) mod imp_win + 导出
app/src/crypto.rs                     [改] + cfg(windows) CredentialKeyProvider + default_vault_cipher
app/src/commands.rs                   [改] 5 个平台分支补 Windows + platform_info 新命令
app/src/storage.rs                    [改] default_db_path 按平台分派
app/src/tray.rs                       [改] Windows 图标与状态可见性
app/src/lib.rs                        [改] + cfg(test) 新测试模块(若有独立测试文件)
app/src/winclip_test.rs               [新] DIB↔PNG / DROPFILES 纯函数单测
app/src/commands_test.rs              [改] 新命令自动纳入注册守卫
app/icons/icon.ico                    [新] 打包与任务栏图标(由 packaging/make_icons.py 生成)
app/icons/tray-windows.png            [新] 彩色托盘图标
app/tauri.conf.json                   [改] bundle.icon 加 .ico + Windows 段
packaging/make_icons.py               [改] + ICO 写出
packaging/build-windows.ps1           [新] Windows 打包脚本(与 build.sh 对称)
packaging/build.sh                    [改] 顶部注释说明 Windows 走独立脚本?否——不改,仅在 README 说明
.github/workflows/package.yml         [新] macOS + Windows 两个打包 job + tag 触发发布
frontend/index.html                   [改] 快捷键提示文案槽位(由 JS 填充)
frontend/src/platform.ts              [新] 修饰键/文案映射 + platform_info 调用
frontend/src/main.ts                  [改] 提示填充 + 授权横幅 Windows 短路
frontend/src/settings.ts              [改] 授权/凭据横幅文案平台化
frontend/src/panel.ts                 [改] SF Symbols → 内联 SVG
frontend/src/styles.css               [改] 字体栈加 Windows 回退
docs/plans/2026-10-13-1716-feat-windows-platform-support-plan.md   [本文件]
AGENTS.md                             [改] 单一事实来源 + 双平台约束 + 进度
README.md                             [改] Windows 安装/使用/限制/凭据管理
```

### Assumptions

- **PA1.** 每个单元的改动都必须在 macOS 上保持 `cargo check/clippy/test` 全绿 —— Windows 代码虽不参与
  macOS 编译,但平台无关的重构(如 `commands.rs` 分支改写、`storage::default_db_path` 分派)会。
- **PA2.** Windows 侧 `imp` 内部的真实副作用(剪贴板读写、按键注入、前台激活)在 macOS 上**无法自动化
  测试**,与 macOS 侧 `imp` 的既有处境相同(仓库既有约定:这部分归真机手动验收)。能在 macOS 上自动化
  测试的只有纯函数:序列号推进语义、DIB↔PNG 转换、DROPFILES 构造、降级决策分支。

### Sequencing (Milestones)

- **M1 — 核心闭环(U1–U3).** Windows 能采集、能唤出、能回贴。到此为止 Windows 已「能用」。
- **M2 — 完整能力(U4–U6).** 加密区、命令收口、托盘表现。功能对等到齐。
- **M3 — 交付(U7–U9).** 前端跨平台化、打包与 CI、文档。

---

## Implementation Units

### U1. Windows 剪贴板采集 + watcher

- **Goal.** Windows 上每 100ms 检测剪贴板变化并按 `filenames → image → rtf → color → string` 优先级
  产出 `Captured`,写入 SQLite。
- **Requirements.** RW1、AEW1、AEW2。
- **Dependencies.** 无(本单元为 M1 起点)。
- **Files.** `app/src/capture.rs`;`app/src/watcher.rs`;`app/src/winclip_test.rs`;`app/Cargo.toml`。
- **Approach.** 新增 `#[cfg(target_os = "windows")] pub fn poll_first_non_empty(change_count: i64,
  prev_change_count: i64) -> Option<Captured>`,签名与 macOS 版逐字一致。探测顺序:①`CF_HDROP`
  → `DragQueryFileW` 取路径 + `canonicalize`(失败标 `resolved=false`,与 macOS 同义);
  ②注册格式 `"PNG"` → 直接 `Captured::image("image/png", ...)`;③`CF_DIBV5`/`CF_DIB` → 纯函数
  `dib_to_png(bytes) -> Option<(Vec<u8>, u32, u32)>`;④注册格式 `"Rich Text Format"` → `Captured::rtf`;
  ⑤`CF_UNICODETEXT` → `Captured::text`。全程在一次 `OpenClipboard`/`CloseClipboard` 临界区内完成,
  任何提前返回都要走同一个收尾路径(用闭包包裹,参照探针里的 `read_format` 写法);
  `OpenClipboard` 失败(剪贴板被别的进程占用)即返回 `None`,下一轮重试。`watcher::poll_change` 的
  Windows 版读 `GetClipboardSequenceNumber() as i64`,沿用 `swap` 语义与初值 `-1`。
  **颜色类型在 Windows 上不产生条目**:Win32 剪贴板没有颜色类型(颜色通常以文本或私有格式出现),
  因此 `ContentKind::Color` 在 Windows 采集侧永远不会被构造。前端 `color` 行的渲染分支保留不动
  (它服务的是历史库里的 macOS 条目与跨平台共享的库文件),本单元注释中如实标注这一差异。
- **Test scenarios.** `dib_to_png`:24bpp 自下而上、32bpp 带 alpha、负 `biHeight`(自上而下)、
  行 4 字节对齐、`biCompression != BI_RGB` 时拒绝、截断输入返回 `None`。
- **Verification.** `cargo test --workspace`(新增单测通过);`cargo clippy --workspace --all-targets
  -- -D warnings`;`cargo check --target x86_64-pc-windows-msvc` 不因本单元新增错误(依赖 sqlite C
  编译,本机不可用则记入 A1 的探针覆盖范围);真机跑 AEW1/AEW2。

### U2. Windows 全局热键

- **Goal.** `Ctrl+Alt+V` 唤出面板,退出时正确注销。
- **Requirements.** RW2、AEW3。
- **Dependencies.** U1(面板能显示才有意义,但实现上无强依赖)。
- **Files.** `app/src/hotkey.rs`。
- **Approach.** 新增 `#[cfg(target_os = "windows")] mod imp`,保持 `register(&AppHandle)` /
  `unregister()` / `check_accessibility() -> bool` 三个签名不变(前端与 main.rs 零改动)。
  `register` 幂等(已注册则返回),spawn 专用线程:`GetCurrentThreadId` → `RegisterHotKey(None, 1,
  MOD_CONTROL | MOD_ALT, VK_V.0 as u32)` → `GetMessageW` 循环 → `WM_HOTKEY` → `tray::show_panel`;
  `WM_APP` → `UnregisterHotKey` + `PostQuitMessage`。线程 id 与 `AppHandle` 各存一个静态槽。
  `check_accessibility` 返回 `true`(Windows 无此概念,RW8)。
- **Test scenarios.** 纯逻辑有限(线程泵无法单测)。断言点是签名与幂等标记的静态结构,
  由「macOS 侧三个测试基线不退化」间接保证接口未变。
- **Verification.** macOS 基线不退化;真机 AEW3 前半段(面板能被热键唤出)+ 退出后热键失效。

### U3. Windows 回贴

- **Goal.** 选中条目 → 写剪贴板 → 还焦点 → 注入 Ctrl+V → 按三态给结果。
- **Requirements.** RW3、AEW3–AEW7。
- **Dependencies.** U2(需要目标窗口在面板显示前被记录)。
- **Files.** `app/src/pasteback.rs`;`app/src/commands.rs`;`app/src/winclip_test.rs`。
- **Approach.** 新增 `#[cfg(target_os = "windows")] mod imp_win`,导出与 macOS 侧同名同签名的
  `WindowsBackend`(实现既有 `PasteBackend` trait)、`RealAccessibilityChecker` 的 Windows 等价物
  (恒 `true`)、`capture_target_app` / `target_bundle_id` / `hide_panel_and_restore_focus` /
  `capture_frontmost`。`perform_paste` 与其既有测试**零改动**。`write_clipboard` 按 KTD15 实现
  五种 kind;`simulate_paste` 按 KTD12 实现;还焦点走 `SetForegroundWindow` + 有界轮询
  `GetForegroundWindow`(时间常数集中在本文件既有的常量区,真机再标定)。
  `commands.rs` 的 `paste_entry` **不复制一份编排**:后端改由平台别名
  `pasteback::RealPasteBackend` 选定(macOS → `MacosBackend`,Windows → `WindowsBackend`),
  于是那 80 多行编排合并成**一份**、由 `#[cfg(any(macos, windows))]` 覆盖,其余平台走
  `Err` 兜底。这样两端不存在"同一段决策逻辑的两个副本各自漂移"的风险。
- **Test scenarios.** 复用既有 18 个决策层测试(必须全绿,证明迁移未动语义);
  新增纯函数测试:`build_dropfiles` 布局(头长 / 双 NUL / UTF-16 LE 路径表)、
  修饰键占用时是否走降级判定、`should_replace_target` 的 Windows 调用点参数正确性。
- **Verification.** `cargo test --workspace`;真机 AEW3/AEW5/AEW6/AEW7。

### U4. Windows 密钥存储与加密区

- **Goal.** 加密区在 Windows 上端到端可用,密钥由凭据管理器托管。
- **Requirements.** RW5、AEW8。
- **Dependencies.** U3(回贴需能解密 vault 条目才算闭环)。
- **Files.** `app/src/crypto.rs`;`app/src/commands.rs`;`app/src/main.rs`;`app/src/storage.rs`。
- **Approach.** 新增 `#[cfg(target_os = "windows")] mod cred`,实现 `KeyProvider`(KTD13),
  命名 `CredentialKeyProvider`,并提供 `#[cfg(target_os = "windows")] pub fn default_vault_cipher()`,
  与 macOS 同名同签名,使 `commands::move_to_vault` / `main.rs` 的校验块只需把 cfg 从 `macos` 扩到
  `any(macos, windows)`。`keychain_available` 命令在 Windows 上探测 `CredentialKeyProvider::data_key().is_ok()`
  —— 命令名保持不变(前端已在用),但在 Windows 语义为「系统凭据库可用」。`storage::default_db_path()`
  按平台分派到 `%APPDATA%\ClipVault\clipvault.db`(用 `std::env::var("APPDATA")`,缺失时回落到
  `dirs` 不可用路径 → 保留既有 `expect` 风格并在文案里说明)。
- **Test scenarios.** `KeyProvider` 无法在 macOS 上测(Windows-only)。可测的是
  `default_db_path` 的纯分派逻辑(抽出 `fn db_path_for(os, home, appdata) -> PathBuf` 纯函数,
  三平台分支各一个测试)。
- **Verification.** macOS 基线不退化(尤其 `crypto_test`);真机 AEW8。

### U5. Windows 命令收口

- **Goal.** 消除所有「仅支持 macOS」的用户可见残留,补 `open_url` 与授权设置深链的 Windows 语义。
- **Requirements.** RW6、RW8。
- **Dependencies.** U3、U4。
- **Files.** `app/src/commands.rs`;`app/src/pasteback.rs`(文案)。
- **Approach.** `open_url` 的 Windows 分支用 `ShellExecuteW`(`SW_SHOWNORMAL`)+ 既有 scheme 白名单,
  并用返回值真正判成败(顺带修掉 macOS 分支「失败文案声称已复制到剪贴板但其实没写」的既有不一致
  —— 该修复同时惠及 macOS,需在 macOS 上回归);`open_accessibility_settings` 在 Windows 上返回
  `Err("Windows 无需辅助功能授权")` 且前端不再渲染横幅(RW8);`pasteback` 的固定文案
  「请手动 ⌘V」改为按平台取「⌘V」/「Ctrl+V」(通过新增的 `pasteback::modifier_label()` 或等价常量)。
- **Test scenarios.** 纯逻辑:URL 白名单函数抽取后单测(既有的 scheme 校验逻辑抽成纯函数,
  三平台共用);文案选择函数按平台返回正确串。
- **Verification.** macOS 基线不退化;真机 AEW4(无授权门槛、无横幅)+ RW6 的手工验证。

### U6. 托盘与窗口的 Windows 表现

- **Goal.** Windows 任务栏托盘图标可见,四态可区分,左键行为符合 Windows 习惯。
- **Requirements.** RW7。
- **Dependencies.** U2。
- **Files.** `app/src/tray.rs`;`app/icons/tray-windows.png`;`packaging/make_icons.py`。
- **Approach.** 新增彩色托盘图标(深蓝剪贴板字形,与 app 图标同源),Windows 上用 `icon()` 而不设
  `icon_as_template`(该标志在 Windows 无意义,黑色模板图在深色任务栏不可见);状态字形
  (`set_title`)是 macOS 专属,Windows 上改为把状态信息并入 tooltip(RW7 的可观测手段),
  并在 `set_state` 里按平台分派。左键单击唤出面板的行为保留(与 macOS 一致,且 Windows 托盘左键
  单击是常见约定),右键出菜单由 `show_menu_on_left_click(false)` 保证。
- **Test scenarios.** 无(纯视觉)。
- **Verification.** 真机在浅色/深色任务栏下各看一次;AEW3 仍通过。

### U7. 前端跨平台化

- **Goal.** Windows 上所有快捷键提示、图标、文案正确,不再出现 `⌘` 与豆腐块。
- **Requirements.** RW9、RW8(前端部分)。
- **Dependencies.** U5(`platform_info` 命令)。
- **Files.** `frontend/src/platform.ts`[新];`frontend/src/main.ts`;`frontend/src/settings.ts`;
  `frontend/src/panel.ts`;`frontend/index.html`;`frontend/src/styles.css`。
- **Approach.** `platform.ts` 暴露 `await initPlatform()`(调 `platform_info`)+ `mod("E")` 之类的
  文案生成函数,返回 `⌘E` 或 `Ctrl+E`。四处的快捷键提示条(主面板底部 1 处 + 三个 modal 底部)
  已改为结构化标记:每个快捷键包在 `<kbd>` 里、鼠标手势用 `<span class="keycap">`、说明文字是
  裸文本、括号补充说明用 `.hint-note`(`frontend/index.html`,键帽样式见 `styles.css` 的
  `.hint kbd` / `.editor-hint kbd` 规则)。U7 据此在启动时遍历 `<kbd>` 改写符号即可,**不需要**
  再引入 `data-key-hint` 属性或重建 DOM。`panel.ts` 的行序号徽标(`⌘1`–`⌘9`)同样处理。
  4 处 SF Symbols 私有区字形(`index.html:19` 搜索图标、`main.ts:154`、`settings.ts:63,174`
  横幅图标)换成内联 SVG。
  `settings.ts` 的授权横幅在 Windows 上整体短路(后端 `check_accessibility` 恒 true 已足够,
  但需确认 15s 自动复检不空转);凭据横幅文案平台化(「钥匙串」→「凭据管理器」)。
  `styles.css` 的 `--font-ui` / `--font-mono` 加 `"Segoe UI"` / `"Cascadia Mono"` 回退。
- **Test scenarios.** 前端无测试框架(仓库现状),归入手工验证。
- **Verification.** macOS 上文案仍为 `⌘`(不退化);Windows 真机逐条核对 AEW 中涉及 UI 的项。

### U8. Windows 打包与 CI

- **Goal.** 产出可安装的 Windows NSIS 安装包,并在 GitHub Actions 上 macOSt 与 Windows 双平台自动打包。
- **Requirements.** (交付要求,非产品 R)。
- **Dependencies.** U1–U7(否则打出来的包装上不能用)。
- **Files.** `app/icons/icon.ico`[新];`app/tauri.conf.json`;`packaging/make_icons.py`;
  `packaging/build-windows.ps1`[新];`.github/workflows/package.yml`[新]。
- **Approach.** `make_icons.py` 增加 ICO 写出(BMP/DIB 条目,尺寸 16/32/48/64/128/256,
  不用 PNG-in-ICO 以兼容资源编译器与 NSIS),生成 `app/icons/icon.ico` 并提交;`tauri.conf.json` 的
  `bundle.icon` 加 `.ico`。`build-windows.ps1` 与 `build.sh` 对称:前端构建 → `cargo tauri build
  --bundles nsis`(显式只要 NSIS,`targets: "all"` 会连带给需要 WiX 的 MSI)→ 打印产物路径。
  CI 为单文件三 job(`macos` / `windows` / `release`):`v*` tag 触发 + `workflow_dispatch`,
  macOS job 用 matrix 按架构并行调 `packaging/build.sh arm64|x64`
  `(session-settled: user-directed — chosen over universal2: 用户要求 Intel 与 Apple Silicon 分开打包)`,
  Windows job 调 `build-windows.ps1`;`release` job 用 runner 自带的 `gh` 把 `.dmg` 与 `.exe`
  挂到 Release。只用 `actions/*` 官方 action + runner 自带 `rustup`/`gh`,不引入第三方 action。
  **Windows job 额外跑 `cargo clippy -D warnings` 与 `cargo test`** —— 这是本仓库唯一
  能在 CI 上验证 `cfg(target_os = "windows")` 分支的地方(本地开发机是 macOS,那些分支
  根本不被编译),所以把 lint 与测试也放在这里,而不是只打包。为此 Windows job 需要先构建
  前端:`tauri-build` 的 build.rs 要求 `frontendDist` 目录已存在。
  **版本必须两条线对齐**:`cargo tauri build` 启动时会交叉校验 Rust crate 与 NPM 包的
  major.minor(`tauri-cli` 的 `check_mismatched_packages`),不一致直接中止。而 `version = "2"`
  的 caret 语义会一路浮到最新 2.x(CI 上解析到 2.12.3),前端 `package-lock.json` 锁的却是
  `@tauri-apps/api` 2.11.1 —— 这正是 Windows 打包首次失败的根因。修法:两侧都写 `~2.11`
  (`app/Cargo.toml` / `frontend/package.json`)、`Cargo.lock` 纳入版本控制(此前被
  `.gitignore` 的 `*.lock` 误伤),这样 CI 与开发机用同一棵依赖树。
  **打包耗时优化(实测后追加)**:分步耗时显示开销集中在 `cargo install tauri-cli` ——
  三个 job 各自从源码编一遍(它拖着 oxc 那套解析器依赖),占 macOS 的 38%、Windows 的 45%。
  故加三块 `actions/cache`(仍是官方 action,不破上面那条约束):`~/.cargo/bin`(cargo-tauri
  二进制,key 带 `TAURI_CLI_MINOR`,命中后安装步骤直接跳过,判据用 `command -v cargo-tauri`
  而不是 cargo 自己的 already-installed 记录)、`~/.cargo/registry` + `git`、以及 release
  编译产物(key 带 Cargo.lock 哈希 + `restore-keys` 回退;只缓 release,不缓 debug ——
  clippy/test 的 `target/debug` 体积大得多,挤占 10GB 额度不划算)。同时删掉 build.sh /
  build-windows.ps1 里与 `beforeBuildCommand` 重复的前端构建。实测(同一天对比):
  Windows 21m9s → 7m52s、macOS x64 14m26s → 2m59s、arm64 14m49s → 5m03s。
  残留:编译步骤仍会重编一部分非本仓库依赖(arm64 只省下 60s),原因是 `~/.cargo/registry`
  与 `target/` 一起从缓存还原后 mtime 关系让 cargo 指纹判定为过期 —— 这是手工 target 缓存
  相对 `Swatinem/rust-cache` 的短板;要吃掉剩余 1.5~3.5 分钟需放宽「不引入第三方 action」。
  **release job 的三个硬约束(都是头一回真正跑到才暴露的)**:①该 job 只消费 Artifacts、
  不 checkout,而 gh 默认靠 `.git` 里的 remote 推断仓库,故每条 gh 命令必须显式带
  `--repo "$GITHUB_REPOSITORY"`;②upload-artifact 按上传路径的公共祖先剥前缀,DMG 会落在
  `dist/dmg/` 下而非 `dist/` 根,原来的 `dist/*.dmg` 匹配不到 —— 会**静默发出只有 `.exe`
  的 Release**,故改为 `find` 递归收集 + 空集守卫;③两个架构的 `ClipVault.app` 同名,合并
  Artifacts 时 x64 覆盖 arm64,故 macOS 侧先暂存再上传并改名 `ClipVault_<arch>.app`。
  另外 `gh release view ... --jq 'assets[].name'` 少了开头的点(jq 里裸标识符是函数引用),
  会把已经成功的发版拖成失败。
- **Test scenarios.** 无(CI 自身即验证)。
- **Verification.** 本地:`python3 packaging/make_icons.py` 产出的 ICO 用一次性解析脚本结构校验
  (条目数 6、尺寸集合、偏移在文件范围内);`cargo tauri info` 确认 config 仍可解析。
  CI:macOS 两个架构 job + Windows job 全绿、Artifacts 内可下载两个 `.dmg` 与 `.exe`。

### U9. 文档

- **Goal.** 仓库文档如实反映双平台范围与 Windows 的差异/限制。
- **Requirements.** RW8/RW9 的文档面。
- **Dependencies.** U1–U8。
- **Files.** `AGENTS.md`;`README.md`。
- **Approach.** `AGENTS.md` 的「单一事实来源」段指向本 plan(MVP plan 的跨平台 out-of-scope 声明
  已被本 plan 取代,需显式写明取代关系);「关键约束」段的「macOS 专用」改为双平台措辞并把 R13 审计
  扩到 Windows 目标;「实施进度」补 Windows 单元。`README.md` 的标题去掉「macOS」限定,新增
  Windows 安装节(SmartScreen 未签名警告、NSIS 安装、`Ctrl+Alt+V`)、Windows 限制节
  (ARM64 不承诺、管理员权限目标进程无法注入、无 color 类型条目)、数据存储路径节补
  `%APPDATA%\ClipVault`,以及**跨平台不共享密钥**的明确说明。
- **Test scenarios.** 无。
- **Verification.** 人工通读;确认 README 中每条限制都能在代码里找到对应实现或计划项。

---

## Verification Contract

### Build & Lint

每个单元提交前,macOS 上全绿(AGENTS.md 既有基线,Windows 代码不得破坏它):

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
(cd frontend && npm run build)
```

**Windows 目标的编译验证(A1 的落实手段)。** 本机(Apple Silicon macOS)无法对
`x86_64-pc-windows-msvc` 做整树编译:`rusqlite` 的 `bundled` 特性需要 MSVC 工具链编译 `sqlite3.c`,
本机只有 Apple clang,会在 `libsqlite3-sys` 的 C 编译阶段失败(缺 `stdlib.h`)。因此:

1. **API 层(已建立,持续维护).** 用一次性探针 crate(仅依赖 `windows` 0.61 + 上述 feature)
   把每个将要用到的 Win32 调用写一遍并对 `x86_64-pc-windows-msvc` 做 `cargo check`。
   该探针已跑通,确认了这些**易错点**:`GlobalFree` 在 `Win32::Foundation` 而非 `System::Memory`;
   `IsClipboardFormatAvailable` / `SetClipboardData` 在 0.61 已 `Result` 化;
   `CF_*` 常量是 `Win32::System::Ole` 的 `CLIPBOARD_FORMAT(u16)` 而非 `DataExchange`;
   `RegisterHotKey` 的 `id` 是 `i32`、`vk` 是 `u32`、修饰键是 `HOT_KEY_MODIFIERS`;
   `DragQueryFileW` 要 `HDROP`(需从 `HANDLE` 转型)而非 `HANDLE`;
   `CREDENTIALW::CredentialBlob` 是 `*mut u8` 而非 `PWSTR`;`INPUT` 的 union 字段是 `INPUT_0{ki}`。
2. **整树层(CI).** `.github/workflows/package.yml` 的 `windows` job 是 Windows 目标整树编译的
   唯一自动化验证点。**在 CI 首次跑绿之前,不得声明 Windows 支持已完成。**

### Tests

- 既有基线(必须零退化):app 94 passed + core 22 passed,1 个 trigram 测试 ignored。
- 新增(可在 macOS 上跑,因是纯函数):`dib_to_png` 的 6 个边界场景、`build_dropfiles` 布局、
  `db_path_for` 三平台分派、URL scheme 白名单纯函数、修饰键文案选择。
- 不可自动化(与 macOS 侧 `imp` 的既有处境一致,归真机验收):真实剪贴板读写、按键注入、
  前台激活、凭据管理器读写。

### AE 手工验收

在 **Windows 10 22H2** 与 **Windows 11** 真机各跑一遍 `AEW1`–`AEW8`;macOS 上回归 `AE1`–`AE12`
各关键路径一次,确认平台分支改写未伤及 macOS。

### Static Network Egress Check

沿用既有两条并在两个目标上交叉执行:

```bash
cargo tree | grep -E 'reqwest|hyper'                                   # 须空(macOS 与 windows 目标)
grep -rE 'reqwest::|hyper::|TcpStream|std::net::' app/src clipvault-core/src   # 须空
```

新增 `windows` crate 只启用剪贴板/热键/线程/凭据/Shell 相关 feature,不引入网络模块。

### 性能与资源

- Windows 上 100ms 轮询期间的 CPU 占用不得因 `OpenClipboard` 竞争而异常升高(真机任务管理器观察)。
- 25MB payload 上限与图像转换路径不得让采集线程卡顿超过一个轮询周期。

---

## Definition of Done

### Per Unit 完成标准

每个 `U<N>` 全项通过其 Verification,且:
- 平台无关代码**零改动**或改动仅限必要的 cfg 扩围(如 `any(macos, windows)`),每处扩围需能说明理由。
- macOS 侧行为无退化(测试基线 + AE 回归)。
- Windows 侧新增的 `unsafe` FFI 集中在各模块的 `imp` 内,每个 `unsafe` 块上方有说明其安全前提的注释
  (与既有 `objc2` 代码的注释密度对齐)。
- 新增/修改的文件已落盘,纯函数逻辑有对应单测。

### 全局完成标准

- `U1`–`U9` 全部完成,`AEW1`–`AEW8` 在 Windows 10/11 各跑通一次,执行记录保留在 PR 描述或 README。
- macOS 基线不退化;`cargo clippy -D warnings` 0 warning;R13 审计在双目标下为空。
- CI 首次跑绿,Artifacts 内可下载 `.dmg` 与 NSIS `.exe`。
- `AGENTS.md` 与 `README.md` 已反映双平台范围,Windows 的全部限制都有明文记载。

### Cleanup 完成标准

- 五个 `commands` 平台分支里不再存在「仅支持 macOS」这类在 Windows 上不可能触发的错误文案。
- 前端不再出现写死的 `⌘` 与 SF Symbols 私有区码点。
- 一次性探针 crate **不进仓库**(它是验证手段,不是产物),其覆盖的 API 结论固化在本文档
  「Build & Lint」一节。

### 提交策略

按单元提交,每个 commit 对应一个 `U<N>`,message 形如:
`feat(windows): U1 剪贴板采集 + watcher 序列号轮询`。
`U8` 的打包与 CI 单独提交,便于在不影响功能单元的前提下回退交付管线。
