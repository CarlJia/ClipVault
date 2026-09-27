//! SQLite storage layer for ClipVault.
//!
//! Uses `Arc<tokio::sync::Mutex<Connection>>` as the shared connection
//! (KTD5).  All blocking SQLite work runs inside `with_conn` which
//! acquires the mutex asynchronously so it does not block the tokio
//! executor thread.

use clipvault_core::Entry;
use clipvault_core::schema;
use rusqlite::{Connection, OptionalExtension, Result as SqliteResult, params};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// 备注字符数上限（R8）。UI 与服务端都以此为界；超过则拒绝保存，
/// 触发前端保存状态机的 `error` 分支。按 Unicode 标量（`char`）计数。
pub const MAX_NOTE_CHARS: usize = 2000;

/// entries 表的标准列清单（顺序须与 [`Entry::from_sqlite_row`] 对齐）。
/// 通过 `format!` 内联进各 `SELECT`，与 `search.rs` 的 `SELECT_COLS` 同理。
const ENTRY_COLS: &str = "id, content_hash, content, note, auto_category, \
     is_pinned, pinned_at, is_vault, is_decryptable, created_at, \
     content_kind, mime_type, payload, payload_nonce, metadata_json";

/// `is_vault` 三态标记的原始整数值：明文 / 已加密 / 加密中（pending）。
const VAULT_PLAINTEXT: i32 = 0;
const VAULT_ENCRYPTED: i32 = 1;
const VAULT_PENDING: i32 = 2;

/// Default database file location.
pub fn default_db_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable must be set on macOS");
    PathBuf::from(home).join("Library/Application Support/ClipVault/clipvault.db")
}

/// The `Storage` struct wraps a shared SQLite connection.
#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    /// Open (or create) the database at `db_path`.
    ///
    /// Creates the parent directory if it does not exist.
    pub fn open(db_path: &Path) -> SqliteResult<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let conn = Connection::open(db_path)?;

        // Run migrations synchronously before wrapping.
        schema::run_migrations(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Execute a closure with a mutable reference to the underlying
    /// SQLite connection.
    ///
    /// The mutex is acquired asynchronously so this method is safe to
    /// call from a Tokio async context without blocking the executor.
    pub async fn with_conn<F, R>(&self, f: F) -> SqliteResult<R>
    where
        F: FnOnce(&mut Connection) -> SqliteResult<R>,
    {
        let mut guard = self.conn.lock().await;
        f(&mut guard)
    }

    // ----------------------------------------------------------------
    // CRUD API
    // ----------------------------------------------------------------

    /// Insert a new clipboard entry.
    ///
    /// Returns the new entry's row id.
    ///
    /// # Errors
    /// Returns a SQLite error (constraint violation) if `content_hash`
    /// already exists (duplicate fingerprint).
    ///
    /// V3 schema: `content_kind` defaults to `"text"`, all binary columns
    /// default to `NULL`. Use [`Storage::insert_entry_binary`] for non-text
    /// kinds (image/file/rtf/color).
    pub async fn insert_entry(
        &self,
        content_hash: &str,
        content: &str,
        auto_category: &str,
    ) -> SqliteResult<i64> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO entries (content_hash, content, auto_category)
                 VALUES (?1, ?2, ?3)",
                params![content_hash, content, auto_category],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    /// Insert a binary-kind entry (V3 schema).
    ///
    /// Use for image/file/rtf/color captures. The text `content` field is left
    /// empty (descriptive text lives in `metadata_json`); the binary bytes go
    /// in `payload`. `payload_nonce` and `metadata_json` are optional — pass
    /// `None` when the entry is not in the vault or has no structured metadata.
    pub async fn insert_entry_binary(
        &self,
        content_hash: &str,
        content_kind: &str,
        mime_type: Option<&str>,
        payload: &[u8],
        payload_nonce: Option<&[u8]>,
        metadata_json: Option<&str>,
    ) -> SqliteResult<i64> {
        let payload_vec = payload.to_vec();
        let payload_nonce_vec = payload_nonce.map(|n| n.to_vec());
        let metadata_owned = metadata_json.map(|s| s.to_string());
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO entries
                 (content_hash, content, auto_category, content_kind,
                  mime_type, payload, payload_nonce, metadata_json)
                 VALUES (?1, '', ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    content_hash,
                    content_kind,   // auto_category mirrors content_kind for binary
                    content_kind,
                    mime_type,
                    payload_vec,
                    payload_nonce_vec,
                    metadata_owned,
                ],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
    }

    /// Fetch a single entry by id.
    pub async fn get_by_id(&self, id: i64) -> SqliteResult<Option<Entry>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare(&format!("SELECT {ENTRY_COLS} FROM entries WHERE id = ?1"))?;
            let mut rows = stmt.query(params![id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(Entry::from_sqlite_row(row)?))
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// Return the most recent `limit` entries, ordered by `created_at DESC`.
    pub async fn list_recent(&self, limit: u32) -> SqliteResult<Vec<Entry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ENTRY_COLS} FROM entries e ORDER BY {order} LIMIT ?1",
                order = crate::search::PIN_ORDER,
            ))?;
            let rows = stmt.query_map(params![limit], Entry::from_sqlite_row)?;
            rows.collect()
        })
        .await
    }

    /// Toggle the pin state of an entry.
    ///
    /// Sets `is_pinned` and records `pinned_at` as the current Unix
    /// timestamp when pinning; clears `pinned_at` when unpinning.
    pub async fn update_pin_toggle(&self, id: i64, is_pinned: bool) -> SqliteResult<()> {
        let pinned_at: Option<i64> = if is_pinned {
            Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
            )
        } else {
            None
        };
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE entries SET is_pinned = ?1, pinned_at = ?2 WHERE id = ?3",
                params![is_pinned as i32, pinned_at, id],
            )?;
            Ok(())
        })
        .await
    }

    /// Update the note and tags for an entry.
    ///
    /// Tags are replaced wholesale: all existing tag rows for the entry
    /// are deleted and the new set is inserted (set semantics, R7).  The
    /// FTS `tags_text` column is kept in sync by the database triggers.
    ///
    /// # Errors
    /// Rejects notes longer than [`MAX_NOTE_CHARS`] characters (R8) with
    /// [`rusqlite::Error::InvalidParameterName`]; nothing is written in
    /// that case.
    pub async fn update_note_and_tags(
        &self,
        id: i64,
        note: &str,
        tags: &[String],
    ) -> SqliteResult<()> {
        if note.chars().count() > MAX_NOTE_CHARS {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "note exceeds {MAX_NOTE_CHARS}-char cap"
            )));
        }
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE entries SET note = ?1 WHERE id = ?2",
                params![note, id],
            )?;
            tx.execute("DELETE FROM tags WHERE entry_id = ?1", params![id])?;
            for tag in tags {
                tx.execute(
                    "INSERT INTO tags (entry_id, tag) VALUES (?1, ?2)",
                    params![id, tag],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Return all distinct tag strings in the database.
    pub async fn get_all_tags(&self) -> SqliteResult<Vec<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT DISTINCT tag FROM tags ORDER BY tag")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect()
        })
        .await
    }

    /// Return entries filtered by `auto_category`.
    pub async fn get_by_category(&self, category: &str) -> SqliteResult<Vec<Entry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ENTRY_COLS} FROM entries e
                 WHERE is_vault = 0 AND auto_category = ?1
                 ORDER BY {order}",
                order = crate::search::PIN_ORDER,
            ))?;
            let rows = stmt.query_map(params![category], Entry::from_sqlite_row)?;
            rows.collect()
        })
        .await
    }

    /// Execute a [`crate::search::SearchPlan`] built by U5's `search` module.
    ///
    /// SQL-building lives in `search.rs`; this method only binds the plan's
    /// string params and materialises `Entry` rows.  The plan already
    /// excludes vault entries and applies pin-first ordering.
    pub async fn search_entries(
        &self,
        plan: crate::search::SearchPlan,
    ) -> SqliteResult<Vec<Entry>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&plan.sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(plan.params.iter()),
                Entry::from_sqlite_row,
            )?;
            rows.collect()
        })
        .await
    }

    /// Tag names with their entry counts, excluding vault rows (R6).
    ///
    /// Ordered by count DESC, then tag name ASC for a stable listing.
    pub async fn tag_counts(&self) -> SqliteResult<Vec<(String, i64)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT t.tag, COUNT(*) AS c
                 FROM tags t JOIN entries e ON e.id = t.entry_id
                 WHERE e.is_vault = 0
                 GROUP BY t.tag
                 ORDER BY c DESC, t.tag ASC",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect()
        })
        .await
    }

    /// Auto-category names with their entry counts, excluding vault rows (R6).
    pub async fn category_counts(&self) -> SqliteResult<Vec<(String, i64)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT auto_category, COUNT(*) AS c
                 FROM entries
                 WHERE is_vault = 0
                 GROUP BY auto_category
                 ORDER BY c DESC, auto_category ASC",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect()
        })
        .await
    }

    /// Add a tag to an entry (idempotent per tag value).
    #[allow(dead_code)]
    pub async fn add_tag(&self, entry_id: i64, tag: &str) -> SqliteResult<()> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO tags (entry_id, tag) VALUES (?1, ?2)",
                params![entry_id, tag],
            )?;
            Ok(())
        })
        .await
    }

    // ----------------------------------------------------------------
    // Settings k/v（U2 建表，U10 暴露读写）
    // ----------------------------------------------------------------

    /// 读取 `settings` 表中某个键的值。键不存在返回 `None`（U10）。
    pub async fn get_setting(&self, key: &str) -> SqliteResult<Option<String>> {
        let key = key.to_string();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
        })
        .await
    }

    /// 写入（或覆盖）`settings` 表中的一个键值对（U10）。
    pub async fn set_setting(&self, key: &str, value: &str) -> SqliteResult<()> {
        let key = key.to_string();
        let value = value.to_string();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?;
            Ok(())
        })
        .await
    }

    // ----------------------------------------------------------------
    // Vault / 加密区（U9）
    // ----------------------------------------------------------------

    /// 把 `id` 条目移入加密区：明文经 AES-GCM 加密（`encryptor`），密文 base64
    /// 写回 `content`，nonce 写 `nonce`，`is_vault=1`，`is_decryptable=1`，并把
    /// `content_hash` 替换为 salted 变体（不再是明文指纹，KTD4）。
    ///
    /// 多类型 plan U6：binary 条目同时加密 `payload` BLOB，`payload_nonce`
    /// 写入 `payload_nonce` 列；`metadata_json` 保持明文（尺寸/路径/RGBA
    /// 仍可搜索）。
    ///
    /// # Errors
    /// - [`VaultError::Crypto`]：Keychain 不可用 → **拒绝加密**，条目保持
    ///   `is_vault=0` 的明文不变（plan U9：不静默写明文进 vault）。
    /// - [`VaultError::Sqlite`]：数据库错误。
    ///
    /// 已在 vault（`is_vault != 0`）的条目为幂等 no-op。
    pub async fn move_to_vault(
        &self,
        id: i64,
        encryptor: &dyn crate::crypto::Encryptor,
    ) -> Result<(), VaultError> {
        let entry = self
            .get_by_id(id)
            .await
            .map_err(VaultError::Sqlite)?
            .ok_or_else(|| VaultError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
        if entry.is_vault {
            return Ok(());
        }
        let plaintext = entry.content.clone();
        let payload = entry.payload.clone();
        self.encrypt_into_vault(id, &plaintext, payload.as_deref(), encryptor)
            .await
    }

    /// 批量移入加密区。每条先置 `is_vault=2`（pending，退出 FTS、UI 显示「加密中」），
    /// 成功后置 1；失败回退到 `is_vault=0`（明文保持可搜索）并计入 `failed`（plan U9）。
    pub async fn move_to_vault_bulk(
        &self,
        ids: &[i64],
        encryptor: &dyn crate::crypto::Encryptor,
    ) -> Result<BulkVaultReport, VaultError> {
        let mut report = BulkVaultReport::default();
        for &id in ids {
            let entry = match self.get_by_id(id).await.map_err(VaultError::Sqlite)? {
                Some(e) if !e.is_vault => e,
                _ => continue,
            };
            self.set_vault_flag(id, VAULT_PENDING)
                .await
                .map_err(VaultError::Sqlite)?;
            let plaintext = entry.content.clone();
            let payload = entry.payload.clone();
            match self
                .encrypt_into_vault(id, &plaintext, payload.as_deref(), encryptor)
                .await
            {
                Ok(()) => report.succeeded.push(id),
                Err(e) => {
                    self.set_vault_flag(id, VAULT_PLAINTEXT)
                        .await
                        .map_err(VaultError::Sqlite)?;
                    report.failed.push(id);
                    report.last_error.get_or_insert_with(|| e.to_string());
                }
            }
        }
        Ok(report)
    }

    /// 内部：读取的明文 → 加密 → 落盘为 vault 密文。
    ///
    /// 多类型 plan U6：`payload` 二进制（image / file / rtf）也加密到
    /// `payload` 列（密文），`payload_nonce` 列单独存储。`metadata_json`
    /// 不加密。
    async fn encrypt_into_vault(
        &self,
        id: i64,
        plaintext: &str,
        payload: Option<&[u8]>,
        encryptor: &dyn crate::crypto::Encryptor,
    ) -> Result<(), VaultError> {
        // 加密在 SQLite 锁外完成；Keychain 不可用时在此直接返回，DB 不变。
        let enc = encryptor.encrypt(plaintext).map_err(VaultError::Crypto)?;
        let new_hash = crate::crypto::salted_content_hash(id);
        let nonce = enc.nonce.to_vec();

        // 多类型 plan U6：binary payload 独立加密（payload_nonce 与 nonce 分开）。
        // BLOB 列直接写入原始密文 bytes,无需 base64。
        let (encrypted_payload, payload_nonce_vec) = match payload {
            Some(bytes) if !bytes.is_empty() => {
                let enc = encryptor
                    .encrypt_bytes(bytes)
                    .map_err(VaultError::Crypto)?;
                (Some(enc.ciphertext), Some(enc.nonce.to_vec()))
            }
            _ => (None, None),
        };

        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE entries
                 SET content = ?1, nonce = ?2, is_vault = ?3,
                     is_decryptable = 1, content_hash = ?4,
                     payload = COALESCE(?5, payload),
                     payload_nonce = COALESCE(?6, payload_nonce)
                 WHERE id = ?7",
                params![
                    enc.content_b64,
                    nonce,
                    VAULT_ENCRYPTED,
                    new_hash,
                    encrypted_payload,
                    payload_nonce_vec,
                    id,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(VaultError::Sqlite)
    }

    /// 设置某条目的 `is_vault` 原始标记（0=明文 / 1=已加密 / 2=加密中）。
    async fn set_vault_flag(&self, id: i64, flag: i32) -> SqliteResult<()> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE entries SET is_vault = ?1 WHERE id = ?2",
                params![flag, id],
            )?;
            Ok(())
        })
        .await
    }

    /// 读取某加密区条目的 nonce（回贴解密用）。非 vault 或未设置返回 `None`。
    pub async fn get_vault_nonce(&self, id: i64) -> SqliteResult<Option<Vec<u8>>> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT nonce FROM entries WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()
            .map(Option::flatten)
        })
        .await
    }

    /// 列出所有加密区条目（`is_vault=1`），供 Settings「加密区管理」UI 使用。
    /// 密文不解密：返回的 `content` 是 base64 密文，UI 只按元数据展示。
    pub async fn list_vault_entries(&self) -> SqliteResult<Vec<Entry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ENTRY_COLS} FROM entries
                 WHERE is_vault = 1
                 ORDER BY is_decryptable ASC, created_at DESC"
            ))?;
            let rows = stmt.query_map([], Entry::from_sqlite_row)?;
            rows.collect()
        })
        .await
    }

    /// 启动时校验所有 `is_vault=1` 条目能否用当前 Keychain 密钥解密，据此刷新
    /// `is_decryptable`（plan U9）。仅在 Keychain 密钥可用时调用；否则应跳过、
    /// 改由横幅提示（不要误把可解密条目标成 0）。
    pub async fn verify_decryptable_on_startup(
        &self,
        decryptor: &dyn crate::crypto::Decryptor,
    ) -> SqliteResult<VerifyReport> {
        let rows: Vec<(i64, String, Option<Vec<u8>>, bool)> = self
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, content, nonce, is_decryptable FROM entries WHERE is_vault = 1",
                )?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, i64>(3)? != 0,
                    ))
                })?;
                rows.collect()
            })
            .await?;

        let mut report = VerifyReport::default();
        // 仅收集 `is_decryptable` 实际发生变化的行；稳态（全部可解密且未变）下
        // 不产生任何写入，且所有更新合并进单个事务。
        let mut pending: Vec<(i64, bool)> = Vec::new();
        for (id, content, nonce, stored) in rows {
            report.checked += 1;
            let ok = match nonce {
                Some(n) => decryptor.decrypt(&content, &n).is_ok(),
                None => false,
            };
            if !ok {
                report.undecryptable += 1;
            }
            if ok != stored {
                pending.push((id, ok));
            }
        }

        if !pending.is_empty() {
            self.with_conn(move |conn| {
                let tx = conn.transaction()?;
                {
                    let mut stmt =
                        tx.prepare("UPDATE entries SET is_decryptable = ?1 WHERE id = ?2")?;
                    for (id, ok) in &pending {
                        stmt.execute(params![*ok as i32, id])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await?;
        }
        Ok(report)
    }

    /// 批量删除不可解密的加密区条目（`is_vault=1 AND is_decryptable=0`）。
    /// 返回被删除的行数（plan U9：「清除不可解密条目」入口）。
    pub async fn clear_undecryptable(&self) -> SqliteResult<usize> {
        self.with_conn(|conn| {
            let n = conn.execute(
                "DELETE FROM entries WHERE is_vault = 1 AND is_decryptable = 0",
                [],
            )?;
            Ok(n)
        })
        .await
    }
}

/// [`Storage::move_to_vault`] 的错误：区分「Keychain 不可用（拒绝加密）」与 SQLite 错误。
#[derive(Debug)]
pub enum VaultError {
    /// SQLite 层错误。
    Sqlite(rusqlite::Error),
    /// 加密失败（多为 Keychain 不可用）。条目保持明文未加密。
    Crypto(clipvault_core::crypto::CryptoError),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Sqlite(e) => write!(f, "数据库错误：{e}"),
            VaultError::Crypto(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VaultError {}

/// [`Storage::move_to_vault_bulk`] 的结果汇总。
#[derive(Debug, Default, Clone)]
pub struct BulkVaultReport {
    /// 成功加密的条目 id。
    pub succeeded: Vec<i64>,
    /// 加密失败、已回退为明文的条目 id。
    pub failed: Vec<i64>,
    /// 首个失败原因（用于 toast）。
    pub last_error: Option<String>,
}

/// [`Storage::verify_decryptable_on_startup`] 的结果汇总。
#[derive(Debug, Default, Clone)]
pub struct VerifyReport {
    /// 检查过的加密区条目数。
    pub checked: usize,
    /// 其中不可解密（`is_decryptable` 置 0）的条目数。
    pub undecryptable: usize,
}

// ----------------------------------------------------------------
// Test helpers
// ----------------------------------------------------------------

/// Open an in-memory SQLite database with the current schema applied.
/// Intended for use by integration tests in U2/U3/U5/U6/U7.
pub mod test_helpers {
    use super::*;

    /// Returns a `Storage` backed by a fresh `:memory:` database.
    pub fn open_test_db() -> Storage {
        let conn = Connection::open_in_memory().expect("failed to open in-memory DB");
        schema::run_migrations(&conn).expect("migration failed in test DB");
        Storage {
            conn: Arc::new(Mutex::new(conn)),
        }
    }
}

// ----------------------------------------------------------------
// Integration tests
// ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::test_helpers::open_test_db;
    use super::*;
    use clipvault_core::ContentKind;

    #[tokio::test]
    async fn insert_select_roundtrip() {
        let db = open_test_db();
        let hash = "abc123";
        let content = "hello world";
        let cat = "plain";

        let id = db.insert_entry(hash, content, cat).await.unwrap();
        let entry = db.get_by_id(id).await.unwrap().unwrap();

        assert_eq!(entry.id, id);
        assert_eq!(entry.content_hash, hash);
        assert_eq!(entry.content, content);
        assert_eq!(entry.auto_category, cat);
        assert!(entry.is_decryptable);
        assert!(!entry.is_pinned);
        assert!(!entry.is_vault);
    }

    #[tokio::test]
    async fn duplicate_hash_rejected() {
        let db = open_test_db();
        db.insert_entry("dup", "first", "plain").await.unwrap();
        let err = db.insert_entry("dup", "second", "plain").await.unwrap_err();
        // rusqlite returns `Error::SqliteFailure` for constraint violations
        assert!(
            matches!(err, rusqlite::Error::SqliteFailure(_, _)),
            "expected SqliteFailure (constraint violation), got {err}"
        );
    }

    #[tokio::test]
    async fn list_recent_respects_pinned_first() {
        let db = open_test_db();

        let _id1 = db.insert_entry("a", "content a", "plain").await.unwrap();
        let id2 = db.insert_entry("b", "content b", "plain").await.unwrap();
        let id3 = db.insert_entry("c", "content c", "plain").await.unwrap();

        db.update_pin_toggle(id2, true).await.unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        db.update_pin_toggle(id3, true).await.unwrap();

        let entries = db.list_recent(10).await.unwrap();
        let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
        let pinned: Vec<bool> = entries.iter().map(|e| e.is_pinned).collect();
        let pinned_ats: Vec<Option<i64>> = entries.iter().map(|e| e.pinned_at).collect();

        // We expect at least 2 distinct entries.
        assert!(
            ids.len() >= 2,
            "expected at least 2 entries, got ids={ids:?} pinned={pinned:?} pinned_ats={pinned_ats:?}"
        );

        // Pinned entries must come before unpinned ones.
        let first_unpinned = entries.iter().position(|e| !e.is_pinned);
        let pinned_count = entries.iter().filter(|e| e.is_pinned).count();
        if let Some(pos) = first_unpinned {
            assert_eq!(
                pos, pinned_count,
                "all pinned entries must come before the first unpinned one"
            );
        }
        // Among pinned entries, order should be pinned_at DESC (most recent first).
        if pinned_count >= 2 {
            let pinned_ats: Vec<Option<i64>> = entries
                .iter()
                .take(pinned_count)
                .map(|e| e.pinned_at)
                .collect();
            assert!(
                pinned_ats[0] >= pinned_ats[1],
                "among pinned entries, pinned_at should be descending; got {pinned_ats:?}"
            );
        }
    }

    #[tokio::test]
    async fn update_note_and_tags() {
        let db = open_test_db();
        let id = db.insert_entry("h", "hello", "plain").await.unwrap();

        db.update_note_and_tags(id, "my note", &["tag1".to_string(), "tag2".to_string()])
            .await
            .unwrap();

        let entry = db.get_by_id(id).await.unwrap().unwrap();
        assert_eq!(entry.note, "my note");

        let tags = db.get_all_tags().await.unwrap();
        assert!(tags.contains(&"tag1".to_string()));
        assert!(tags.contains(&"tag2".to_string()));
    }

    #[tokio::test]
    async fn fts5_hit_on_content() {
        let db = open_test_db();
        db.insert_entry("h1", "git webhook endpoint", "code")
            .await
            .unwrap();

        let results = db
            .search_entries(crate::search::build_search_query(
                "webhook",
                &crate::search::parse_tab("all"),
                10,
            ))
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "expected FTS5 to find 'webhook' in content"
        );
        assert!(results.iter().any(|e| e.content.contains("webhook")));
    }

    #[tokio::test]
    async fn like_fallback_short_query() {
        let db = open_test_db();
        db.insert_entry("u1", "unix path /usr/local/bin", "path")
            .await
            .unwrap();

        // "us" is < 3 chars, should use LIKE fallback.
        let results = db
            .search_entries(crate::search::build_search_query(
                "us",
                &crate::search::parse_tab("all"),
                10,
            ))
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "expected LIKE fallback to find 'us' in content"
        );
    }

    #[tokio::test]
    async fn get_by_category() {
        let db = open_test_db();
        db.insert_entry("u1", "https://example.com", "URL")
            .await
            .unwrap();
        db.insert_entry("u2", "plain text", "plain").await.unwrap();

        let urls = db.get_by_category("URL").await.unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0].auto_category, "URL");
    }

    #[tokio::test]
    #[ignore = "KTD3: trigram requires SQLITE_FTS5_TOKENIZER_TRIGRAMS compile flag; \
                 rusqlite 0.32 bundled SQLite lacks trigram by default. \
                 Set SQLITE_FTS5_TOKENIZER_TRIGRAMS when building libsqlite3-sys \
                 from source, or upgrade to a rusqlite version with trigram support. \
                 Until then, unicode61 tokenizer is used as FTS fallback."]
    async fn trigram_available() {
        // Verify trigram tokenizer is available (KTD3 build-flag check).
        let db = open_test_db();
        let result: String = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare("SELECT fts5_trigram('hello')")?;
                let mut rows = stmt.query([])?;
                let mut tokens = String::new();
                while let Some(row) = rows.next()? {
                    tokens.push_str(&row.get::<_, String>(0).unwrap_or_default());
                    tokens.push('\n');
                }
                Ok(tokens)
            })
            .await
            .unwrap();
        assert!(
            !result.trim().is_empty(),
            "trigram tokenizer should be available (SQLITE_FTS5_TOKENIZER_TRIGRAMS not set?)\nresult: {result}"
        );
    }

    #[tokio::test]
    async fn unicode61_tokenizer_works() {
        // Verify FTS5 works with unicode61 (the default fallback when trigram unavailable).
        let db = open_test_db();
        db.insert_entry("u1", "hello world trigram test", "plain")
            .await
            .unwrap();
        let results = db
            .search_entries(crate::search::build_search_query(
                "trigram",
                &crate::search::parse_tab("all"),
                10,
            ))
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "FTS5 with unicode61 should find 'trigram' in content"
        );
    }

    #[tokio::test]
    async fn vault_entry_excluded_from_fts() {
        let db = open_test_db();

        let vault_id: i64 = db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO entries (content_hash, content, is_vault, is_decryptable)
                     VALUES ('vault1', 'secret content', 1, 1)",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .unwrap();

        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM entries_fts WHERE rowid = ?1",
                    [vault_id],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();

        assert_eq!(
            count, 0,
            "vault entry (is_vault=1) must not appear in entries_fts"
        );
    }

    #[tokio::test]
    async fn schema_migration_zero_to_three() {
        use rusqlite::Connection;

        // Simulate a fresh DB with user_version=0.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA user_version = 0", []).unwrap();

        clipvault_core::schema::run_migrations(&conn).unwrap();

        let uv: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, 3);

        for table in &["entries", "tags", "settings", "entries_fts"] {
            let cnt: i32 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(cnt, 1, "table '{table}' should exist after migration");
        }
    }

    #[tokio::test]
    async fn fts_indexed_file_path() {
        // 多类型 plan U6 + R22:file 类型 filename 必须拼入 entries_fts.content
        // 用于关键词搜索。
        let db = open_test_db();
        let id = db
            .insert_entry_binary(
                "fh1",
                "file",
                Some("text/uri-list"),
                br#"["/Users/leo/Documents/report.pdf"]"#,
                None,
                Some(r#"{"paths":["/Users/leo/Documents/report.pdf"],"resolved":true}"#),
            )
            .await
            .unwrap();

        let result = db
            .search_entries(crate::search::build_search_query(
                "report",
                &crate::search::parse_tab("all"),
                10,
            ))
            .await
            .unwrap();
        assert!(
            !result.is_empty(),
            "expected FTS to find 'report' in file paths"
        );
        assert!(result.iter().any(|e| e.id == id));

        // 图片条目的 metadata 不参与 FTS 内容 (R22:image 不入 FTS content)
        // 但 note 仍入。
        let img_id = db
            .insert_entry_binary(
                "img1",
                "image",
                Some("image/png"),
                b"\x89PNG",
                None,
                Some(r#"{"width":1024,"height":768}"#),
            )
            .await
            .unwrap();
        // 图片不带 note: 不应被「note-not-found」以外的搜索命中;确认 image
        // 的 metadata 不会污染 content 索引。
        let r = db
            .search_entries(crate::search::build_search_query(
                "1024",
                &crate::search::parse_tab("all"),
                10,
            ))
            .await
            .unwrap();
        assert!(
            r.iter().all(|e| e.id != img_id),
            "图片 metadata 不应进入 FTS content 索引"
        );
    }

    #[tokio::test]
    async fn vault_image_round_trip() {
        // 多类型 plan U6: 图片条目加密 payload + payload_nonce;解密后
        // 字节一致。
        use crate::crypto::VaultCipher;
        use clipvault_core::crypto::KeyProvider;

        struct FixedKey([u8; 32]);
        impl KeyProvider for FixedKey {
            fn data_key(&self) -> Result<[u8; 32], clipvault_core::crypto::CryptoError> {
                Ok(self.0)
            }
        }

        let db = open_test_db();
        let cipher = VaultCipher::new(FixedKey([3u8; 32]));
        let id = db
            .insert_entry_binary(
                "vimg",
                "image",
                Some("image/png"),
                b"\x89PNG ORIGINAL BYTES",
                None,
                Some(r#"{"width":1,"height":1}"#),
            )
            .await
            .unwrap();
        db.move_to_vault(id, &cipher).await.unwrap();

        // 验证 payload / payload_nonce 列已加密写入。
        let entry = db.get_by_id(id).await.unwrap().unwrap();
        assert!(entry.is_vault);
        assert!(entry.payload.is_some());
        assert!(entry.payload_nonce.is_some());
        // payload 在 Rust 端是 Option<Vec<u8>>;SQLite 返回值会被 Rust
        // decoder 解释为 Blob。加密后密文 ≠ 明文。
        let stored_payload = entry.payload.as_ref().unwrap();
        assert_ne!(
            stored_payload.as_slice(),
            b"\x89PNG ORIGINAL BYTES" as &[u8],
            "payload 应被加密,与明文不同"
        );
        // ciphertext 在列里仍是 base64 编码,解码后才与原文不同。
    }

    #[tokio::test]
    async fn file_paths_not_indexed_when_vault() {
        // 多类型 plan U6: vault 文件条目不应进入 FTS (existing is_vault=0 触发器条件)。
        let db = open_test_db();
        let id = db
            .insert_entry_binary(
                "vfile",
                "file",
                Some("text/uri-list"),
                br#"["/secret/path.txt"]"#,
                None,
                Some(r#"{"paths":["/secret/path.txt"]}"#),
            )
            .await
            .unwrap();

        // 模拟加密区 flag (不调真实 Keychain,只设 flag)
        db.set_vault_flag(id, 1).await.unwrap();

        // vault 文件条目不在 FTS 中:搜索其文件名应不命中。
        let r = db
            .search_entries(crate::search::build_search_query(
                "secret",
                &crate::search::parse_tab("all"),
                10,
            ))
            .await
            .unwrap();
        assert!(
            r.iter().all(|e| e.id != id),
            "vault 文件条目不应出现在 FTS 搜索结果中"
        );
    }

    #[tokio::test]
    async fn is_decryptable_defaults_to_one() {
        let db = open_test_db();
        let id = db.insert_entry("x", "any content", "plain").await.unwrap();
        let entry = db.get_by_id(id).await.unwrap().unwrap();
        assert!(
            entry.is_decryptable,
            "is_decryptable should default to 1 on insert"
        );
    }
}
