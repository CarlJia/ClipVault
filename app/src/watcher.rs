//! 后台剪贴板监听器（U3 / R1 / R3 / R4 / R11 部分）。
//!
//! 一个 `tokio::task` 每 100ms 轮询 `NSPasteboard.general` 的 `changeCount`
//! （非破坏性读取）。当 `changeCount` 增长时读取 `.string(forType: .string)`，
//! 经去重、skip-once、访客模式过滤后，做自动分类并写入 SQLite。
//!
//! 轮询间隔取 100ms：平均检测延迟约 50ms、峰值 ≤ 100ms，满足 R1 的 < 200ms 预算。
//!
//! macOS 之外的平台没有 `NSPasteboard`，`poll_change` 返回 `None`，
//! 便于在任意平台跑确定性单元测试（测试只驱动 [`Watcher::process`] 与
//! [`should_dedup`]，不触碰真实剪贴板）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::classify::classify;
use crate::storage::Storage;

/// 轮询间隔（R1 修订：< 200ms 检测预算）。
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 去重时间窗:同一指纹在该时长内重复出现视为重复,不入库。
const DEDUP_WINDOW: Duration = Duration::from_secs(5);
/// 指纹长度:SHA-256 十六进制的前 16 个字符。
const HASH_PREFIX_LEN: usize = 16;

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
    last_change_count: AtomicI64,
}

impl Watcher {
    /// 构造监听器。
    ///
    /// `last_change_count` 初始化为 `-1`（不可能的真实值),保证首次轮询若剪贴板
    /// 非空会捕获一次。
    pub fn new(
        storage: Arc<Storage>,
        guest_mode: Arc<AtomicBool>,
        skip_next: Arc<AtomicBool>,
    ) -> Self {
        Self {
            storage,
            guest_mode,
            skip_next,
            last_hash: Mutex::new(None),
            last_change_count: AtomicI64::new(-1),
        }
    }

    /// 运行监听主循环（永不返回)。由调用方 `tokio::spawn`。
    pub async fn run(self) {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Some(text) = self.poll_change() {
                self.process(text).await;
            }
        }
    }

    /// 读取剪贴板变化（macOS 实现)。
    ///
    /// 当 `changeCount` 相较上次增长时返回其纯文本内容,否则返回 `None`。
    #[cfg(target_os = "macos")]
    fn poll_change(&self) -> Option<String> {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};

        // NSPasteboard.generalPasteboard 不限主线程,可在 tokio 工作线程调用。
        let pasteboard = NSPasteboard::generalPasteboard();
        let change_count = pasteboard.changeCount() as i64;

        let prev = self.last_change_count.swap(change_count, Ordering::SeqCst);
        if change_count == prev {
            return None;
        }

        let value = unsafe { pasteboard.stringForType(NSPasteboardTypeString) };
        value.map(|s| s.to_string())
    }

    /// 非 macOS 平台无 `NSPasteboard`,始终返回 `None`。
    #[cfg(not(target_os = "macos"))]
    fn poll_change(&self) -> Option<String> {
        None
    }

    /// 处理一段新捕获的文本:去重 + skip-once + 访客模式过滤 + 分类 + 入库。
    pub async fn process(&self, text: String) {
        // 1. 空白直接忽略。
        if text.trim().is_empty() {
            return;
        }

        // 2. 计算指纹。
        let hash = hash_prefix(&text);
        let now = Instant::now();

        // 3. 去重判定:锁内比对并更新指纹,入库前释放锁(不跨 await 持锁)。
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

        // 4. skip-once:消费并复位;命中则本次不入库(R11)。
        if self.skip_next.swap(false, Ordering::SeqCst) {
            return;
        }

        // 5. 访客模式:开启时不入库(R10;开关由 U10 写入)。
        if self.guest_mode.load(Ordering::SeqCst) {
            return;
        }

        // 6. 自动分类并写入。
        let category = classify(&text);
        match self
            .storage
            .insert_entry(&hash, &text, category.as_str())
            .await
        {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                // content_hash UNIQUE 冲突:该指纹此前已入库,视为重复,静默忽略。
            }
            Err(e) => {
                eprintln!("clipvault watcher: insert failed: {e}");
            }
        }
    }
}
