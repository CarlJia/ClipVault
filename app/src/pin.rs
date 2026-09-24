//! Pin 置顶逻辑（U7）。
//!
//! `⌘P` 切换选中条目的置顶状态（R16）。核心是一次 SQLite 事务里的
//! 「读当前状态 → 取反 → 写 `is_pinned` + `pinned_at` → 读回整行」，
//! 返回刷新后的 [`Entry`] 供前端就地重绘。
//!
//! **`pinned_at` 语义（本单元锁定）：**
//! - 置顶时：`pinned_at = strftime('%s','now')` 等价值——用
//!   [`SystemTime`] 取 Unix 秒，与 U2 `created_at` 的单位一致。
//! - 取消置顶时：`pinned_at = NULL`。选择 NULL 而非保留旧值，是因为
//!   排序（U5 `PIN_ORDER`）只对 `is_pinned = 1` 的行看 `pinned_at`，
//!   NULL 化最干净、不留悬垂时间戳，也让「取消后再置顶」拿到全新的
//!   置顶时刻、正确排到置顶组最前（AE8）。
//!
//! 排序本身在 U5 的 `search`/`storage` 里已用
//! `is_pinned DESC, pinned_at DESC, created_at DESC` 落地；本单元只
//! 负责翻转状态，不改排序。

use std::time::{SystemTime, UNIX_EPOCH};

use clipvault_core::Entry;
use rusqlite::{Result as SqliteResult, params};

use crate::storage::Storage;

/// 读回整行 entry 的列清单（顺序须与 [`Entry::from_sqlite_row`] 对齐）。
const SELECT_ONE: &str = "SELECT id, content_hash, content, note, auto_category, \
     is_pinned, pinned_at, is_vault, is_decryptable, created_at \
     FROM entries WHERE id = ?1";

/// 切换 `id` 条目的置顶状态，返回刷新后的 [`Entry`]。
///
/// 在单个事务内完成翻转，避免并发读写下的竞态。
///
/// # Errors
/// - 条目不存在 → [`rusqlite::Error::QueryReturnedNoRows`]。
/// - 其它 SQLite 错误按原样上抛。
pub async fn toggle_pin(storage: &Storage, id: i64) -> SqliteResult<Entry> {
    // 在锁外取当前时刻，锁内只做纯 SQL。
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs() as i64;

    storage
        .with_conn(move |conn| {
            let tx = conn.transaction()?;

            // 读当前置顶状态（条目不存在 → QueryReturnedNoRows）。
            let is_pinned_now: i64 = tx.query_row(
                "SELECT is_pinned FROM entries WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )?;

            let new_pinned = is_pinned_now == 0;
            let pinned_at: Option<i64> = if new_pinned { Some(now) } else { None };

            tx.execute(
                "UPDATE entries SET is_pinned = ?1, pinned_at = ?2 WHERE id = ?3",
                params![new_pinned as i32, pinned_at, id],
            )?;

            let entry = tx.query_row(SELECT_ONE, params![id], Entry::from_sqlite_row)?;

            tx.commit()?;
            Ok(entry)
        })
        .await
}
