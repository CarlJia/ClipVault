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
- **列表旁预览栏**：U1–U6 完成（见 `docs/plans/2026-10-16-1430-feat-side-preview-pane-plan.md`）——
  选中图片 / 长文本条目按 **Space**（keymap 新动作 `preview`，可重绑）或点行内缩略图，
  面板从 560×420 长到 960×420、右侧（放不下时左侧）多出预览栏，列表位置不动；图片由
  后端缩到长边 ≤ 1600px 再 base64 回传（原图不进 WebView），文本给全文，file / rtf / color
  只给提示。R13 仍为空（零新 crate：`image` / `base64` 早就在树内）。剩余：macOS 真机
  AE1–AE4 与 Windows 真机复跑；加密区模态内的预览入口列入后续 PR（模态遮住预览栏）。
  代码评审留下的 6 条已全部落地：图片解码进 `spawn_blocking`、方向键扫图时请求单飞合并、
  尺寸切换按**窗口所在屏**取工作区（不再跟鼠标跳屏）、`hintHtml` 转义收口、去掉三处
  测试专用导出、`panel.rs` 拆成 `panel/{mod,geometry,macos,windows}.rs`（原 1171 行）。
- **启动不再重捕既有剪贴板**（`Watcher::new`）：`last_change_count` 改为用构造时的剪贴板
  序列号播种（原来硬编码 `-1`，导致启动首轮必定捕获一次既有剪贴板）。这是「删除能扛过重启」
  的前提 —— 否则删掉的内容只要还在剪贴板里，就会在重启后原样回到历史；加密区条目更糟，
  它的 `content_hash` 是 salted 变体、永远不匹配明文指纹，回来的是一条**明文**普通条目。
  代价：ClipVault 未运行时复制的内容不再于启动后补录。纯规则抽在 `watcher::initial_change_count`
  里（吃 `Option`），跨平台可单测。
- **录制态让出全局热键**（plan R16 / AE7 / KTD9）：设置里点「重绑」后、按下新键之前，
  `set_hotkey_recording(active=true)` 会把 `open_panel` 在 OS 侧的热键摘掉。不摘的话，
  用户按的正是被系统截走的那一下 —— `RegisterEventHotKey` / `RegisterHotKey` 在按键进入
  WebView **之前**就吃掉它，录制监听器收不到任何 `keydown`，那一行永远停在「请按新键…」
  （macOS / Windows 同构）。收工装回有两条路：前端 `cancelRecording`（采到键 / `Esc` /
  行内取消 / 切行 / 关设置的唯一收口）+ 后端兜底（面板隐藏、页面重载）。状态机在
  `keymap::recording_transition`（纯函数，`Nothing` / `Release` / `Restore`），
  **只有真的让出过才装回** —— 否则面板每次收起都会重注册一次全局热键。撤销必须按 action
  定位，**不能**用 `unregister_all`（Windows 侧它会把消息泵线程 `Quit` 掉，等于永久失联）。
  真机 AE7（按当前全局组合重绑）仍是人工步骤，见 plan 的 Verification Contract。
- **性能优化（W1–W7）**：一轮只读审计（4 个 Explore agent 分头扫存储 / 搜索、采集 / 图像、
  前端、平台代码）定位的 30 条可改进项，按「收益 × 风险」分 7 个 wave 落地。
  **W1 SQL**：`MIGRATION_V5` 加 4 个索引（`tags(tag)` / `entries(auto_category)` /
  `entries(is_vault, is_decryptable)` / `entries(is_vault, created_at DESC)`）；
  `Storage::open` 加 WAL + `synchronous=NORMAL` + `busy_timeout`；`entries_fts_update` 补
  `WHEN OLD.is_vault = 0 OR NEW.is_vault = 0`（纯 vault 内 UPDATE 不再空写 FTS）；新增
  `ENTRY_LIST_COLS`，列表查询不再把 `payload` / `payload_nonce` BLOB 拉进内存（30 条 × 2 MB
  的 alloc 归零）；`update_note_and_tags` 走 `json_each` 多行 INSERT；`is_fts_hostile`
  收窄到真 FTS5 元字符（搜 URL / 路径 / `client-acme` 从 LIKE 全表扫回到倒排）；
  `insert_captured_with_metadata` 改成吃调用方传进来的指纹，不再对同一份 payload 算两遍
  SHA-256。
  **W2 并发**：watcher 的 `make_thumbnail` 与 `capture::poll_first_non_empty` 整段进
  `tauri::async_runtime::spawn_blocking`（图像解码期间 executor worker 仍能接 IPC）。
  `last_change_count` 随之改 `Arc<AtomicI64>`，让 blocking 闭包 move 一份进去完成 swap ——
  `NSPasteboard` 是 `!Send`，读 + 探测必须整段关在闭包里。
  **W3 启动**：`Storage::get_settings_batch` 把 setup 钩子里 4 次独立 `block_on` 合成一次
  `SELECT … WHERE key IN (…)`；`crypto::with_keychain_timeout`（同步 `data_key` 的 2 秒预算，
  超时不写 cache）+ `crypto::preload_spawn`（启动期后台预热数据密钥）；`log::read_tail` 改成
  从末尾倒读 —— 多读 1 字节是为了让 `tail_lines` 能分辨「读到 cap」与「文件本来就这么长」，
  从而正确丢掉被截断的首行半段。
  **W4 读路径**：`search_entries_with_tags` / `list_recent_with_tags` 用 `LEFT JOIN tags` +
  `GROUP_CONCAT(t.tag, CHAR(31))` 一次带回标签（分隔符用 ASCII US 而非逗号，含逗号的标签
  不会被拆开），命令层原本的二次 `with_conn` 消失；`open_url` 改 async、去掉 `block_on`。
  **W5 前端样式与节流**：`.progress` 改 `transform: scaleX()`（不再每帧触发布局）；
  `scrollToSelected` 走 `requestAnimationFrame` 合帧；`.editor-card` 去掉大范围 `box-shadow`
  （**保留** `.editor` 的 `backdrop-filter` —— 那是刻意留的，理由在 `styles.css` `.panel`
  那段注释里）；`.preview-image` 写后端给的固有尺寸消除 CLS；`escapeHtml` 改单遍
  `replace` + 映射表；`cancelAccessibilityRecheck` 在关设置时清掉 15 秒复检定时器；
  `panel.ts` 的缩略图与 `preview.ts` 的 data URL 不再过 `escapeHtml`（base64 字母表天然安全）。
  **W6 前端结构**：`renderEntryRow` 从 `renderEntries` 抽出，`editNote` 保存后只替换那**一行**
  的 `outerHTML`（列表用事件委托，换行节点不丢事件；换完重取 `selectedEl`）。
  **W7 后端**：`preview.rs` 加进程级 FIFO 预览缓存，按 **`content_hash`** 索引（内容变了
  自然不命中、不需要失效逻辑；**vault 条目 bypass** —— 其可解密状态会变，缓存会让换错 key
  时回放旧结果）；`commands::copy_log_path` 写剪贴板**前**把内容指纹登记到
  `AppState::self_writes`，`Watcher::process` 第 3.5 步 `take()` 一次比对并跳过（不论匹配
  与否都消费，且放在去重之前，自写内容不进 `last_hash`）。
  **未做，各有实测理由**：`Storage` 拆 reader / writer 双连接（W2 的 R10，KTD8 已推迟）；
  透明度 slider 前端 debounce（R15 —— 前端本来就把 `input`（只改 CSS）与 `change`（拖拽释放
  才写库）分开绑定，每次拖拽只 1 次 SQLite 写，agent 报告的「每秒 30+ 次」是没看到这层绑定）；
  `verify_decryptable_on_startup` 并发解密（R12 —— `KeychainKeyProvider::data_key` 先查进程内
  `CACHE`，plan 的「N 条 vault = N 次 Keychain 访问」前提不成立，剩余每条是微秒级 AES-GCM）；
  W6 的通用 keyed diff（R25 —— `paintEntries` 的另一个调用点是 `load()` 的全量数据替换，
  diff 在那里没有收益；真正受益的 `editNote` 单条路径已由局部更新覆盖）；preview 改走
  `Channel<Uint8Array>` / asset protocol（R27）与 thumb 提到独立 `entries.thumb` 列（R29）——
  后两条经确认独立排期。
- **测试基线**：app 261 passed + core 44 passed（合计 305），1 个 trigram 测试 ignored。
  其中 1 例来自同期合入 main 的 `app/src/pasteback_test.rs` 增量，与本轮预览栏无关。
  本轮增量是「列表旁预览栏」（plan U1–U6）：app +28（`app/src/preview_test.rs` 的 19 例 ——
  缩放算术 3 / 重编码 3 / 明文图片与文本 3 / 加密区 4（vault 文本、vault 图片、
  `is_decryptable=false` 不下发内容、错 key 走 undecryptable）/ 错误与边界 6；`app/src/panel/geometry.rs`
  的 9 例 —— `panel_size_for` 两档、`PanelMode::parse`、`fitted_size` 窄屏收缩、
  `expanded_origin` 右侧够 / 翻左侧 / 两侧都不够居中 / 负原点）、core +1
  （`default_preview_is_space`，并把 `all_actions()` 迁到 15、`default_map_covers_every_action`
  钉住基数）、前端 +24（`preview.test.ts` 19 例：开关状态机与尺寸 IPC、三种 kind 派发、
  图片缓存、跟随选中（扫过一串时请求合并）、旧结果不覆盖新内容、加密区「无法解密」、
  IPC 失败与转义；`keymap.test.ts` 5 例：
  `default_preview_is_space` / 空查询命中 / 空格焦点门 / dispatch 到 `togglePreview` /
  键帽渲染 `Space`；`settings.test.ts` 不新增用例，只把两处基数断言从 13/14 改成 14/15）。
  上一轮增量是「重绑时按当前全局组合没反应」的修复（plan R16 / AE7 / KTD9）：app +2
  （`app/src/keymap.rs` 的 `recording_transition_releases_on_entry_only` —— 进录制必须让出、
  收工才装回、其余空转；`set_recording_tracks_state` —— 标记与动作由同一次 `swap` 给出）、
  前端 +6（`settings.test.ts` 的「录制态让出全局热键」6 例：进录制先让位 / `Esc` 也装回 /
  改绑到别的组合 / 切行时让位与收工按调用顺序落地 / 没在录制时收工不发 IPC /
  录到绑不了的键时录制态留着）。
  上一轮增量是「快捷键冲突 → 弹框 → 强行换绑 + 未绑定」（plan R14 / R15）：core +6
  （`clipvault-core/src/keymap.rs` 的 `unbound_action_roundtrips_as_json_null` /
  `unbound_action_does_not_own_its_old_combo` / `entry_with_null_key_is_still_rejected` /
  `claim_takes_combo_and_unbinds_owner` / `claim_unbinds_every_other_holder_only` /
  `claim_on_own_combo_is_noop`）、app +5（`app/src/keymap.rs` 的 `conflict_error_skips_unbound`
  与 `global_hotkey_changed` 4 条：改面板动作不碰 OS / 改 `open_panel` 自己碰 OS /
  被抢占解绑时也要碰 OS / 值没变不碰）、前端 +16（弹框的三条出口与换绑失败路径、
  弹框期间按键不改绑、重开设置把没答完的弹框收掉、抢全局热键时的警告、未绑定的渲染、
  `force` 标志、未绑定不参与匹配 / 不占组合）。
  再往前是本 PR 的前半段「重绑到已被占用的组合」：core `conflict_for_*` 4 条（找到占用者 /
  排除自己与空闲组合 / 默认表本身无重复 / 多条同时占用时取排序靠前那条）与 app 的
  `conflict_error_*` 2 条（会指名道姓 / 空闲或自己放行）。
  更早的增量（「删除单条记录」的评审收尾）：app +2
  （`app/src/storage.rs` 的 `delete_entry_handles_vault_row_without_touching_others_fts`、
  `clear_undecryptable_also_removes_tags`）、core +4（`clipvault-core/src/keymap.rs` 的
  `serialize_default_schema_shape` / `parse_rejects_malformed_input` /
  `parse_unknown_action_is_dropped` —— 三条从**从不编译的孤儿文件**
  `clipvault-core/src/keymap_test.rs` 搬进来后删掉那个文件；以及
  `clipvault-core/src/schema.rs` 的 `migration_v4_creates_tags_entry_id_index`）。
  **schema 已到 V4**：`MIGRATION_V4` 为 `tags.entry_id` 建索引 `idx_tags_entry_id`
  （删除单条记录与清空不可解密条目都按 `entry_id` 删标签）。
  更早的增量是「删除选中条目」本身：`app/src/storage.rs` 的
  `delete_entry_removes_row_tags_and_search_index`、`app/src/watcher_test.rs` 的
  `last_change_count_is_seeded_with_current_sequence_number`（app +2）与
  `clipvault-core/src/keymap.rs` 的 `default_delete_entry_is_meta_backspace`、
  `default_map_covers_every_action`（core +2）。
  更早的增量来自 `app/src/log.rs`（U1 16 + U2 11 = 27 个新单测）、`app/src/single_instance.rs`
  （1 个纯函数测试）与 `app/src/watcher_test.rs`（2 条：重复指纹文案的格式契约 +
  库层 `content_hash` 去重被吞掉）。Windows-only 的 `win_pump_tests` /
  `single_instance::win_tests` 只在 CI 的 `windows` job 与真机上跑，不计入本机基线。
  前端 4 个文件共 92 例 + 1 todo（`keymap.test.ts` 39 + 1 todo（含加密区模态的重绑传导与
  `Esc` 逃生口、命名键 token 的**跨语言对照** —— 用例直接读 `clipvault-core/src/keymap.rs`
  源码，经 Vite 的 `?raw`，不给前端引 `@types/node`）、`settings.test.ts` 28（其中 11 例钉住
  「强行换绑」弹框的三条出口与「未绑定」的显示，4 例钉住录制态的收口路径 —— 关掉设置面板、
  等 keymap 的轮询窗口内重绑、录制成功 / 失败，6 例钉住「录制时让出全局热键」的时机与顺序，
  1 例钉住「1 全局 + 14 面板 = 15」的动作基数
  与分组）、`preview.test.ts` 18（预览栏开关状态机与尺寸 IPC、kind 派发、图片缓存、
  跟随选中、迟到响应、加密区「无法解密」）、`logview.test.ts` 7）由 `npm --prefix frontend test` 覆盖。
  （依赖编译期 `SQLITE_FTS5_TOKENIZER_TRIGRAMS` 标志，见 KTD3）。
  **本轮「性能优化（W1–W7）」实测基线：app 292 passed + core 45 passed（合计 337）+ 1 ignored；
  前端 135 例（7 个测试文件）**（上一轮记录的是 305 + 1 ignored / 92 + 1 todo）。新增用例按
  wave：
  W1 —— core `migration_v5_creates_hot_path_indexes`；app `list_recent_does_not_fetch_payload_blob`、
  `update_note_and_tags_with_10_tags_uses_json_each`、`update_note_and_tags_source_uses_json_each`、
  `entries_fts_update_trigger_has_when_clause_to_skip_pure_vault_updates`、
  `is_fts_hostile_allows_slash_dot_dash_plus_equal_and_backslash`、`parse_tab_dispatches_to_recent_tag_and_category`、
  `sanitize_query_strips_control_chars_and_trims`、
  `insert_captured_with_metadata_uses_caller_hash_not_internal_fingerprint`（另把
  `process_handles_binary_capture`、`schema_migration_zero_to_latest` 按新契约更新）。
  W2 —— `watcher_process_runs_make_thumbnail_inside_spawn_blocking` +
  `watcher_poll_change_async_macos_moves_pasteboard_into_spawn_blocking`（Windows 同源的那条只在 CI 的
  `windows` job 跑，不计入本机基线；两条都用括号化字面量匹配源码，避免命中注释里的裸字样）。
  W3 —— `read_tail_equals_full_read_semantics`、`read_tail_does_not_read_backup_when_current_is_large_enough`、
  `get_settings_batch_reads_several_keys_and_omits_missing`、`get_settings_batch_with_empty_keys_returns_empty_map`、
  `with_keychain_timeout_*` 3 条（透传结果 / 透传错误 / 超时返回 Err；超时用例用 50 ms 小预算，
  不拖慢套件）。
  W4 —— `list_recent_with_tags_returns_tags_alongside_entries`、
  `tags_containing_commas_are_not_split_by_the_concat_delimiter`、`search_entries_with_tags_returns_tags_alongside_hits`、
  `build_search_query_with_tags_adds_join_concat_and_group_by`、`with_tags_variant_binds_the_same_params`。
  W5 —— 前端 `panel.test.ts` 3（`escapeHtml` 单遍实现与原 5 段链式逐字等价 / 5 个元字符的实体形式 /
  base64 data URL 原样通过）+ `preview.test.ts` 4（`<img>` 固有尺寸 / 缺尺寸不写属性 / 0 不写属性 /
  缓存命中路径也带尺寸）。
  W6 —— 前端 `panel.test.ts` 3（`renderEntries` 输出 == 逐行 `renderEntryRow` 的拼接 / `data-id` /
  `data-index` / selected 标记 / 空列表走空态）。
  W7 —— `process_skips_self_written_content`、`self_write_slot_does_not_swallow_a_different_capture`、
  `preview_is_served_from_cache_after_payload_is_cleared`、`vault_image_preview_is_not_cached`。
- **⌘1–9 回贴与浏览器的标签页快捷键冲突**：在 Edge / Chrome 里**按住** ⌘1–9 回贴时，面板先隐藏、
  焦点交还浏览器，而按键**自动重复**仍在继续（默认约 500ms 后起）—— 重复出来的 `⌘+数字` 于是落到
  已经回到前台的浏览器上，被它当成自己的「切到第 N 个标签页」。用户看到的是「按 ⌘1 没贴出内容、
  浏览器却切了标签页」，随后注入的合成 ⌘V 也落到被切走的那一页（快速轻点不复现，正是因为
  交还焦点前系统还没开始重复）。
  修法：`main.ts` 的 ⌘1–9 分支先按 `ev.repeat` 只认第一次（其余仍然吞掉），再等**触发键松开**
  （`gesture.ts` 的 `waitForKeyRelease`）才走 `activateIndex` —— 手势期间面板一直是 key window，
  重复按键由 WebView 吞掉，目标应用只会看到合成的 ⌘V。面板失焦 / 1s 内未松开 → 放弃这次回贴，
  **不**照常回贴：照常回贴等于把仍被按着的 ⌘+数字原样漏给目标应用，正是要修的 bug。
  只等触发键、不等修饰键：自动重复由「键仍被按着」驱动，修饰键自身不会重复。
  本轮新增前端 5 例（`gesture.test.ts`）→ 前端 **140 例（8 个测试文件）**。
- **Windows 分支的本地检查与 CI 分工**：`cfg(target_os = "windows")` 的分支在 macOS 上**不参与**
  本地 `cargo check`，所以改到它时必须跑 `packaging/check-windows.sh`（桩掉 C 侧后拿到整树类型检查
  与 clippy，几秒；**不链接、不执行**，边界写在脚本头部注释里）。**构建与执行**仍然只有 CI 能验：
  `.github/workflows/ci.yml` 在**每个 PR** 上跑（macOS：fmt / clippy / `cargo test` / 前端构建 +
  vitest / R13 网络出口审计；Windows：clippy + `cargo test`，真实 MSVC 工具链），所以涉及 Windows
  分支的改动提 PR 即自动验证。`.github/workflows/package.yml` 只在推 `v*` tag 或手动触发时做打包
  （DMG / NSIS / Release），不做校验。
