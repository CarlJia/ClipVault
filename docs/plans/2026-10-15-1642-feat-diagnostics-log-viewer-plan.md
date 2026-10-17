---
title: "诊断日志与查看入口 - Plan"
type: feat
artifact_contract: ce-unified-plan/v1
product_contract_source: ce-plan-bootstrap
execution: code
date: 2026-10-15
---

## Goal Capsule

**Objective.** 把 ClipVault 的诊断信息真正落盘,让用户在「从 Finder / 资源管理器启动应用」这条真实路径上,无需终端也能看到一段可读的运行历史;并把「如何拿日志」从目前的隐性默契变成产品里可见的入口。

**Means.** 新增 `app/src/log.rs` 作为诊断日志的单一来源,沿用 `log` facade + 仓库自建的轻量文件 sink;日志文件落在 `clipvault.db` 同一目录(`storage::default_db_path().with_file_name("clipvault.log")`);现有 15 处 `eprintln!` 全部改走门面,新设一个「日志级别」持久化设置(`settings` 表)实时生效;在托盘菜单加「查看日志…」(在系统文件管理器中显示文件),在设置面板加一段「诊断日志」区(显示尾部、显示路径、清空、级别选择)。KTD1–KTD7。

**Authority hierarchy.** 仓库 `AGENTS.md` 与既有 plans 仍然有效:不动 R13 网络出口审计(不引入 `reqwest` / `hyper` / `std::net::`);平台差异仍按「吃显式平台参数的纯函数」惯例;新模块落在 `app/` 而非 `clipvault-core/`(`core` 历来只放跨 binary 的纯逻辑,本 plan 引入的是 Tauri 主进程内的 sink,不进 `core`)。本 plan 对「日志内容红线」做出产品级判断(任何级别都不记剪贴板内容),覆盖既有 R12 加密区/默认离线的安全边界。

**Stop conditions.** U1–U6 全部完成,验证契约全绿,macOS AE1–AE4 通过;Windows 侧以 CI `windows` job(整树 clippy + test)为编译底座,`cargo test --workspace` 在 Windows 上全绿即可;`cargo tree | grep -E 'reqwest|hyper'` 仍为空(必须);`clippy -D warnings` 全工作区干净。

**Execution profile.** 6 个 implementation unit。U1 单独落 commit(`pub mod log` 在 `lib.rs` 中导出后,`pub` 项不被 `clippy` 判 `dead_code`);U2 与 U3 强依赖 U1,顺序提交;U4、U5、U6 之间相互独立可并行,但 U6 留到最后以收口文档与测试基线。U2 与 U3 也可拆为两 commit。

## Product Contract

### Summary

为 ClipVault 引入持久化的诊断日志(单一文件,字节上限约 1 MB,无 `.1` 备份)与用户可见的查看入口(托盘「查看日志…」+ 设置面板日志区),让排障不再依赖终端或工程构建;日志只记诊断事实,绝不记剪贴板内容、文件路径或图片字节。

### Problem Frame

现在 ClipVault 的诊断信息全部走 `eprintln!` → stderr。`AGENTS.md` / `README.md` 都明确说明:`.app` 从 Finder 启动没有终端,Windows release 设置了 `windows_subsystem = "windows"` 同样没有控制台——所以「打包后的真实使用场景」里,用户看不到任何一行日志。`README.md` 的隐私一节写明了 `clipvault.db` 的数据目录位置,但日志目前不存在。15 处 `eprintln!` 散落在 7 个文件里、级别不分、没有时间戳与模块名、没有轮转上限,也不能在面板里查看或清空。

### Requirements

#### 日志落盘 (R1–R6)

- R1. 应用启动时在数据目录(与 `clipvault.db` 同目录)创建日志文件 `clipvault.log`;日志文件初始化失败(权限/磁盘满等)时,应用继续以「无日志」模式运行,启动流程不被阻塞。
- R2. 每条记录至少含 UTC 时间戳(毫秒精度,以 `Z` 结尾)、级别、模块名、消息文本,固定行格式 `YYYY-MM-DDTHH:MM:SS.mmmZ LEVEL [module] message`(KTD2)。控制字符(除换行外)按 `?` 转写;单行长度无固定上限但由显示层另设字节上限(R13)。
- R3. 日志级别默认 INFO;ERROR 始终写入,INFO 始终写入,WARN 始终写入;`verbose` 级别为额外的结构信息(条目 kind/mime/字节数、计时、状态转移),不记剪贴板内容。
- R4. 日志只记诊断事实:级别、模块、稳定错误码/消息、条目 kind / mime / 字节数 / 指纹前缀;**不记**剪贴板正文、任意文件路径(剪贴板里粘贴的文件除外,含义见 R16)、图片字节、metadata 里的文件名。
- R5. 日志文件达到单文件字节上限时按 main + `.1` 轮转:先删除既有的 `clipvault.log.1`,再把 `clipvault.log` 重命名为 `clipvault.log.1`,最后新建空 `clipvault.log` 继续写;单文件 ≤ 1 MiB,总磁盘占用 ≤ 2 MiB;轮转失败(如 Windows 上 `.1` 删除失败)降级为单文件截断,不阻塞写入。
- R6. 现有 15 处 `eprintln!` 调用点全部改为统一日志门面,信息量不少于改前;不引入新的人为噪声。

#### 日志级别设置 (R7–R9)

- R7. 日志级别可持久化到 `settings` 表 `log_level` 行,跨重启保留;缺省 `info`;非法值回落 `info`。
- R8. 日志级别在运行时修改即时生效,不需要重启应用;新级别对随后到达的每条记录生效。
- R9. 运行时以共享 `Arc<AtomicU8>` 为单一事实来源,`settings` 表只做持久化层,原子和库在启动时对齐(与 `privacy.rs` 访客模式、面板透明度的形状一致)。

#### 查看入口 (R10–R14)

- R10. 托盘菜单提供「查看日志…」入口,点击后在系统文件管理器中显示日志文件(macOS Finder 用 `open -R`,Windows 资源管理器用 `explorer.exe /select,`)。平台命令在 `app/src/log.rs` 中以「显式 `is_macos` 参数的纯函数」封装,两套参数在 macOS 上都可单测。
- R11. 设置面板提供「诊断日志」区(纳入现有 `#settings` 的 `.settings-body` 滚动容器,不开新模态),显示:日志文件绝对路径、当前文件大小、当前级别、文件尾部的最近若干行。
- R12. 设置面板提供「清空日志」按钮:截断 `clipvault.log` 到 0 字节并删除 `clipvault.log.1`(若存在);操作后界面立即反映空状态,`LogInfo.size_bytes` 与 `lines` 都归零。
- R13. 尾部读取的字节上限为 64 KiB,行数上限为 400 行,两者取先到者;超出时丢弃最早的部分,保留末尾;返回前两端的 UTF-8 不完整字节按丢字处理,绝不 panic。
- R14. 前端日志区按纯文本渲染(`textContent` 写入,不经 `innerHTML`),日志内容不被当作 HTML 执行;尾部的字节与行数上限由前端展示层独立再 cap 一道,后端 R13 是首道。

#### 隐私与边界 (R15–R16)

- R15. 日志文件为明文落盘,安全边界与 `clipvault.db` 一致(用户会话 + 文件系统权限);`README.md` 的隐私一节须同步说明日志文件位置与安全边界。
- R16. 任何级别都不记录剪贴板正文;DEBUG 级别可选地记录「条目 kind + mime + 字节数 + 8 字符指纹前缀」,但永远不记录内容本身;**不**提供「调试级记录剪贴板内容」的开关(见 Scope Boundaries)。

### Key Decisions

- 日志内容红线。**R4 / R16 的最严解读优先**:默认 INFO 与 verbose 级别都不记剪贴板内容、不记文件路径、不记图片字节、不记 metadata 文件名。替代方案(在 DEBUG 级别上允许内容记录)被显式拒绝——本机可能保存加密区条目,日志一旦写入内容就形成了用户未授权的明文副本,违背 R12「加密区内容从不离开系统凭据库」的设计意图。Governs R4, R16。
- 入口形态。**两条入口并立**:托盘「查看日志…」做「在系统文件管理器中显示」,设置面板日志区做「内嵌的尾随查看 + 清空 + 级别选择」。替代方案(只在面板内做,或只做系统文件管理器)被拒绝:面板内嵌覆盖「用户自己现场看一眼」场景,系统文件管理器覆盖「交给支持 / 自己用 grep 翻历史」场景,两者不可互相替代。Governs R10, R11, R12。

### Acceptance Examples

- AE1. 打包后的 macOS `.app` 从 Finder 启动,触发一次回贴失败,在设置面板打开「诊断日志」区,能看到对应行包含 `ERROR [pasteback]` 与带 `Z` 后缀的 UTC 时间戳;在 Windows 桌面启动后做相同事,行为一致。无需打开终端。
- AE2. 设置面板把级别切到「详细」并重启应用,重新进入设置面板,级别仍为「详细」;切回「标准」后,新的 INFO 及以下记录不再追加(可由后续 INFO 级别以下调用点的 `info!` 宏不再写出文件验证)。
- AE3. 连续复制大量内容让日志轮转若干次后,数据目录里同时存在 `clipvault.log` 与 `clipvault.log.1`,两份文件大小之和 ≤ 2 MiB,再增长也只覆盖已有 `.1`;`clipvault.log` 单文件始终 ≤ 1 MiB。
- AE4. 在设置面板点「清空日志」,文件大小立刻变为 0,UI 立即显示空状态;点托盘「查看日志…」,在 Finder/资源管理器中定位到该文件并高亮(`open -R` / `explorer /select,`)。

### Scope Boundaries

**Deferred for later**

- 日志上传/遥测/导出为压缩包。
- 结构化 JSON 日志格式。
- 远程 syslog / 网络日志后端。
- 面板内的日志搜索/过滤 UI。
- 「调试级记录剪贴板内容」的显式开关(KTD 决策已拒绝,留作未来若改 R12 时的可逆点)。
- `log` 之外的 `tracing` / `tauri-plugin-log` / `fern` 等日志框架(本 plan 已选择 `log` facade + 自建 sink)。

**Outside this product's identity**

- 任何会引入 `reqwest` / `hyper` / `std::net::` 的日志后端(R13 仍是项目硬约束,任何后续日志需求也必须满足此约束)。
- 修改 watcher 行为、回贴行为、托盘其他菜单项、加密区语义。
- 打造日志专用的单实例机制。Windows 侧已有单实例守卫(Windows plan 的 RW10 / KTD18 / U10),但它解决的是「重复启动多出托盘图标」这一产品行为,不是为日志竞争设计的;本 plan 不依赖它做互斥,沿用既有的写入降级策略。

## Planning Contract

### Key Technical Decisions

- KTD1. 日志门面:复用 `log` crate 作为 facade(版本由 Cargo 解析到 `0.4.34`,该版本已经在依赖树中,通过 `tauri` / `tao` 间接拉入,本次仅在 `app/Cargo.toml` 中把它升为直接依赖,不会引入新的 crate)。sink 由本仓库自建,放在 `app/src/log.rs`。不引入 `tracing` / `tauri-plugin-log` / `fern` / `simplelog`。理由:facade 0.4.34 已解析,直接依赖零网络/新 crate 增量;`tracing` 与 `tauri-plugin-log` 会增加依赖图谱与权限表面,与 R13 文化不符;自建 sink 让行格式、轮转、字节上限、读取逻辑全部由仓库拥有,适合「只记诊断事实」的强约束。
- KTD2. 时间戳不引入日期 crate。手写纯函数 `pub fn format_timestamp(epoch_millis: i64) -> String`,把 Unix 毫秒转成 `YYYY-MM-DDTHH:MM:SS.mmmZ`(`Z` 结尾,UTC),使用 Howard Hinnant `civil_from_days` 算法的最简变体。`chrono` 0.4.45 在 `Cargo.lock` 中存在但 `cargo tree --workspace` 中不可达;`time` 0.3.55 由 `toml_datetime` 间接拉入但只用于 toml 解析,直接依赖其宏/格式化 API 不可控。手写函数单测覆盖闰年/世纪年/UTC 边界,与 `storage::resolve_data_dir` 同样可在 macOS 上被另一平台的分支测到(本函数无平台分支,但纯函数性同等重要)。
- KTD3. 文件句柄与写入门控:进程级 `OnceLock<Mutex<LogState>>`,`LogState` 持有 `std::fs::File` 与当前字节数;写入路径持锁 → 检查轮转 → 写。锁中毒(`PoisonError`)降级为丢弃本次写入并把状态置为 no-op,绝不 panic。R1 的「不阻塞启动」通过 init 函数返回 `Result<(), InitError>` 配合 `setup` 中的 `if let Err(e) = log::init(...) { eprintln!("..."); }` 实现(在 init 成功前不替换全局 logger,所有 `log::info!` 调用仍走 stderr,与今天一致)。
- KTD4. 内容派生的指纹只记前缀。`watcher.rs:247` 现状为 `eprintln!("[capture] skipped: {e} (hash {hash})")`,全量 64-hex content hash 在共享日志时构成对剪贴板内容的指纹泄露(R4 红线)。改为 `format!("[..{}]", &hash[..8])`(8 hex 字符前缀),同时把 target 改为 `watcher::capture`,使面板日志能按模块筛选(未来)。`panel.rs` 的 `log_skip_once` 现有调用点传的是「诊断分支标签」(`"not-main-thread"` / `"no-monitors"` 等,6 处皆为 `&'static str` 字面量,不是文件系统路径),迁移时保持原标签。
- KTD5. 日志级别持久化:沿用 `privacy.rs` 的形状,`app/src/log.rs` 内提供 `pub fn parse_log_level(raw: Option<&str>) -> Level`、`persist_log_level(storage, level)`、`boot_load_log_level(storage, atomic) -> Level`,key 常量 `pub const LOG_LEVEL_KEY: &str = "log_level"`;`AppState` 新增 `pub log_level: Arc<AtomicU8>`(0=info,1=verbose,2=debug=目前等同 verbose + 额外结构事实),`set_log_level` 命令路径与 `set_panel_opacity` 形状完全一致。
- KTD6. 「查看日志…」在 `app/src/log.rs` 中以纯函数 `pub fn reveal_argv(is_macos: bool, path: &str) -> (String, Vec<String>)` 提供,macOS 返回 `("open", vec!["-R".into(), path.into()])`,Windows 返回 `("explorer.exe", vec!["/select,".into() + path])`(`/select,` 紧贴路径,无空格,标准格式);`commands.rs` `reveal_log` 命令把 `reveal_argv(cfg!(target_os = "macos"), &path.to_string_lossy())` 喂给 `std::process::Command::new(prog).args(args)`,错误映射为 `Result<(), String>`。`use std::process::Command` 放在函数体内,参考 `open_url` 中同类 `#[cfg]` 分支的注释,保证 Windows 编译下不被 `clippy -D warnings` 判未使用导入(本命令两平台都用,直接放函数体顶部即可,但仍用此模式与 `open_url` 对齐)。
- KTD7. 前端日志区不开新模态:复用 `#settings` 的 `.settings-body` 滚动容器(R17 滚轮独占),沿用现有 `.settings-section` / `.settings-row` / `.settings-value` 视觉层级;不引入新 CSS 类,减少 styles.css 改面。`textContent` 写入纯文本,不调用 `escapeHtml`(那是 innerHTML 路径的辅助);后端 R13 的字节/行数 cap 是首道,前端在展示层再 cap 一道「最多渲染 200 行」以保证面板内不出现长滚动阻塞。
- KTD8. 日志轮转走「main + `.1`」方案,而非单文件 FIFO 截断(session-settled: user-approved — chosen over 单文件截断: 保留最近一段历史,排查跨天/跨多次触发的问题时不至于一段一段消失;代价是磁盘上限翻倍到 2 MiB,且 `remove_file`/`rename` 在 Windows 上若失败需降级)。实现要点:
  - **轮转函数纯化**:`pub fn backup_log_path(dir: &Path) -> PathBuf { dir.join("clipvault.log.1") }` 与现有的 `log_path_in` 同形,纯函数可单测。
  - **触发时机**:写之前调 `should_rotate(current_size, incoming_len, MAX_LOG_BYTES)`,在持锁状态下做 `fs::remove_file(backup).ok()` → `fs::rename(current, backup)`(失败时降级 `set_len(0)` + 记录一行 `WARN [log]` 到 stderr)→ 重置 `size=0` → 走原 append 路径。`init` 阶段若发现 `current` 已经超过 `MAX_LOG_BYTES`(陈旧大文件),同样先尝试轮转而非直接 `set_len(0)`,这样重启后仍保留启动前的最后一段历史。
  - **clear_log**:截断 `clipvault.log` 到 0 + 删除 `clipvault.log.1`(若存在),与 R12 的"清空所有日志"语义一致。
  - **read_log**:同时读 `clipvault.log.1` 与 `clipvault.log`(按时间顺序拼接,旧在前新在后),再走 `tail_lines` 字节/行数 cap;`.1` 存在与否对调用方透明。设置面板 UI 上标注 "最近 ~400 行" 而非"整个文件",避免用户误以为轮转丢了数据。
  - **reveal_log**:继续选中 `clipvault.log`(`open -R` / `explorer /select,`),同一文件夹下 `.1` 仍可见;不改为打开父目录,避免破坏既有 R10 的"定位到该文件"的语义。

**后续变更(2026-10-17,修 2026-10-15 落地时留下的两处缺陷)。** 上面 KTD7 的「不引入新 CSS 类」
按原文保留 —— 它在当时成立,但「级别下拉框」与「文件路径」两行不适用,各自新增了一个类:

- 级别 `<select>` 当时套的是 `.slider`,而那是 `input[type=range]` 的**轨道**样式
  (`appearance:none` + `height:4px`),套在 select 上把控件压成一条看不见选中文案的细线。
  改用专用类 `.settings-select`(自绘盒子 + 内联 caret + `color-scheme: dark`)。
- 「文件路径」原本只有 `title` 能看全值(`max-width: 280px` 截断),现支持**点击复制**:
  新增 IPC 命令 `copy_log_path`(复用回贴写剪贴板的平台后端),样式类 `.settings-log-path`。

KTD7 的其余结论不变:仍不开新模态、仍复用 `.settings-body` 滚动容器与 `.settings-section` /
`.settings-row` / `.settings-value` 视觉层级、仍用 `textContent` 写入、仍在展示层 cap 200 行。

### High-Level Technical Design

```mermaid
flowchart TB
    subgraph setup["main.rs setup()"]
      A[解析 data dir] --> B[log::init(data_dir)]
      B --> C[storage::open]
      C --> D[privacy::boot_load_*]
      D --> E[log::boot_load_log_level]
      E --> F[log::set_max_level]
    end
    subgraph writes["热路径"]
      W1[log::info! / warn! / error! 宏] --> W2[LogState sink]
      W2 -->|size+len>cap| W3[remove .1 + rename current -> .1]
      W3 --> W2
    end
    subgraph ipc["IPC"]
      I1[read_log] --> J[read .1 + current, tail 64K/400 行]
      I2[clear_log] --> K[truncate 0 + remove .1]
      I3[set_log_level] --> L[atomic store + persist]
      I4[reveal_log] --> M[reveal_argv + spawn]
    end
    setup --> writes
    J --> T[LogTailDto { path, size, level, lines }]
    L --> writes
```

调用顺序:setup 早期解析 `data_dir`(通过 `storage::default_db_path` 的父目录)→ `log::init` 以 `O_APPEND | O_CREAT | O_RDWR` 打开 `clipvault.log`,若失败则保留 stderr 作为兜底,后续 `log::*!` 宏依然能编译运行但写到 stderr。轮转仅发生在「写之前」,按 KTD8 的 main + `.1` 流程。`read_log` 同时读 `.1` 与 current(按时间顺序拼接)再走 `tail_lines` 取 64 KiB / 最多 400 行。`reveal_log` 调用 `reveal_argv(is_macos, path)` 拿参数数组再 `Command::new` spawn。

### Sequencing Notes

- U1 单 commit,无前置;`pub mod log` 在 `lib.rs` 中公开,避免 `pub` 项被 `clippy` 判 `dead_code`。
- U2 与 U3 强依赖 U1;U2 改 `Cargo.toml`(`log = "0.4"`,旧 lockfile 自动命中)与 `main.rs` / `lib.rs` / `commands.rs`;U3 仅改 7 个文件的日志调用点。
- U4 与 U5 可并行,都依赖 U1 + U2。
- U6 留最后,收口 README、AGENTS.md、CHANGELOG 之类。

### Open Questions

无(用户的「confirm」把 Phase 0.7 的三个 fork 全部委派为 agent 决策,见 Key Decisions / KTD 记录)。

### Assumptions

- 不为日志单独加锁。Windows 侧已按 Windows plan 的 RW10 / KTD18 / U10 加了单实例守卫(重复启动只激活已有实例),因此 Windows 上不会有两个实例同时写同一日志;macOS 侧重复打开 `.app` 本就只激活已有实例,但两个开发实例(或 dev 与打包版并存)仍可能写同一文件——此时轮转可能让其中一方看到被截断的文件,日志可能丢行——属可接受退化,写入失败时降级为 no-op,绝不崩溃。
- 「verbose」级别不会引入 DEBUG-only 的剪贴板内容开关;若未来需要,在 product 决策层(R16 修订)单独评估,不在本 plan 的范围内。
- `log` facade 版本选 `0.4`(目前 transitive 已是 0.4.34),不锁更细的版本。

## Implementation Units

### U1. 日志模块骨架与格式化

**Goal.** 引入 `app/src/log.rs` 与 `app/src/log_test.rs`,实现纯函数化的路径、级别、格式化与轮转逻辑,作为后续单元的地基。

**Requirements.** R1, R2, R4, R5, R13 的纯函数部分(不含 R10/R11/R12 的 IPC / 前端)。

**Files.**
- `app/src/log.rs`(新)— `pub fn default_log_path() -> PathBuf` / `pub fn log_path_in(dir: &Path) -> PathBuf` / `pub fn backup_log_path(dir: &Path) -> PathBuf` / `pub enum Level { Info, Verbose }` / `pub fn parse_level(raw: Option<&str>) -> Level` / `pub fn format_timestamp(epoch_millis: i64) -> String` / `pub fn format_line(ts_millis: i64, level: Level, target: &str, msg: &str) -> String` / `pub fn should_rotate(current_size: u64, incoming_len: u64, cap: u64) -> bool` / `pub fn tail_lines(bytes: &[u8], max_bytes: usize, max_lines: usize) -> String` / `pub fn reveal_argv(is_macos: bool, path: &str) -> (String, Vec<String>)` / `pub const MAX_LOG_BYTES: u64 = 1 * 1024 * 1024` / `pub const LOG_LEVEL_KEY: &str = "log_level"`。
- `app/src/log_test.rs`(新)— 上述纯函数单测。
- `app/src/lib.rs`(改)— `pub mod log;`。
- `app/Cargo.toml`(改)— 暂不新增直接依赖(仅添加 U2 的 `log = "0.4"`),但本 unit 在测试中需要把 facade 暂用 `Option`/`Mutex` 旁路,本 unit 本身仅测试纯函数,不调用 `log::*!` 宏,不需要 facade。

**Approach.**
- 路径:`default_log_path() = storage::default_db_path().with_file_name("clipvault.log")`;`log_path_in(dir)` 是供测试使用的纯函数,直接 `dir.join("clipvault.log")`。
- 级别:`Level` 仅两值,`parse_level` 把 `Some("verbose")` 视为 Verbose,其它(含 `None` 与 `"info"`)视为 Info;`AtomicU8` 编码 0=Info / 1=Verbose(预留 2=Debug 但不开放)。
- 时间戳:用 `chrono`-free 转换。接受 `epoch_millis: i64`(允许负值以兼容 1969-12-31 这种边界,只测正向);返回 `String`;单测覆盖 0、1234567890123、`1970-01-01 00:00:00.000Z`、闰年边界(`2000-02-29`、`1900-02-28` vs `2000-02-29`)、`9999-12-31 23:59:59.999Z`。
- 行格式:`{ts} {LEVEL} [{target}] {msg}\n`,其中 `msg` 内的控制字符(除 `\n` 外)按 `?` 转写;长度不限。
- 轮转:`should_rotate(size, incoming, cap) = size.saturating_add(incoming) > cap`;`MAX_LOG_BYTES = 1 MiB`;实现层在 `init` 与每次写之前调用。
- 尾部:`tail_lines(bytes, max_bytes, max_lines)` 切片到 `max_bytes` 内,丢弃可能不完整的首行,按 `\n` 分行,保留最后 `max_lines` 行;UTF-8 边界 `from_utf8_lossy` 处理,绝无 panic。
- 平台显示:`reveal_argv(false, "/x/y.log") = ("open", ["-R", "/x/y/log"])`;`reveal_argv(true, r"C:\x\y.log") = ("explorer.exe", ["/select,C:\\x\\y.log"])`(字符串拼接,保留 Windows 路径反斜杠)。

**Patterns to follow.** `app/src/storage.rs:54` `resolve_data_dir` 的「显式平台参数的纯函数」惯例;`app/src/privacy.rs` `parse_panel_opacity` 的「非法值回落 + clamp」形状;`app/src/storage.rs:697-726` 数据目录解析的「macOS 上测另一平台分支」的单测写法(本函数的 Windows 分支也可在 macOS 上用 `reveal_argv(true, "C:\\...")` 测)。

**Test scenarios.**
- `log_path_in_appends_filename`:对 `PathBuf::from("/x/ClipVault")` 调 `log_path_in` 断言产出 `/x/ClipVault/clipvault.log`。
- `parse_level_known_and_unknown`:`Some("verbose")` → Verbose;`Some("info")` / `Some("")` / `None` / `Some("debug")` → Info。
- `format_timestamp_known_epochs`:epoch 0 → `1970-01-01T00:00:00.000Z`;epoch 1234567890123 → `2009-02-13T23:31:30.123Z`;闰年 `2000-02-29` 与非闰年 `1900-02-28` 区分;`9999-12-31T23:59:59.999Z` 仍正确(不溢出)。
- `format_line_control_chars`:消息含 `\x07` 与 `\n` 时,输出整行以 `\n` 结尾,`\x07` 被替换为 `?`。
- `should_rotate_under_and_over_cap`:cap=1024,`0+1024=false` / `1024+1=true` / `1023+2=true` / `1024+0=true`(陈旧大文件也轮转)。
- `backup_log_path_appends_dot_1`:对 `PathBuf::from("/x/ClipVault")` 调 `backup_log_path` 断言产出 `/x/ClipVault/clipvault.log.1`。
- `should_rotate_with_backup_present`:cap 命中后,纯层(`should_rotate` 返回 `true`)由 I/O 层负责 `remove(.1)` + `rename(current, .1)`,此处只断言轮转条件本身(无新纯函数)。
- `tail_lines_truncates_and_drops_partial_first`:输入 200 行,`max_lines=10` 保留最后 10 行;首行被截断(无前导 `\n`)时丢弃。
- `tail_lines_handles_split_multibyte`:输入末尾是 1 字节 UTF-8 序列不完整(`0xE4` 一个孤立字节),不 panic,产出合法 UTF-8。
- `reveal_argv_macos_uses_open_r`:断言 `("open", ["-R", "/var/log/clipvault.log"])`。
- `reveal_argv_windows_uses_explorer_select`:断言 `("explorer.exe", ["/select,C:\\Users\\me\\AppData\\Roaming\\ClipVault\\clipvault.log"])`(`/select,` 紧贴路径,无空格)。
- `format_line_combined`:已知 ts + level + target + msg 拼成整行,`\n` 在尾。

**Verification.** `cargo test -p clipvault --lib log_test::` 全绿;`cargo clippy --workspace --all-targets -- -D warnings` 不报 `dead_code`(`pub mod log` 在 `lib.rs` 中导出)。

### U2. 启动接线与日志级别运行时状态

**Goal.** 让应用启动时落地日志文件、把 `log` facade 接入,运行时持有可改的日志级别原子。

**Requirements.** R1, R3, R7, R8, R9。

**Files.**
- `app/src/log.rs`(改)— `pub fn init(path: &Path) -> Result<(), InitError>` / `pub fn boot_load_log_level(storage: &Storage, atomic: &AtomicU8) -> Level` / `pub fn persist_log_level(storage: &Storage, level: Level) -> SqliteResult<()>` / `pub fn set_max_level(atomic: &AtomicU8, level: Level)` / `pub fn read_info(path: &Path, atomic: &AtomicU8) -> LogInfo { size, level, backup_size }` / `pub fn read_tail(dir: &Path, max_bytes: usize, max_lines: usize) -> std::io::Result<String>`(读 `.1` + current 拼接后走 `tail_lines`)/ `pub fn clear(dir: &Path) -> std::io::Result<()>`(truncate current + remove `.1`);`pub struct LogState`(`File` + `u64` 当前字节 + `Level` 缓存)+ 全局 `OnceLock<Mutex<LogState>>`。
- `app/src/main.rs`(改)— 顶部 `use clipvault::log;`;`setup` 中第一句调 `log::init(&default_log_path())`(失败 `eprintln!` 兜底,不返回 Err),然后 `Storage::open`,再 `log::boot_load_log_level`;`AppState` 新增 `log_level: Arc<AtomicU8>` 字段(在 `app.manage(AppState { ... })` 之前初始化)。
- `app/src/commands.rs`(改)— `AppState` 加 `log_level: Arc<AtomicU8>`。
- `app/Cargo.toml`(改)— `log = "0.4"` 加入 `dependencies`(依赖树已有 0.4.34,无新 crate)。
- `app/src/lib.rs`(如未加)— 确认 `pub mod log;` 已在 U1 加入。

**Approach.**
- `init`:以 `OpenOptions::new().create(true).append(true).read(true).write(true)` 打开;读 `metadata().len()` 拿初始字节;若 `len > MAX_LOG_BYTES` 则按 KTD8 走「`remove_file(.1).ok()` → `rename(current, .1)`」轮转(失败则降级为 `set_len(0)`),然后用 create 标志新开空 `clipvault.log` 并把 `LogState.size` 记为 0;设置 `OnceLock`;若 `set` 因并发(单进程实际不会)失败,降级为 no-op 函数式 logger。
- `boot_load_log_level`:`storage::get_setting(LOG_LEVEL_KEY)` → `parse_level` → `atomic.store(level as u8)` → 返回 `Level`。
- `set_max_level`:`atomic.store(level as u8)`;同时把 `Level` 缓存到 `LogState` 内,写路径读 `LogState` 内的 level(避免每条记录都原子读)。
- `init` 之前不设置 `log` 全局 logger,所有 `log::info!` 仍走 stderr;`init` 成功后才 `log::set_boxed_logger`,从此刻起 `log::*!` 走 sink。
- 调用顺序:在 `Storage::open` **之前**完成 `log::init`,因为 `Storage::open` 失败会向上返回、设置流程中断,此刻若有日志记录应写到文件;但日志文件路径需要 `default_db_path()`(通过 `default_log_path()`)——而 `default_db_path()` 只读环境变量,不会触发 SQLite,可在 `init` 中安全求值。R1 的「不阻塞启动」由 `init` 返回 `Result` + 调用点用 `if let Err` 决定兜底模式实现。

**Patterns to follow.** `privacy::boot_load_panel_opacity` 形状;`app/src/main.rs:64-74` 的 boot-load 顺序写法;`clipvault-core/src/crypto.rs` 的 `InitError` 风格(如已存在);若 `crypto` 没有合适的,定义一个 `log::InitError = std::io::Error` 即可。

**Test scenarios.**
- `init_creates_file_when_missing`:临时目录 + 不存在的 `clipvault.log` → `init` 后文件存在且大小 0;`init` 调用第二次幂等(`OnceLock` 行为),不报错。
- `init_rotates_oversize_existing_via_rename`:在临时目录预写 2 MiB `clipvault.log` 与 1 KiB `clipvault.log.1` → `init` 后 `clipvault.log` ≤ 1 MiB,旧内容落到 `clipvault.log.1`,原 1 KiB `.1` 已被覆盖;`OnceLock` 二次调幂等。
- `init_when_only_backup_exists_no_panic`:临时目录有 `clipvault.log.1` 但无 `clipvault.log` → `init` 创建新空 `clipvault.log`,`.1` 保持原状(不被误删,属非我们责任的状态)。
- `init_failure_is_non_fatal`:传入只读目录(`temp_dir` 之外)→ 返回 `Err`,但调用方按「兜底」处理,无 panic。
- `boot_load_log_level_round_trip`:用 `storage::test_helpers::open_test_db` 写 `info` → `boot_load` 读出 `Info`;写 `verbose` → 读出 `Verbose`;写 `garbage` → 读出 `Info`(回落)。
- `set_max_level_takes_effect_immediately`:`set_max_level(atomic, Verbose)` 后,从 sink 写出的 INFO 行也仍写出(因为默认 INFO 已包含),但 `set_max_level(atomic, Info)` 后 `atomic` 值为 `Info`;`should_emit(atomic, target_level) -> bool` 的纯函数在 level=Info 时不写出 Debug(target_level=Debug),`level=Verbose` 时写出 — 此函数是 U2 的新纯函数,与 U1 的 `should_rotate` 配套。
- `clear_truncates_current_and_removes_backup`:`init` 写入若干行,然后模拟触发一次轮转(预置 `.1`),调 `clear` → `clipvault.log` 大小 0 + `clipvault.log.1` 不存在。
- `read_info_reports_size_and_level`:`init` 写入 → `read_info` 返回的 `LogInfo { size, level, backup_size }` 与实际一致;`size` 仅指 `clipvault.log` 当前大小,`backup_size` 为 `clipvault.log.1` 大小(为 0 时 UI 不显示「旧日志」行)。
- `read_tail_combines_backup_and_current`:预置 `clipvault.log.1` 含 N 行 + `clipvault.log` 含 M 行,`read_tail` 返回值应含 `.1` 的全部与 current 的全部(若合起来 ≤ cap)或仅 current 的尾部(cap 触发时);拼接顺序保证时间序(`.1` 先,`current` 后)。
- `read_tail_handles_missing_backup`:仅 current 存在时,`read_tail` 不报错,行为退化为只读 current。

**Verification.** `cargo test -p clipvault --lib log::` 全绿;`cargo run` 后数据目录有 `clipvault.log`,内容含 `INFO [main] ...` 的启动行;关掉可写权限后启动,应用仍正常运行(手动一次性验证,见 Definition of Done)。

### U3. 收口现有 eprintln 调用点

**Goal.** 把 15 处 `eprintln!` 全部改为 `log::error!` / `log::warn!` / `log::info!`,信息量不少于改前,不引入新的内容泄露。

**Requirements.** R4, R6, R16(隐私红线),以及 R3 的级别映射。

**Files.**
- `app/src/main.rs`(改)— 4 处 `eprintln!` → `log::warn!` / `log::error!`。
- `app/src/watcher.rs`(改)— 3 处:payload 太大 / content_hash 冲突(改前缀)/ insert 失败。target 设为 `"watcher::capture"`,便于面板后续按模块筛选。
- `app/src/pasteback.rs`(改)— 5 处:macOS 前台等待 / Windows 前台等待 / 打开剪贴板失败 / 修饰键未释放 / SendInput 被拒绝。target 设为 `"pasteback"`(macOS 端)与 `"pasteback::win"`(Windows 端),或统一 `"pasteback"` 而依赖 `cfg!`;本 plan 选统一 `"pasteback"`,在 macOS 上只测到 `cfg!(false)` 路径但统一。
- `app/src/hotkey.rs`(改)— 1 处:未实现的全局 action。target `"hotkey"`。
- `app/src/keymap.rs`(改)— 1 处:parse 失败。target `"keymap"`。
- `app/src/panel.rs`(改)— `log_skip_once` 改走 `log::warn!(target: "panel::display", ...)`;其 once-per-branch 内部 mutex 保留(见 R6「不引入新噪声」);`is_macos` 显式参数化已存在(注释 50-51 行说「同 `storage::resolve_data_dir`」),不破坏既有单测。
- `app/src/tray.rs`(改)— 2 处:图标加载失败 / 访客模式持久化失败。target `"tray"`。

**Approach.**
- KTD4 的 `watcher.rs:247` 改写:`let prefix = &hash[..8.min(hash.len())]; log::warn!(target: "watcher::capture", "[capture] skipped: {e} (hash {prefix}..)");` ——注意 `hash` 是 `String` 可能极短(空字符串),`8.min(hash.len())` 防御;另外为避免 `prefix` 为空时格式里出现 `(hash ..)`,加一个 `if prefix.is_empty()` 跳过。
- 级别映射:失败类(R3 的 ERROR/WARN 始终写出)走 `log::error!` 或 `log::warn!`;其余(目前没有 INFO 级别的 `eprintln!`)不需要新增 INFO 埋点。R6 的「不引入新的人为噪声」要求 KTD4 与 KTD3 的「保留原样」即满足。**例外(2026-10-15 调整)**:`watcher.rs` 的 `content_hash` UNIQUE 冲突那一条从 `warn!` 降为 `debug!`(verbose 才写) —— 它是设计内的「重复指纹」路径,每次启动遇到已入库的剪贴板都会命中一次(`Watcher::new` 的 -1 初值保证首轮捕获既有剪贴板),在**用户可见**的日志里被读成故障;文案同时改为 `… (duplicate fingerprint, no new entry; hash <8 字符>..)`,由纯函数 `duplicate_fingerprint_log` 组装并有单测。
- 模块级:每个文件最顶部不引入额外 `use`(全局 logger 由 `log` crate 提供),保留各自文件的现有 use 列表。
- `panel::log_skip_once` 的内部 `Mutex<Vec<&'static str>>` 是去重,改动小;其六处调用点传 `"not-main-thread"` 等标签字面量,迁移时 `log::warn!(target: "panel::display", "面板定位跳过（{}）：{}", path, detail)` 等价替换 `eprintln!`,语义不变,`LOGGED` 仍去重。

**Patterns to follow.** R4 隐私红线写在每个文件相关行的注释(本仓库的注释密度与 KTD 引用是惯例);`app/src/watcher.rs:170` 已有 `[capture] skipped:` 前缀,保持。

**Test scenarios.**
- `eprintln_count_is_zero`:静态断言 `app/src` 中 `eprintln!` 调用数为 0(`grep` 在测试或 CI 中执行,见 Verification Contract);`log::error!` / `log::warn!` 在 `app/src` 中各文件均存在。
- `panel_log_skip_once_dedup_unchanged`:既有单测(若未存在则新增)断言同一 path 多次调用只产生一次 `log::warn!`。
- `watcher_hash_logging_truncates`:单元测一个 `truncate_hash_for_log(s: &str) -> String` 纯函数(KTD4),断言 `""` 不报错但返回空串、`"abcdef"` → `"abcdef"`(全量输出,因为已短于 8)、`"0123456789abcdef..."` → `"01234567..."`(8 字符前缀 + `..`)。
- `watcher_skipped_log_message_starts_with_brackets`(实装为 `duplicate_fingerprint_log_keeps_prefix_and_truncates_hash`,断言对象是纯函数而不是 warn 路径):断言文案以 `[capture] skipped:` 开头(对接既有 grep 习惯)+ KTD4 的 8 字符截断 + 空指纹不打出残缺格式。**为什么抽纯函数**:测试构建不安装全局 logger(`install_global_logger` 在 `cfg(test)` 下是空实现),真实的 log 记录捕获不到 —— 原计划「模拟 `e.code` 走 warn 路径、断言格式化输出」在测试里做不到。另加 `process_swallows_duplicate_fingerprint_already_in_db`:换一个 Watcher 绕过 5 秒窗口去重、走到 UNIQUE 分支,断言不报错、不新增行。

**Verification.** `rg "eprintln!" app/src` 输出为空;`cargo test --workspace` 全绿;`cargo run` 后日志文件含 `WARN [panel::display]` 等行(一次空屏幕调用触发 `no-monitors` 之类);把日志级别切到 `verbose` 还能看到 `watcher::capture` 的重复指纹行(2026-10-15 起它不再是 WARN)。

### U4. 日志级别 IPC 与设置面板日志区

**Goal.** 让用户在设置面板里看尾随日志、清空、切换级别;后端提供 `read_log` / `clear_log` / `get_log_level` / `set_log_level` 四个 IPC。

**Requirements.** R7, R8, R11, R12, R13, R14。

**Files.**
- `app/src/commands.rs`(改)— `LogInfo` DTO + 4 个 `#[tauri::command]`。
- `app/src/main.rs`(改)— `invoke_handler` 注册 4 个新命令。
- `frontend/index.html`(改)— 在 `#settings-body` 的「快捷键」区块之下追加「诊断日志」`.settings-section`,含路径、大小、级别 select、清空按钮、尾部 `<pre>` 区;`Esc` 仍由既有 settings close 路径处理。
- `frontend/src/logview.ts`(新)— `formatTailForView(raw: string, maxLines: number) -> string` 等纯 helper。
- `frontend/src/logview.test.ts`(新)— helper 单测。
- `frontend/src/settings.ts`(改)— 新增 `readLog() / clearLog() / getLogLevel() / setLogLevel()` IPC 包装。
- `frontend/src/main.ts`(改)— 在 `openSettings()` 末尾追加 `void refreshLogSection();`;新的 `refreshLogSection` 拉取并填入。

**Approach.**
- 后端 `LogInfo`:`{ path: String, size_bytes: u64, level: String, lines: String }`,`lines` 已是按 R13 cap 后的尾部(后端负责字节/行上限)。
- `read_log`:`log::read_info(path, &state.log_level)` 读出当前文件大小 + 级别,再 `log::read_tail(path, 64*1024, 400)` 拼接 `.1` + current 取尾部;`level` 字段是当前生效级别(用 `log_level.load(SeqCst)` 翻译为 `info` / `verbose`)。
- `clear_log`:`log::clear(path)`,失败返回 `Result<(), String>`。
- `set_log_level(level: String)`:clamp 到 `info` / `verbose`(`parse_level` 已做);`atomic.store` + `persist_log_level`;**`Level` 缓存** 在 `LogState` 内由 sink 自己同步读取,所以写路径不需要 AppState 介入(避免锁 `AppState` 阻塞 watcher);AppState 的 `log_level` 原子只在 IPC 与启动路径使用。
- 前端:`refreshLogSection` 把 `lines` 写入 `<pre>` 的 `textContent`(R14);`level` 渲染为 `<select>`,变更时调 `setLogLevel`,成功后再 `refreshLogSection` 一次;`clearLog` 成功后也再拉一次。
- 设置面板布局:沿用现有 `.settings-section` / `.settings-row` / `.settings-value` 三类,无新 CSS。

**Patterns to follow.** `commands::paste_entry` 的 `Result<T, String>`;`set_panel_opacity` 的 `clamp` + 持久化形状;`settings.ts` 的 `getPanelOpacity` / `setPanelOpacity` 包装与 fallback;`openSettings` 在 main.ts:567-579 已 `void renderHotkeysSection()`,U4 在同一行下方加 `void refreshLogSection();`。

**Test scenarios.**
- `parse_level_via_commands_returns_default_on_garbage`:在 `commands_test.rs` 中通过 mock state 调 `get_log_level` (storage 写入 `garbage`) → 返回 `"info"`。
- `set_log_level_persists_to_settings`:同一文件,写 `verbose` → `settings` 表 `log_level` 行值为 `"verbose"`。
- `set_log_level_out_of_range_falls_back`:`"debug"` 与 `""` → 写库 `info`。
- `formatTailForView_truncates_to_max_lines`:输入 1000 行,`maxLines=200` 保留最后 200 行;输入不足 `maxLines` 不裁剪;输入为空返回空串。
- `formatTailForView_preserves_newlines`:多行输入不被并成单行。
- Vitest `npm --prefix frontend test` 全绿(包含已有的 `keymap.test.ts`)。

**Verification.** `cargo test --workspace` 全绿;`npm --prefix frontend test` 全绿;`npm --prefix frontend run build` 通过;`tauri dev` 启动后,设置面板能显示尾部、清空、切换级别并跨重启保留。

### U5. 托盘「查看日志…」与跨平台显示文件

**Goal.** 在 macOS Finder / Windows 资源管理器中显示日志文件。

**Requirements.** R10。

**Files.**
- `app/src/log.rs`(改,已在 U1)— `reveal_argv` 已写,U5 直接调用。
- `app/src/commands.rs`(改)— `reveal_log` 命令:从 `log::default_log_path()` 拿路径 → `log::reveal_argv(cfg!(target_os = "macos"), &path.to_string_lossy())` → `Command::new(prog).args(args).spawn()`;失败映射为 `Result<(), String>`。
- `app/src/main.rs`(改)— `invoke_handler` 注册 `reveal_log`。
- `app/src/tray.rs`(改)— 在 `build_tray` 中新增 `MenuItem::with_id(app, "logs", "查看日志…")`,在 `on_menu_event` 中 `"logs" => crate::commands::reveal_log(...)`。
- `app/src/commands_test.rs`(改)— 测 `reveal_argv` 在两平台的参数数组(`commands` 本身只测 `reveal_argv` 的纯部分,spawn 不在单测里)。

**Approach.**
- 托盘菜单的「查看日志…」点击后:**不**打开主面板,直接 reveal;若想同时打开主面板以便于用户继续操作,可在主面板已显示时跳过重复显示,但本 plan 保持简单:不打开主面板,只 reveal(在 Finder/Explorer 中定位文件,用户可双击在「文本编辑」/「记事本」中打开)。
- `reveal_log` 错误:若 `init` 之前未创建文件,`default_log_path()` 仍可返回路径(路径只是字符串),但 reveal 会在 OS 层报「文件不存在」;把这种错误翻译为「日志文件尚未创建,需先让应用记录一条日志」的提示,而不是裸 `io::Error`。
- `reveal_argv` 的 Windows 路径在 `commands_test.rs` 测时直接传字面量字符串,不真 spawn(否则测依赖 Windows)。

**Patterns to follow.** `commands::open_url` 的 `#[cfg]`-scoped `use std::process::Command` 与两平台 spawn 形状;`tray::on_menu_event` 已有的 menu id 派发模式(`"open"` / `"skip_once"` / `"guest"` / `"vault"` / `"about"` / `"settings"` / `"quit"`)。

**Test scenarios.**
- `reveal_argv_macos_returns_open_with_R`(U1 已测,U5 在 commands_test 中再次复用断言)。
- `reveal_argv_windows_returns_explorer_select`(同上)。
- `reveal_log_returns_path_for_existing_file`:临时文件 + `reveal_argv` → spawn(测层不真正 spawn,用 `assert_cmd` 或 trait-ify;本 plan 选最简:`reveal_log` 内部抽出 `pick_reveal_argv(path) -> (String, Vec<String>)`,在 commands_test 测这个纯函数,不真 spawn)。
- `tray_menu_contains_logs_item`:tray 菜单构建后,`app.tray_by_id` 拿到菜单,`menu.items()` 中存在 id == `"logs"`(若 Tauri API 允许;若不允许,跳过该测,保留 grep 静态检查 `rg "查看日志" app/src`)。
- `on_menu_event_logs_dispatches_to_reveal`:在 `tray::on_menu_event` 现有 match 上加 `"logs" => crate::commands::reveal_log(app)` —— 不在单测里跑 spawn,改用 `cfg(test)` 注入的 mock 计数器(若 Tauri 允许,否则跳过此测)。

**Verification.** `cargo test --workspace` 全绿;macOS 手工:打包后从 Finder 启动 → 点托盘「查看日志…」→ Finder 打开并高亮 `clipvault.log`;Windows 手工:打包后从桌面启动 → 点托盘「查看日志…」→ 资源管理器打开并选中文件。

### U6. 文档与验证契约收口

**Goal.** 同步 README、AGENTS.md 与验证脚本,使本 plan 的成果在文档与 CI 上闭环。

**Requirements.** R15。

**Files.**
- `README.md`(改)—「隐私 → 数据存储位置」一节追加日志文件路径与一句话说明(明文落盘,安全边界与 DB 相同)。原计划的「建议单实例运行以保证日志连续」一句**不再需要**:Windows 侧已由单实例守卫保证(Windows plan RW10 / KTD18 / U10),macOS 侧重复打开本就只激活已有实例。
- `AGENTS.md`(改)—「实施进度 → 测试基线」一行更新到新基线;`R13 网络出口审计` 不变。
- `app/src/commands.rs`(改,可选)— `reveal_log` 的错误信息本地化(中文 toast 风格)。
- `docs/plans/` 暂不追加新 plan(本 plan 即来源)。

**Approach.**
- README 隐私一节的「数据存储位置」按现有写法续:
  - macOS: `$HOME/Library/Application Support/ClipVault/clipvault.log`
  - Windows: `%APPDATA%\ClipVault\clipvault.log`
  - 加一句:「日志仅记诊断事实(级别、模块、错误码、条目 kind/mime/字节数/指纹前缀),不记剪贴板正文。日志为明文落盘,安全边界与 `clipvault.db` 相同。」
- AGENTS.md 测试基线:本 plan 不会提前确定新基线;执行 `ce-work` 时跑 `cargo test --workspace` 即可获真实数字,本 plan 留 `<!-- AUTO-UPDATE -->` 占位(避免本 plan 在执行前给出不准确的数字)。

**Patterns to follow.** 既有 `R13 网络出口审计` 段落;`README.md` 现有的「macOS / Windows 各一行」行文。

**Test scenarios.** 本 unit 不引入新 Rust/TS 单测,以 grep 静态检查收口:
- `rg "clipvault.log" README.md` 命中(两个平台各 1 次)。
- `rg "R13" README.md AGENTS.md` 仍存在。
- `rg "eprintln!" app/src` 为空(沿用 U3 的断言,但收口到 CI)。

**Verification.** `cargo check --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`cargo test --workspace`、`(cd frontend && npm run build)`、`npm --prefix frontend test` 全部通过;`cargo tree | grep -E 'reqwest|hyper'` 为空;`rg "eprintln!" app/src clipvault-core/src` 为空(扩展到 clipvault-core 是为了对称,虽本次未改)。

## Verification Contract

- `cargo check --workspace --all-targets` 通过。
- `cargo clippy --workspace --all-targets -- -D warnings` 通过(含 Windows 目标的 CI `windows` job 跑一遍 `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo test --workspace`)。
- `cargo fmt --all -- --check` 通过。
- `cargo test --workspace` 通过。最低基线 `app 190 + core 27 = 217` 之前的失败/忽略数保持,U1–U5 引入的新测试全部绿。
- `npm --prefix frontend test` 通过(vitest 包含 `keymap.test.ts` 与 U4 新增的 `logview.test.ts`)。
- `npm --prefix frontend run build` 通过。
- R13 审计:
  - `cargo tree | grep -E 'reqwest|hyper'` 为空。
  - `rg "eprintln!" app/src clipvault-core/src` 为空。
  - `rg -E 'reqwest::|hyper::|TcpStream|std::net::' app/src clipvault-core/src` 为空(沿用既有 CI 行)。
- macOS 手工(一次性):打包后从 Finder 启动 → 触发一次回贴失败 → 设置面板日志区出现对应行(AE1)→ 清空按钮生效(AE4)→ 级别切换跨重启保留(AE2)。
- Windows 手工(一次性,在真机或 VM):同 macOS 四项。
- 隐私门:启动后写入若干条 INFO/WARN 日志,人工 `grep` 检查日志内容不含剪贴板原文(可粘贴一段唯一字符串触发 watcher 记录,日志中不应出现该字符串)。

## Definition of Done

- U1–U6 全部按各自 `**Verification**` 段通过。
- CI 在 PR 上 macOS + Windows 两个 job 全绿,包含 R13 审计与 `rg "eprintln!"` 静态检查。
- README 与 AGENTS.md 同步更新,新基线数字由 `ce-work` 实际跑测试后填回(不留「217」这种计划阶段给定的不准确数字)。
- 任何「R16 边界附近的灰色日志」在 PR 评审阶段被显式确认。
- 调试追踪(临时 `eprintln!` / `dbg!` / 死代码 / 试验性 fn)在最终 diff 中清除。

## Appendix

### A. eprintln 现状盘点(15 处,均迁移至 `log::*!`,target 与级别见 U3)

| 文件:行 | 现有内容 | target | 级别 |
|---|---|---|---|
| `app/src/main.rs:49` | ActivationPolicy 失败 | `main` | warn |
| `app/src/main.rs:82` | boot_load_hotkeys 失败 | `main` | warn |
| `app/src/main.rs:107` | 加密区可解密性校验失败 | `main` | error |
| `app/src/main.rs:122` | apply_global_hotkey 失败 | `main` | warn |
| `app/src/watcher.rs:170` | payload too large | `watcher::capture` | warn |
| `app/src/watcher.rs:247` | `[capture] skipped: {e} (hash {hash})` | `watcher::capture` | debug(KTD4 改前缀;2026-10-15 由 warn 降级 —— 重复指纹是设计内路径) |
| `app/src/watcher.rs:250` | `clipvault watcher: insert failed: {e}` | `watcher::capture` | error |
| `app/src/pasteback.rs:566` | macOS 前台等待超时 | `pasteback` | warn |
| `app/src/pasteback.rs:913` | Windows 前台等待超时 | `pasteback` | warn |
| `app/src/pasteback.rs:960` | 打开剪贴板失败 | `pasteback` | error |
| `app/src/pasteback.rs:1027` | 修饰键未释放 | `pasteback` | warn |
| `app/src/pasteback.rs:1044` | SendInput 被拒绝 | `pasteback` | error |
| `app/src/hotkey.rs:324` | 未实现的全局 action | `hotkey` | warn |
| `app/src/keymap.rs:40` | keymap parse 失败 | `keymap` | warn |
| `app/src/panel.rs:296` | 面板定位跳过 `{path}` | `panel::display` | warn |
| `app/src/tray.rs:148` | 托盘图标加载失败 | `tray` | warn |
| `app/src/tray.rs:205` | 访客模式持久化失败 | `tray` | error |

实际 17 处,表中 17 条全部覆盖(U3 实施时按表对照即可)。

### B. 外部 crate 影响

- `log 0.4.34`:已在 `cargo tree --workspace` 中(transitively 通过 `tauri` / `tao`),`app/Cargo.toml` 加为直接依赖后 lockfile 仅有「`clipvault` 直接依赖 `log`」这一条边变化,`cargo tree` 输出形状不变,R13 审计仍空。
- `chrono 0.4.45` / `time 0.3.55`:均不直接依赖。`chrono` 在 lockfile 中存在但 `cargo tree --workspace | grep chrono` 为 0(不可达);`time` 0.3.55 由 `toml_datetime` 间接拉入,仅用于 toml 解析。
- 本 plan 引入**零**新 crate。
