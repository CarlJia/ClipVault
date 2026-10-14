//! Entry model for ClipVault clipboard entries.

use serde::{Deserialize, Serialize};

/// A clipboard entry stored in ClipVault.
///
/// Schema version 3 (`MIGRATION_V3`) extended this with five columns so
/// non-text clipboard kinds (image/file/rtf/color) can round-trip alongside
/// the original pure-text entries from MVP v1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Primary key.
    pub id: i64,
    /// SHA-256 fingerprint of the raw content (first 8 bytes as hex, 16 chars).
    /// For vault text entries this is `hash(random_salt || ciphertext)` (U9).
    /// For V3 binary entries the hash incorporates `content_kind` to avoid
    /// cross-kind collisions on identical bytes (multi-type plan K8).
    pub content_hash: String,
    /// Raw text content (plaintext or base64-encoded ciphertext for vault).
    /// For non-text kinds this is the empty string (descriptive text lives in
    /// `metadata_json`); the binary payload is in `payload`.
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

    // --- V3 columns (multi-type plan) ---
    /// Stable content-kind label from [`crate::ContentKind::as_str`].
    /// Always `"text"` for MVP-derived rows; one of `text` / `image` / `file`
    /// / `rtf` / `color` for multi-type captures.
    pub content_kind: String,
    /// MIME type for binary payloads (`image/png`, `application/rtf`, etc.).
    /// `None` for text entries.
    pub mime_type: Option<String>,
    /// Opaque binary payload (image bytes / RTF bytes / JSON-encoded file
    /// path array / color encoded data). `None` for text entries.
    pub payload: Option<Vec<u8>>,
    /// 12-byte AES-GCM nonce for the encrypted `payload` BLOB (vault binary
    /// entries only). Independent from `nonce` which protects the text
    /// `content` ciphertext. `None` for non-vault rows and for text entries.
    pub payload_nonce: Option<Vec<u8>>,
    /// Structured metadata JSON. Schema varies by `content_kind`:
    /// - `image`: `{width, height, size_bytes, thumb?}`
    /// - `file`: `{paths, resolved}`
    /// - `color`: `{rgba}`
    /// - `rtf`: `{}`
    /// - `text`: typically `None`.
    pub metadata_json: Option<String>,
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
        content_kind: String,
        mime_type: Option<String>,
        payload: Option<Vec<u8>>,
        payload_nonce: Option<Vec<u8>>,
        metadata_json: Option<String>,
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
            content_kind,
            mime_type,
            payload,
            payload_nonce,
            metadata_json,
        }
    }

    /// 从一行 SQLite 查询结果构造 `Entry`。
    ///
    /// 列顺序须与 `SELECT` 保持一致：
    /// `id, content_hash, content, note, auto_category, is_pinned,
    ///  pinned_at, is_vault, is_decryptable, created_at,
    ///  content_kind, mime_type, payload, payload_nonce, metadata_json`。
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
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
            row.get(13)?,
            row.get(14)?,
        ))
    }
}
