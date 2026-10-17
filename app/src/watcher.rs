//! 后台剪贴板监听器（U3 / R1 / R3 / R4 / R11 部分）。
//!
//! 一个 `tokio::task` 每 100ms 轮询 `NSPasteboard.general` 的 `changeCount`
//! （非破坏性读取）。当 `changeCount` 增长时读取 `.string(forType: .string)`，
//! 经去重、skip-once、访客模式过滤后，做自动分类并写入 SQLite。
//!
//! 轮询间隔取 100ms：平均检测延迟约 50ms、峰值 ≤ 100ms，满足 R1 的 < 200ms 预算。
//!
//! 剪贴板来源按平台取:macOS 读 `NSPasteboard.changeCount`,Windows 读
//! `GetClipboardSequenceNumber()` —— 两者都是单调递增的剪贴板序列号,判据同一套。
//! 其余平台 `poll_change` 返回 `None`,便于在任意平台跑确定性单元测试（测试只驱动
//! [`Watcher::process`] 与 [`should_dedup`]，不触碰真实剪贴板）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use clipvault_core::ContentKind;

use crate::capture::{self, Captured};
use crate::storage::Storage;
use crate::thumbnail;

/// 轮询间隔（R1 修订：< 200ms 检测预算）。
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 去重时间窗:同一指纹在该时长内重复出现视为重复,不入库。
const DEDUP_WINDOW: Duration = Duration::from_secs(5);
/// 指纹长度:SHA-256 十六进制的前 16 个字符。
const HASH_PREFIX_LEN: usize = 16;
/// verbose 日志里保留的指纹长度(KTD4):8 个十六进制字符,全量指纹在共享日志时
/// 等于泄露剪贴板内容的内容派生指纹。
const LOG_HASH_PREFIX_LEN: usize = 8;

/// 计算文本的 SHA-256 指纹前缀（16 位十六进制,即前 8 字节)。
pub fn hash_prefix(text: &str) -> String {
    use std::fmt::Write as _;

    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();

    let mut hex = String::with_capacity(HASH_PREFIX_LEN);
    for byte in digest.iter().take(HASH_PREFIX_LEN / 2) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// 组装「同一指纹已在库中」的 verbose 文案。
///
/// 抽成纯函数是为了可测:测试构建不安装全局 logger(`log.rs` 的
/// `install_global_logger` 在 `cfg(test)` 下是空实现),日志级别与文案无法在单测里
/// 捕获,只有这里能被断言。保留 `[capture] skipped:` 前缀(既有 grep 习惯),
/// 并把「重复、未新增条目」说清楚 —— 它曾被读成故障。
pub fn duplicate_fingerprint_log(err: &str, hash: &str) -> String {
    let prefix: String = hash.chars().take(LOG_HASH_PREFIX_LEN).collect();
    if prefix.is_empty() {
        format!("[capture] skipped: {err} (duplicate fingerprint, no new entry; hash <empty>)")
    } else {
        format!("[capture] skipped: {err} (duplicate fingerprint, no new entry; hash {prefix}..)")
    }
}

/// 纯函数去重判定，便于单测。
///
/// 当上一条指纹存在、与当前指纹相同、且距上次入库时间在 `window` 内时返回 `true`
/// （即“应去重、不入库”)。
pub fn should_dedup(
    prev: Option<(&str, Instant)>,
    current_hash: &str,
    now: Instant,
    window: Duration,
) -> bool {
    match prev {
        Some((hash, ts)) => hash == current_hash && now.duration_since(ts) < window,
        None => false,
    }
}

/// 剪贴板监听器。
pub struct Watcher {
    storage: Arc<Storage>,
    /// 访客模式开关（U10 负责写入,U3 只读)。
    guest_mode: Arc<AtomicBool>,
    /// “本次不记录”一次性开关（U10 负责置位,U3 消费并复位)。
    skip_next: Arc<AtomicBool>,
    /// 最近一次入库的指纹与时间戳,用于去重窗口。
    last_hash: Mutex<Option<(String, Instant)>>,
    /// 最近一次读到的 `changeCount`,用于判断剪贴板是否变化。
    ///
    /// W2 / KTD7:用 `Arc<AtomicI64>` 而不是 `AtomicI64`,让 `poll_change_async`
    /// 的 `spawn_blocking` 闭包可以 move 一个 owned 句柄进去——`NSPasteboard`
    /// 不是 `Send`, 不能直接 hold 进 executor 的 .await 边界, 必须把
    /// 整个读取 + 探测都关进 blocking pool。swap 必须发生在同一个闭包里
    /// (「序列号无论是否捕获成功都推进」这一既有语义),否则闭包返回后再
    /// `store` 会与下一轮 `load` 错位。
    last_change_count: Arc<AtomicI64>,
    /// W7 / R30：自写抑制槽。ClipVault 自己写进剪贴板的内容（目前只有
    /// 「复制日志路径」）的指纹登记在这里；下一轮 `process` 见到同一指纹就跳过。
    /// 与 [`crate::commands::AppState::self_writes`] 是**同一个** `Arc`。
    self_writes: Arc<Mutex<Option<String>>>,
}

/// 当前剪贴板序列号;读不到时返回 `None`。
///
/// 平台差异只在此处:`GetClipboardSequenceNumber` 在非交互式窗口站返回 `0`(「没有
/// 序列号」),当作 `None` 处理 —— 否则会把「序列号不可用」误判成「剪贴板永远没变」。
#[cfg(target_os = "macos")]
fn current_change_count() -> Option<i64> {
    use objc2_app_kit::NSPasteboard;
    Some(NSPasteboard::generalPasteboard().changeCount() as i64)
}

#[cfg(target_os = "windows")]
fn current_change_count() -> Option<i64> {
    match unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() } {
        0 => None,
        n => Some(n as i64),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn current_change_count() -> Option<i64> {
    None
}

/// `last_change_count` 的初值:拿到当前序列号就播种它,拿不到才退回 `-1`
/// (那环境下首轮试一次、什么都没有,与播种之前的行为一致)。
///
/// 吃 `Option` 而不是直接调 [`current_change_count`] 是为了让这条**跨平台**规则能在
/// macOS 上单测 —— 两个平台分支各自只能在真机上跑到(AGENTS.md 的平台绑定约定)。
pub fn initial_change_count(current: Option<i64>) -> i64 {
    current.unwrap_or(-1)
}

impl Watcher {
    /// 构造监听器。
    ///
    /// `last_change_count` 用**构造时的剪贴板序列号**播种(不是 `-1`),于是启动后第一轮
    /// 会判定「序列号没变」而不捕获既有剪贴板 —— 这正是「删除能扛过重启」所依赖的机制:
    /// 删除只抹掉库里的行,而库是跨重启唯一的「这份内容我已经见过」的记忆(进程内的
    /// `last_hash` 与 `changeCount` 都活不过重启)。若首轮照旧捕获,用户刚删掉的密码只要
    /// 还在剪贴板里就会原样回来;加密区条目更糟 —— 它的 `content_hash` 是 salted 变体,
    /// 永远不匹配明文指纹,重新入库的会是一条**明文**普通条目(与 `move_to_vault` 同因)。
    ///
    /// 代价是 ClipVault 未运行时复制的内容不再于启动后被补录(首轮只认「此后发生的粘贴」)。
    pub fn new(
        storage: Arc<Storage>,
        guest_mode: Arc<AtomicBool>,
        skip_next: Arc<AtomicBool>,
        self_writes: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            storage,
            guest_mode,
            skip_next,
            last_hash: Mutex::new(None),
            // W2:Arc 包一层,让 `poll_change_async` 的 spawn_blocking 闭包可以 move.
            last_change_count: Arc::new(AtomicI64::new(initial_change_count(
                current_change_count(),
            ))),
            // W7 / R30:与 `AppState::self_writes` 是同一个 Arc。
            self_writes,
        }
    }

    /// 运行监听主循环（永不返回)。由调用方 `tokio::spawn`。
    pub async fn run(self) {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            // W2 / R9:`capture::poll_first_non_empty`(读 NSPasteboard 数据 + 头字节
            // 解析 + Dict 转换)是 CPU/IO 重活,在 blocking pool 跑,让 executor
            // worker 在这一帧可以接 IPC(搜索框输入响应不再被吞)。
            if let Some(captured) = self.poll_change_async().await {
                self.process(captured).await;
            }
        }
    }

    /// 异步读剪贴板变化（macOS 实现 / 多类型扩展后）。
    ///
    /// 整段读取与探测关进 `spawn_blocking`——`NSPasteboard.generalPasteboard()`
    /// 返回的 `Retained<NSPasteboard>` 是 `!Send`,不能 hold 进 executor 的 .await
    /// 边界。`last_change_count` 用 `Arc<AtomicI64>`,让闭包 move 一个 owned 句柄
    /// 在内部完成 swap(语义上保留「序列号无论是否捕获都推进」)。executor 这一帧
    /// 立即让出, IPC 在 spawn_blocking worker 跑 NSPasteboard 读取期间仍能跑。
    #[cfg(target_os = "macos")]
    async fn poll_change_async(&self) -> Option<Captured> {
        let last_change_count = Arc::clone(&self.last_change_count);
        let join = tauri::async_runtime::spawn_blocking(move || -> Option<Captured> {
            use objc2_app_kit::NSPasteboard;
            let pasteboard = NSPasteboard::generalPasteboard();
            let change_count = pasteboard.changeCount() as i64;
            let prev = last_change_count.swap(change_count, Ordering::SeqCst);
            if change_count <= prev {
                return None;
            }
            capture::poll_first_non_empty(change_count, prev)
        });
        // spawn_blocking panic / cancel 退回到「本轮无捕获」;`Option::default()` 即 `None`。
        let captured: Option<Captured> = join.await.unwrap_or_default();
        captured
    }

    /// 非 macOS / 非 Windows 平台没有剪贴板序列号, 始终返回 `None`。
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    async fn poll_change_async(&self) -> Option<Captured> {
        None
    }

    /// 异步读剪贴板变化(Windows 实现 / U1)。
    ///
    /// 与 macOS 版逐字对应:`GetClipboardSequenceNumber` 与 `last_change_count.swap`
    /// 整段都在 spawn_blocking 内。`OpenClipboard` + 探测 + DIB→PNG 转码都是
    /// 阻塞调用，必须在 blocking pool。
    #[cfg(target_os = "windows")]
    async fn poll_change_async(&self) -> Option<Captured> {
        let last_change_count = Arc::clone(&self.last_change_count);
        let join = tauri::async_runtime::spawn_blocking(move || -> Option<Captured> {
            let change_count =
                unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
                    as i64;
            let prev = last_change_count.swap(change_count, Ordering::SeqCst);
            if change_count <= prev {
                return None;
            }
            capture::poll_first_non_empty(change_count, prev)
        });
        // spawn_blocking panic / cancel 退回到「本轮无捕获」;`Option::default()` 即 `None`。
        let captured: Option<Captured> = join.await.unwrap_or_default();
        captured
    }

    /// 处理一次多类型捕获:去重 + skip-once + 访客模式过滤 + 25MB 上限 +
    /// 分类 + 入库。
    ///
    /// `auto_category` 对非文本类型取 `content_kind.as_str()`（与 KTD8
    /// 对齐）;文本类型走 [`crate::classify::classify`]。
    pub async fn process(&self, captured: Captured) {
        // 1. 空白文本直接忽略（binary 不可能为空,直接走 25MB 检查）
        if matches!(captured.kind, ContentKind::Text)
            && captured
                .text
                .as_deref()
                .map(|t| t.trim().is_empty())
                .unwrap_or(true)
        {
            return;
        }

        // 2. 25MB payload 上限（multi-type plan R23）:binary 跳过并日志一行
        if let Some(payload) = captured.payload.as_ref()
            && payload.len() > MAX_PAYLOAD_BYTES
        {
            let mb = payload.len() / 1024 / 1024;
            ::log::warn!(target: "watcher::capture", "[capture] skipped: payload too large ({mb} MB)");
            return;
        }

        // 3. 指纹。
        let hash = captured.fingerprint();
        let now = Instant::now();

        // 3.5 W7 / R30：自写抑制。ClipVault 自己刚写进剪贴板的内容（目前只有
        //     「复制日志路径」）不该进历史。写侧（`commands::copy_log_path`）把
        //     指纹登记在这个共享槽里；这里 `take()` 一次性取出并比对 —— **不论
        //     匹配与否都消费掉**，所以它只对「紧随其后的那一次捕获」生效：写剪贴板
        //     失败时也不会留成一颗日后误伤真实复制的定时炸弹。
        //
        //     放在去重之前：自写内容不该进入 `last_hash`，否则用户紧接着真复制
        //     同样一段文字时，会被 5 秒去重窗口当成重复而丢掉。
        {
            let mut guard = self.self_writes.lock().await;
            let pending = guard.take();
            if pending.as_deref() == Some(hash.as_str()) {
                ::log::debug!(
                    target: "watcher::capture",
                    "[capture] skipped: self-written content (R30)"
                );
                return;
            }
        }

        // 4. 去重判定:锁内比对并更新指纹,入库前释放锁(不跨 await 持锁)。
        {
            let mut guard = self.last_hash.lock().await;
            if should_dedup(
                guard.as_ref().map(|(h, t)| (h.as_str(), *t)),
                &hash,
                now,
                DEDUP_WINDOW,
            ) {
                return;
            }
            *guard = Some((hash.clone(), now));
        }

        // 5. skip-once:消费并复位;命中则本次不入库(R11)。
        if self.skip_next.swap(false, Ordering::SeqCst) {
            return;
        }

        // 6. 访客模式:开启时不入库(R10;开关由 U10 写入)。
        if self.guest_mode.load(Ordering::SeqCst) {
            return;
        }

        // 7. auto_category: 文本走 classify 启发式,二进制与 kind 同名。
        let auto_category = match captured.kind {
            ContentKind::Text => {
                let text = captured.text.as_deref().unwrap_or("");
                crate::classify::classify(text).as_str().to_string()
            }
            other => other.as_str().to_string(),
        };

        // 8. 落库。图片走 thumbnail 生成(U3)—— CPU 重活搬到 blocking pool。
        //
        // W2 / R8:`image::load_from_memory` + resize + `PngEncoder::write_image`
        // + base64 在 20 MB PNG 上约 50-100 ms,旧路径直接占在 executor worker
        // 上,期间任何 `search_results` / `paste_entry` IPC 都要排队等这一帧
        // 跑完。把「读 payload + 跑 `make_thumbnail`」搬进 spawn_blocking,
        // executor worker 等到 join handle 后立即接 IPC;thumbnail 与 DB 写
        // 顺序执行,但 IPC 不再被吞。
        let mut metadata_to_persist =
            serde_json::to_string(&captured.metadata).unwrap_or_else(|_| "{}".to_string());

        let thumb: Option<crate::thumbnail::Thumbnail> =
            if matches!(captured.kind, ContentKind::Image) {
                // 把 owned 数据 move 进 'static 闭包。
                let payload_for_thumb = captured.payload.clone().unwrap_or_default();
                let mime_for_thumb = captured.mime.clone().unwrap_or_default();
                let join: tauri::async_runtime::JoinHandle<Option<crate::thumbnail::Thumbnail>> =
                    tauri::async_runtime::spawn_blocking(move || {
                        thumbnail::make_thumbnail(&payload_for_thumb, &mime_for_thumb)
                    });
                join.await.unwrap_or_default() // spawn_blocking panic / cancel 退回到无 thumb
            } else {
                None
            };

        if let Some(t) = thumb {
            // 把 thumb base64 与 dim 拼进 metadata_json
            let mut meta: serde_json::Value = serde_json::from_str(&metadata_to_persist)
                .unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = meta.as_object_mut() {
                obj.insert("thumb".into(), serde_json::Value::String(t.b64));
                obj.insert("thumb_dim".into(), serde_json::Value::Number(t.dim.into()));
            }
            metadata_to_persist = meta.to_string();
        }

        match capture::insert_captured_with_metadata(
            &self.storage,
            &captured,
            &auto_category,
            &metadata_to_persist,
            &hash,
        )
        .await
        {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                // content_hash UNIQUE 冲突:该指纹此前已入库,视为重复,不入库
                // (MVP plan「重复指纹去重(content_hash UNIQUE 触发冲突 → 视为同条)」)。
                // 级别取 verbose(debug!)而非 WARN:这是**设计内的常见路径** —— 同一份内容
                // 隔一会儿再复制一次很常见,in-process 的 5 秒去重窗口拦不住,库里那行还在,
                // 于是由 `content_hash` UNIQUE 拦下。日志在设置面板里给用户看,每次都留一条
                // WARN 会被读成故障(实测就是这样被报回来的)。
                // 仍要留痕的理由:指纹算法一旦退化(如 binary 分支曾漏掉 payload,让同 kind
                // 的所有内容共用一个指纹),真正的「重复」与「被误判为重复而丢弃的新内容」
                // 在代码里完全无法区分,只有 verbose 日志能事后分辨。
                // 打 `e` 而不是写死「duplicate content_hash」:这个分支按错误码匹配,将来
                // 加了别的约束(NOT NULL / CHECK)进来时,日志会自己说明是哪个约束。
                // 文案由 `duplicate_fingerprint_log` 组装(格式与 KTD4 的指纹截断有单测)。
                ::log::debug!(
                    target: "watcher::capture",
                    "{}",
                    duplicate_fingerprint_log(&e.to_string(), &hash)
                );
            }
            Err(e) => {
                ::log::error!(target: "watcher::capture", "clipvault watcher: insert failed: {e}");
            }
        }
    }
}

/// 25MB payload 上限（multi-type plan R23）。超限 watcher 跳过本次
/// 捕获并写一行 `[capture] skipped: payload too large` 日志,不弹用户
/// 提示 toast（避免误打扰）。
pub const MAX_PAYLOAD_BYTES: usize = 25 * 1024 * 1024;
