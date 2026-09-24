//! Entry model for ClipVault clipboard entries.

use serde::{Deserialize, Serialize};

/// A clipboard entry stored in ClipVault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Primary key.
    pub id: i64,
    /// SHA-256 fingerprint of the raw content (first 12 bytes as hex).
    /// For vault entries this is `hash(random_salt || cid)` — U9 territory.
    pub content_hash: String,
    /// Raw text content.
    pub content: String,
    /// Optional user note (up to 2000 chars in UI).
    pub note: String,
    /// Auto-detected category.
    pub auto_category: String,
    /// Whether the entry is pinned.
    pub is_pinned: bool,
    /// Unix timestamp (seconds) when pinned, or None.
    pub pinned_at: Option<i64>,
    /// Whether this entry lives in the encrypted vault.
    pub is_vault: bool,
    /// Whether the vault content is decryptable.
    /// `1` = Keychain key available, decrypt should succeed.
    /// `0` = Keychain key missing (e.g. machine migration), content unreadable.
    pub is_decryptable: bool,
    /// Unix timestamp (seconds) when the entry was first captured.
    pub created_at: i64,
}

impl Entry {
    /// Construct an `Entry` from a raw database row (index-based).
    #[allow(clippy::too_many_arguments)]
    pub fn from_row(
        id: i64,
        content_hash: String,
        content: String,
        note: String,
        auto_category: String,
        is_pinned: i32,
        pinned_at: Option<i64>,
        is_vault: i32,
        is_decryptable: i32,
        created_at: i64,
    ) -> Self {
        Self {
            id,
            content_hash,
            content,
            note,
            auto_category,
            is_pinned: is_pinned != 0,
            pinned_at,
            is_vault: is_vault != 0,
            is_decryptable: is_decryptable != 0,
            created_at,
        }
    }

    /// 从一行 SQLite 查询结果构造 `Entry`。
    ///
    /// 列顺序须与 `SELECT` 保持一致：
    /// `id, content_hash, content, note, auto_category, is_pinned,
    ///  pinned_at, is_vault, is_decryptable, created_at`。
    /// bool 列（`is_pinned` / `is_vault` / `is_decryptable`）沿用
    /// [`Entry::from_row`] 的「非 0 即真」coercion。
    pub fn from_sqlite_row(row: &rusqlite::Row) -> rusqlite::Result<Entry> {
        Ok(Entry::from_row(
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
        ))
    }
}
