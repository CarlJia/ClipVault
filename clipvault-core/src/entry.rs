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
}
