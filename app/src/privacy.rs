//! 访客模式 + 「本次不记录」隐私开关（U10 / R10 / R11）。
//!
//! # 两个开关，两种寿命
//!
//! - **访客模式（guest_mode，R10）**：*持久化*。开启后 watcher 不写入新条目
//!   （U3 已在 [`crate::watcher::Watcher::process`] 早返回），已有条目保持只读
//!   可访问。状态存进 `settings` 表（`"guest_mode" -> "1" / "0"`），跨重启保留。
//! - **「本次不记录」（skip_next，R11）**：*纯运行时*。置位后下一次复制不入库，
//!   U3 在入口 `swap(false)` 消费并复位（一次性，不污染后续）。**永不持久化**，
//!   开机始终为 `false`。
//!
//! # 单一事实来源
//!
//! 运行期以与 [`crate::watcher::Watcher`] 共享的 `Arc<AtomicBool>` 为准，
//! `settings` 表只是持久化层。写路径（[`set_guest_mode`]）*先*更新原子、*再*落库；
//! 启动路径（[`boot_load_guest_mode`]）从库读出后写回原子，使二者在开机时对齐。
//!
//! 本模块刻意保持“薄且纯”：不触碰托盘 / 前端，便于用内存或临时文件 DB 做确定性
//! 单元测试（见 `privacy_test.rs`）。托盘图标与前端开关的同步在 `commands.rs` /
//! `tray.rs` / `main.rs` 里围绕这些 helper 接线。

use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::Result as SqliteResult;

use crate::storage::Storage;

/// 访客模式在 `settings` 表中的键名。
pub const GUEST_MODE_KEY: &str = "guest_mode";

/// 把 `settings` 表里读到的原始值解析为布尔：仅 `"1"` 视为开启，
/// 其它（`None` / `"0"` / 任何非法值）一律视为关闭。纯函数，便于单测。
pub fn parse_guest_value(raw: Option<&str>) -> bool {
    matches!(raw, Some("1"))
}

/// 布尔 → 落库字符串（`"1"` / `"0"`）。
pub fn guest_value_str(enabled: bool) -> &'static str {
    if enabled { "1" } else { "0" }
}

/// 运行时应用访客模式：更新共享原子（单一事实来源）。同步、无 I/O。
pub fn apply_guest_mode(flag: &AtomicBool, enabled: bool) {
    flag.store(enabled, Ordering::SeqCst);
}

/// 从 `settings` 表读取访客模式（缺省 / 非法值 → `false`）。
pub async fn load_guest_mode(storage: &Storage) -> SqliteResult<bool> {
    let raw = storage.get_setting(GUEST_MODE_KEY).await?;
    Ok(parse_guest_value(raw.as_deref()))
}

/// 把访客模式持久化到 `settings` 表（不触碰原子）。
pub async fn persist_guest_mode(storage: &Storage, enabled: bool) -> SqliteResult<()> {
    storage
        .set_setting(GUEST_MODE_KEY, guest_value_str(enabled))
        .await
}

/// 写路径：更新共享原子（即时生效）*并*持久化到 `settings` 表。
///
/// 命令层（前端开关）与托盘 toggle 都经由它，保证 AtomicBool 与库始终一致。
pub async fn set_guest_mode(
    storage: &Storage,
    flag: &AtomicBool,
    enabled: bool,
) -> SqliteResult<()> {
    apply_guest_mode(flag, enabled);
    persist_guest_mode(storage, enabled).await
}

/// 启动路径：从 `settings` 表加载访客模式 → 写入共享原子 → 返回该值
/// （供托盘图标 / CheckMenuItem 反映初始状态）。在 spawn watcher 前调用。
pub async fn boot_load_guest_mode(storage: &Storage, flag: &AtomicBool) -> SqliteResult<bool> {
    let enabled = load_guest_mode(storage).await?;
    apply_guest_mode(flag, enabled);
    Ok(enabled)
}

/// 「本次不记录」：置位一次性 skip flag（R11）。
///
/// 纯运行时——**绝不**写库；watcher 在下一次捕获时 `swap(false)` 消费。
pub fn set_skip_next(flag: &AtomicBool) {
    flag.store(true, Ordering::SeqCst);
}
