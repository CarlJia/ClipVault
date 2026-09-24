//! SQLite storage layer for ClipVault.
//!
//! Uses `Arc<tokio::sync::Mutex<Connection>>` as the shared connection
//! (KTD5).  All blocking SQLite work runs inside `with_conn` which
//! acquires the mutex asynchronously so it does not block the tokio
//! executor thread.

use clipvault_core::Entry;
use clipvault_core::schema;
use rusqlite::{Connection, Result as SqliteResult, params};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// 备注字符数上限（R8）。UI 与服务端都以此为界；超过则拒绝保存，
/// 触发前端保存状态机的 `error` 分支。按 Unicode 标量（`char`）计数。
pub const MAX_NOTE_CHARS: usize = 2000;

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

    /// Fetch a single entry by id.
    pub async fn get_by_id(&self, id: i64) -> SqliteResult<Option<Entry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content_hash, content, note, auto_category,
                        is_pinned, pinned_at, is_vault, is_decryptable, created_at
                 FROM entries WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            if let Some(row) = rows.next()? {
                Ok(Some(Entry::from_row(
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
                )))
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// Return the most recent `limit` entries, ordered by `created_at DESC`.
    pub async fn list_recent(&self, limit: u32) -> SqliteResult<Vec<Entry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content_hash, content, note, auto_category,
                        is_pinned, pinned_at, is_vault, is_decryptable, created_at
                 FROM entries
                 ORDER BY is_pinned DESC,
                          CASE WHEN pinned_at IS NULL THEN 1 ELSE 0 END ASC,
                          pinned_at DESC,
                          created_at DESC
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit], |row| {
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
            })?;
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

    /// Full-text search using FTS5.
    ///
    /// Short queries (< 3 chars) fall back to a LIKE scan on `content`.
    ///
    /// **Note:** This is the U2 placeholder.  U5 will complete the
    /// multi-tab and fuzzy-search logic.
    pub async fn search(&self, query: &str, _tab: &str, limit: u32) -> SqliteResult<Vec<Entry>> {
        self.with_conn(|conn| {
            if query.is_empty() {
                return Self::list_recent_sync(conn, limit);
            }

            if query.len() < 3 {
                return Self::search_like(conn, query, limit);
            }

            // Sanitise query by stripping control characters.
            let safe_query: String = query.chars().filter(|c| !c.is_control()).collect();
            let fts_query = format!("\"{safe_query}\"*");

            let mut stmt = conn.prepare(
                "SELECT e.id, e.content_hash, e.content, e.note, e.auto_category,
                        e.is_pinned, e.pinned_at, e.is_vault, e.is_decryptable, e.created_at
                 FROM entries e
                 WHERE e.is_vault = 0
                   AND e.id IN (SELECT rowid FROM entries_fts WHERE entries_fts MATCH ?1)
                 ORDER BY is_pinned DESC,
                          CASE WHEN pinned_at IS NULL THEN 1 ELSE 0 END ASC,
                          pinned_at DESC,
                          created_at DESC
                 LIMIT ?2",
            )?;

            let rows = stmt.query_map(params![fts_query, limit], |row| {
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
            })?;
            rows.collect()
        })
        .await
    }

    fn list_recent_sync(conn: &mut Connection, limit: u32) -> SqliteResult<Vec<Entry>> {
        let mut stmt = conn.prepare(
            "SELECT id, content_hash, content, note, auto_category,
                    is_pinned, pinned_at, is_vault, is_decryptable, created_at
             FROM entries
             ORDER BY is_pinned DESC,
                      CASE WHEN pinned_at IS NULL THEN 1 ELSE 0 END ASC,
                      pinned_at DESC,
                      created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
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
        })?;
        rows.collect()
    }

    fn search_like(conn: &mut Connection, query: &str, limit: u32) -> SqliteResult<Vec<Entry>> {
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT id, content_hash, content, note, auto_category,
                    is_pinned, pinned_at, is_vault, is_decryptable, created_at
             FROM entries
             WHERE is_vault = 0 AND (content LIKE ?1 OR note LIKE ?1)
             ORDER BY is_pinned DESC,
                      CASE WHEN pinned_at IS NULL THEN 1 ELSE 0 END ASC,
                      pinned_at DESC,
                      created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit], |row| {
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
        })?;
        rows.collect()
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
            let mut stmt = conn.prepare(
                "SELECT id, content_hash, content, note, auto_category,
                        is_pinned, pinned_at, is_vault, is_decryptable, created_at
                 FROM entries
                 WHERE is_vault = 0 AND auto_category = ?1
                 ORDER BY is_pinned DESC,
                          CASE WHEN pinned_at IS NULL THEN 1 ELSE 0 END ASC,
                          pinned_at DESC,
                          created_at DESC",
            )?;
            let rows = stmt.query_map(params![category], |row| {
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
            })?;
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
            let rows = stmt.query_map(rusqlite::params_from_iter(plan.params.iter()), |row| {
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
            })?;
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
    // Vault / 加密区（U9）
    // ----------------------------------------------------------------

    /// 把 `id` 条目移入加密区：明文经 AES-GCM 加密（`encryptor`），密文 base64
    /// 写回 `content`、nonce 写 `nonce`、`is_vault=1`、`is_decryptable=1`，并把
    /// `content_hash` 替换为 salted 变体（不再是明文指纹，KTD4）。
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
        self.encrypt_into_vault(id, &entry.content, encryptor).await
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
            // 读取明文（pending 标记前，避免依赖 pending 后的行状态）。
            let entry = match self.get_by_id(id).await.map_err(VaultError::Sqlite)? {
                Some(e) if !e.is_vault => e,
                _ => continue, // 不存在或已在 vault：跳过。
            };
            self.set_vault_flag(id, 2)
                .await
                .map_err(VaultError::Sqlite)?;
            match self.encrypt_into_vault(id, &entry.content, encryptor).await {
                Ok(()) => report.succeeded.push(id),
                Err(e) => {
                    // 回退到明文态（is_vault=0），触发器会把内容重新放回 FTS。
                    self.set_vault_flag(id, 0)
                        .await
                        .map_err(VaultError::Sqlite)?;
                    report.failed.push(id);
                    // Keychain 不可用会让整批失败：记录首个原因后继续标记其余为 failed。
                    report.last_error.get_or_insert_with(|| e.to_string());
                }
            }
        }
        Ok(report)
    }

    /// 内部：读取的明文 → 加密 → 落盘为 vault 密文。不改动其它列。
    async fn encrypt_into_vault(
        &self,
        id: i64,
        plaintext: &str,
        encryptor: &dyn crate::crypto::Encryptor,
    ) -> Result<(), VaultError> {
        // 加密在 SQLite 锁外完成；Keychain 不可用时在此直接返回，DB 不变。
        let enc = encryptor.encrypt(plaintext).map_err(VaultError::Crypto)?;
        let new_hash = crate::crypto::salted_content_hash(id);
        let nonce = enc.nonce.to_vec();
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE entries
                 SET content = ?1, nonce = ?2, is_vault = 1,
                     is_decryptable = 1, content_hash = ?3
                 WHERE id = ?4",
                params![enc.content_b64, nonce, new_hash, id],
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
            .or(Ok(None))
        })
        .await
    }

    /// 列出所有加密区条目（`is_vault=1`），供 Settings「加密区管理」UI 使用。
    /// 密文不解密：返回的 `content` 是 base64 密文，UI 只按元数据展示。
    pub async fn list_vault_entries(&self) -> SqliteResult<Vec<Entry>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, content_hash, content, note, auto_category,
                        is_pinned, pinned_at, is_vault, is_decryptable, created_at
                 FROM entries
                 WHERE is_vault = 1
                 ORDER BY is_decryptable ASC, created_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
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
            })?;
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
        let rows: Vec<(i64, String, Option<Vec<u8>>)> = self
            .with_conn(|conn| {
                let mut stmt =
                    conn.prepare("SELECT id, content, nonce FROM entries WHERE is_vault = 1")?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                })?;
                rows.collect()
            })
            .await?;

        let mut report = VerifyReport::default();
        for (id, content, nonce) in rows {
            report.checked += 1;
            let ok = match nonce {
                Some(n) => decryptor.decrypt(&content, &n).is_ok(),
                None => false,
            };
            if !ok {
                report.undecryptable += 1;
            }
            self.with_conn(move |conn| {
                conn.execute(
                    "UPDATE entries SET is_decryptable = ?1 WHERE id = ?2",
                    params![ok as i32, id],
                )?;
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

        let results = db.search("webhook", "all", 10).await.unwrap();
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
        let results = db.search("us", "all", 10).await.unwrap();
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
        let results = db.search("trigram", "all", 10).await.unwrap();
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
    async fn schema_migration_zero_to_one() {
        use rusqlite::Connection;

        // Simulate a fresh DB with user_version=0.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA user_version = 0", []).unwrap();

        clipvault_core::schema::run_migrations(&conn).unwrap();

        let uv: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, 2);

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
