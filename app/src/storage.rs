//! SQLite storage layer for ClipVault.
//!
//! Uses `Arc<tokio::sync::Mutex<Connection>>` as the shared connection
//! (KTD5).  All blocking SQLite work runs inside `with_conn` which
//! acquires the mutex asynchronously so it does not block the tokio
//! executor thread.

use clipvault_core::Entry;
use clipvault_core::schema;
use rusqlite::{Connection, OptionalExtension, Result as SqliteResult, params};
use serde_json::to_string as json_to_string;
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

/// 列表查询专用列清单（性能优化 W1 / KTD2）。
///
/// 列表路径（`list_recent` / `get_by_category` / `search_entries`）从不消费
/// `payload` 与 `payload_nonce`——`to_dtos` 只把 11 个轻字段拼成 DTO,大
/// BLOB 在拿到 `Row` 那一刻被 `rusqlite` 拷成 `Vec<u8>` 之后立即丢弃。30
/// 条 × 平均 2 MB payload ≈ 60 MB 一次性 alloc,无任何功能收益。
///
/// 把这两列写为 `NULL AS payload, NULL AS payload_nonce`,让 `Entry::from_sqlite_row`
/// 索引 0..=14 仍对齐, 但 SQLite 不去 fetch BLOB 列——`Option<Vec<u8>>::get`
/// 解 `NULL` 为 `None`, 零分配; `to_dtos` 行为不变。
///
/// 顺序与列名必须与 `ENTRY_COLS` 完全一致(`Entry::from_sqlite_row` 索引硬绑)。
const ENTRY_LIST_COLS: &str = "id, content_hash, content, note, auto_category, \
     is_pinned, pinned_at, is_vault, is_decryptable, created_at, \
     content_kind, mime_type, NULL AS payload, NULL AS payload_nonce, metadata_json";

/// `is_vault` 三态标记的原始整数值：明文 / 已加密 / 加密中（pending）。
const VAULT_PLAINTEXT: i32 = 0;
const VAULT_ENCRYPTED: i32 = 1;
const VAULT_PENDING: i32 = 2;

/// `GROUP_CONCAT(t.tag, CHAR(31))` 的结果串 → `Vec<String>`（W4 / R16）。
///
/// `LEFT JOIN tags` 下**没有标签**的条目该列为 `NULL` → 空 vec。
/// `Some("")` 理论上不会出现（`GROUP_CONCAT` 对全 NULL 的组返回 NULL，对
/// 有值的组至少一个非空元素），仍按空处理以防万一。
fn split_tag_concat(raw: Option<String>) -> Vec<String> {
    match raw {
        Some(s) if !s.is_empty() => s.split('\x1f').map(String::from).collect(),
        _ => Vec::new(),
    }
}

/// Default database file location.
///
/// 按平台分派到各自的系统惯例位置：macOS 是 `~/Library/Application Support/ClipVault/`，
/// Windows 是 `%APPDATA%\ClipVault\`（即 `C:\Users\<user>\AppData\Roaming\ClipVault\`）。
/// 之所以两端各用各的惯例、而不是自己造一个统一的路径，是因为放在系统约定的位置，
/// 备份/迁移工具与用户才能按预期找到它。
///
/// 注意这只是**位置**不同：库文件格式、schema 与迁移完全相同，两端的库可以互相打开。
/// 唯一不通用的是加密区条目的密钥 —— 它存在各自的系统凭据库里，见 `crate::crypto`。
pub fn default_db_path() -> PathBuf {
    resolve_data_dir(
        cfg!(target_os = "windows"),
        std::env::var("HOME").ok(),
        std::env::var("APPDATA").ok(),
    )
    .join("ClipVault")
    .join("clipvault.db")
}

/// 数据根目录的平台分派。
///
/// 抽成纯函数是为了让 **Windows 分支也能在 macOS 上被单测覆盖**：路径写错是那种
/// "编译得过、跑起来才发现库建到了别处"的问题，而当前开发机是 macOS，
/// `cfg!` 的另一个分支在真机上永远不会被执行到。三个入参都是显式的，测试直接喂值。
fn resolve_data_dir(is_windows: bool, home: Option<String>, appdata: Option<String>) -> PathBuf {
    if is_windows {
        let appdata = appdata.expect("APPDATA environment variable must be set on Windows");
        PathBuf::from(appdata)
    } else {
        let home = home.expect("HOME environment variable must be set");
        PathBuf::from(home).join("Library/Application Support")
    }
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

        // 性能优化 W1 / KTD5:本地单文件 SQLite 的标准加速。
        // - WAL:读写不再互相阻塞;即便仍是单连接, `search_entries` 在
        //   `with_conn` 排队的窗口里 watcher 的写入不再让它堵。
        // - synchronous=NORMAL 配 WAL:SQLite 文档推荐做法;崩溃时丢最后
        //   一两个事务但已经持久化。本地单文件单机场景可接受。
        // - busy_timeout=5s:遇到「另一个连接正持有锁」时(罕见,只在 WAL
        //   多 connection 下出现) 等 5 秒。
        //
        // `journal_mode` 是「结果 pragma」,必须用 `pragma_update`(会读出旧
        // 值并写新值),不能塞 `execute_batch`(后者丢弃结果)。rusqlite 0.32
        // 的签名是 `(schema: Option<DatabaseName>, name: &str, value: V)`。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;

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
                    content_kind, // auto_category mirrors content_kind for binary
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

    /// 把 `content_hash` 已存在的条目 "上浮到顶":仅刷 `created_at = now`,
    /// 其余字段(`is_pinned` / `pinned_at` / `note` / `tags` / `payload` /
    /// `metadata_json` / `auto_category`)一律不动 —— 复制的"再次见到"语义只
    /// 影响排序,不该改写用户手动编辑的备注或置顶状态。
    ///
    /// 调用形态:watcher 命中 `content_hash UNIQUE` 冲突后,把已有行 `created_at`
    /// 推到此刻,让 `PIN_ORDER` (clipvault-search.rs `is_pinned DESC, pinned_at
    /// DESC, created_at DESC`) 把这条重新排到列表顶部 —— 修复用户症状
    /// 「相同内容再次粘贴还在原位置,找不到」。
    ///
    /// `now` 用 Unix 秒（与 `entries.created_at` 列单位一致,见 `toggle_pin`
    /// `SystemTime` 取秒的写法）。
    /// 返回被刷的行数:0 = 没有此 fingerprint（不该发生,watcher 刚撞过
    /// UNIQUE）,1 = 正常单条命中。
    pub async fn bump_to_top_by_content_hash(
        &self,
        content_hash: &str,
        now: i64,
    ) -> SqliteResult<usize> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE entries SET created_at = ?1 WHERE content_hash = ?2",
                params![now, content_hash],
            )
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
                "SELECT {ENTRY_LIST_COLS} FROM entries e ORDER BY {order} LIMIT ?1",
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
    /// # Performance (W1 / KTD3)
    /// 旧版 N 个标签 = N 次单独 `INSERT INTO tags`,每次都触发一次
    /// `tags_fts_insert` 重算 `tags_text`。新版用 `json_each` 把 N 行
    /// 合到一条 SQL:`INSERT INTO tags (entry_id, tag) SELECT ?1, value
    /// FROM json_each(?2)` —— `json1` 由 bundled SQLite 自带,零新依赖。
    /// tags 触发器仍每次触发(FTS5 触发器的固有限制),但 SQLite 层写次数
    /// 从 N 降到 1。
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
        // `json_each(?)` 接收合法 JSON 文本:把 `Vec<String>` 序列化成
        // `["a","b"]`,每行产出 `value` 一列。空数组会让 `json_each`
        // 输出 0 行,等价于"无标签可写"。
        let tags_json = json_to_string(tags)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE entries SET note = ?1 WHERE id = ?2",
                params![note, id],
            )?;
            tx.execute("DELETE FROM tags WHERE entry_id = ?1", params![id])?;
            // 一次 INSERT 写完所有标签(可能 0 行)。`entry_id` 与 `tag`
            // 都被 `idx_tags_entry_id`(V4)/ `idx_tags_tag`(V5) 覆盖;
            // 唯一性由 `tags.id` 主键守护——同一 entry_id + tag 不会撞
            // 冲突,DELETE FROM tags 已经把旧的全清。
            tx.execute(
                "INSERT INTO tags (entry_id, tag) \
                 SELECT ?1, value FROM json_each(?2)",
                params![id, tags_json],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// 删除单条记录，连同它的标签行。返回被删除的条目行数（`0` 表示 id 不存在）。
    ///
    /// `tags` 必须显式删：连接没有开 `PRAGMA foreign_keys`，schema 里的
    /// `ON DELETE CASCADE` 不会生效。FTS 索引由 `entries_fts_delete` 触发器同步移除。
    pub async fn delete_entry(&self, id: i64) -> SqliteResult<usize> {
        self.with_conn(move |conn| {
            let tx = conn.transaction()?;
            // 先删 entries 再删 tags：两个触发器都是逐行触发的，反过来写的话
            // `tags_fts_delete` 会对每个标签行各重写一次 `entries_fts.tags_text`，
            // 而那一行 FTS 记录紧接着就被 `entries_fts_delete` 整个删掉 —— 纯浪费
            // （删大文本 + 多标签时可按 10ms 量级实测到）。这个顺序下 tags 触发器的
            // `WHEN (SELECT is_vault FROM entries …)` 取不到行、求值为 NULL，直接跳过。
            let removed = tx.execute("DELETE FROM entries WHERE id = ?1", params![id])?;
            tx.execute("DELETE FROM tags WHERE entry_id = ?1", params![id])?;
            tx.commit()?;
            Ok(removed)
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
                "SELECT {ENTRY_LIST_COLS} FROM entries e
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

    /// 执行带标签的 [`crate::search::SearchPlan`]（性能优化 W4 / R16）。
    ///
    /// 与 [`Storage::search_entries`] 同形，只是 SQL 里多了
    /// `GROUP_CONCAT(t.tag, CHAR(31))` 一列（见
    /// [`crate::search::build_search_query_with_tags`]）；标签串按 `\x1f`
    /// 拆开后与 `Entry` 一起返回。命令层因此不必为同一批 id 二次
    /// `with_conn` 拉标签（旧 `load_tags` 的 N+1 形状）。
    pub async fn search_entries_with_tags(
        &self,
        plan: crate::search::SearchPlan,
    ) -> SqliteResult<Vec<(Entry, Vec<String>)>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&plan.sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(plan.params.iter()), |row| {
                let entry = Entry::from_sqlite_row(row)?;
                Ok((entry, split_tag_concat(row.get::<_, Option<String>>(15)?)))
            })?;
            rows.collect()
        })
        .await
    }

    /// 列表侧带标签变体（性能优化 W4 / R16）：`list_recent` 的 LEFT JOIN 版本。
    pub async fn list_recent_with_tags(
        &self,
        limit: u32,
    ) -> SqliteResult<Vec<(Entry, Vec<String>)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {cols} FROM entries e \
                 LEFT JOIN tags t ON t.entry_id = e.id \
                 GROUP BY e.id ORDER BY {order} LIMIT ?1",
                cols = crate::search::SELECT_COLS_WITH_TAGS,
                order = crate::search::PIN_ORDER,
            ))?;
            let rows = stmt.query_map(params![limit], |row| {
                let entry = Entry::from_sqlite_row(row)?;
                Ok((entry, split_tag_concat(row.get::<_, Option<String>>(15)?)))
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

    /// 一次读取多个 `settings` 键(性能优化 W3 / R11)。
    ///
    /// 启动路径(`main::setup`)原本对 4 个键各做一次 `SELECT value FROM settings
    /// WHERE key = ?`,4 次独立 `block_on` 串行抢同一个 `conn` mutex。这里合并成
    /// 一条 `WHERE key IN (?, ?, …)`,一次往返一次锁。返回值只含**实际存在**的键;
    /// 调用方按 `Option<&str>` 处理缺失(与单键 `get_setting` 的 `None` 语义一致)。
    pub async fn get_settings_batch(
        &self,
        keys: &[&str],
    ) -> SqliteResult<std::collections::HashMap<String, String>> {
        if keys.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders = vec!["?"; keys.len()].join(", ");
        let sql = format!("SELECT key, value FROM settings WHERE key IN ({placeholders})");
        let owned: Vec<String> = keys.iter().map(|s| (*s).to_string()).collect();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(owned.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.collect()
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
                let enc = encryptor.encrypt_bytes(bytes).map_err(VaultError::Crypto)?;
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
            let tx = conn.transaction()?;
            // 先删 tags 再删 entries，两条 DELETE 用同一判据。连接没开
            // `PRAGMA foreign_keys`，`ON DELETE CASCADE` 不生效，所以反过来写的话
            // 这些 entry_id 随 entries 一起消失，`tags` 里就留下永远指不回条目的
            // 幽灵标签行（标签云与 tag_counts 会把它们算进去）。
            // 先删 tags 不会白重写 `entries_fts`：这批判据只命中加密区条目，而
            // `tags_fts_delete` 的 WHEN 对 `is_vault = 1` 恒为假，直接短路。
            tx.execute(
                "DELETE FROM tags WHERE entry_id IN \
                 (SELECT id FROM entries WHERE is_vault = 1 AND is_decryptable = 0)",
                [],
            )?;
            let n = tx.execute(
                "DELETE FROM entries WHERE is_vault = 1 AND is_decryptable = 0",
                [],
            )?;
            tx.commit()?;
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
    use super::{PathBuf, resolve_data_dir, test_helpers::open_test_db};

    #[test]
    fn data_dir_uses_appdata_on_windows() {
        // Windows 分支在当前开发机（macOS）上永远不会被执行到，只能这样覆盖。
        let dir = resolve_data_dir(
            true,
            Some("/Users/me".into()),
            Some(r"C:\Users\me\AppData\Roaming".into()),
        );
        assert_eq!(dir, PathBuf::from(r"C:\Users\me\AppData\Roaming"));
    }

    #[test]
    fn data_dir_uses_application_support_on_macos() {
        let dir = resolve_data_dir(false, Some("/Users/me".into()), Some("ignored".into()));
        assert_eq!(dir, PathBuf::from("/Users/me/Library/Application Support"));
    }

    #[test]
    #[should_panic(expected = "APPDATA")]
    fn data_dir_without_appdata_panics_on_windows() {
        // 缺环境变量时宁可当场炸掉也不要静默落到当前工作目录（那会把库建到随机位置）。
        resolve_data_dir(true, Some("/Users/me".into()), None);
    }

    #[test]
    #[should_panic(expected = "HOME")]
    fn data_dir_without_home_panics_on_macos() {
        resolve_data_dir(false, None, None);
    }

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

    /// `bump_to_top_by_content_hash` 的合约:仅刷目标行 `created_at = now`,
    /// 不动 `is_pinned` / `pinned_at` / `note` / `payload` 等其它字段;
    /// 不命中时返回 0。
    #[tokio::test]
    async fn bump_to_top_by_content_hash_only_updates_created_at() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let db = open_test_db();
        let id_target = db
            .insert_entry("target_hash", "tgt content", "plain")
            .await
            .unwrap();
        let id_other = db
            .insert_entry("other_hash", "other content", "plain")
            .await
            .unwrap();

        // 给目标行加 note + pin,验证 bump 不动它们。
        db.with_conn(move |conn| {
            conn.execute(
                "UPDATE entries SET note = ?1, is_pinned = 1, pinned_at = 12345 WHERE id = ?2",
                rusqlite::params!["manual note", id_target],
            )
        })
        .await
        .unwrap();

        let before_target = db.get_by_id(id_target).await.unwrap().unwrap();
        let before_other = db.get_by_id(id_other).await.unwrap().unwrap();
        assert_eq!(before_target.note, "manual note".to_string());
        assert!(before_target.is_pinned);

        // 找一个 fresh 的 `now`(比行默认的 strftime('%s','now') 更晚,避免
        // 撞同秒边界)。
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 1000;
        let affected = db
            .bump_to_top_by_content_hash("target_hash", now)
            .await
            .unwrap();
        assert_eq!(affected, 1, "命中一个 hash 应刷 1 行");

        let after_target = db.get_by_id(id_target).await.unwrap().unwrap();
        assert_eq!(after_target.created_at, now, "created_at 必须刷到 now");
        assert_eq!(
            after_target.note,
            "manual note".to_string(),
            "note 必须不被 bump 改写"
        );
        assert!(after_target.is_pinned, "is_pinned 必须不动");
        assert_eq!(
            after_target.pinned_at, before_target.pinned_at,
            "pinned_at 必须不动 —— 这是 pin 时刻,不应被复制语义污染"
        );
        assert_eq!(after_target.content, before_target.content);

        // 另一行完全不动。
        let after_other = db.get_by_id(id_other).await.unwrap().unwrap();
        assert_eq!(after_other.created_at, before_other.created_at);
        assert_eq!(after_other.content, "other content");

        // 不命中:返回 0。
        let affected_zero = db
            .bump_to_top_by_content_hash("nonexistent", now)
            .await
            .unwrap();
        assert_eq!(affected_zero, 0);
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

    /// 性能优化 W1 / R5:`update_note_and_tags` 多行 INSERT 走 `json_each`,
    /// 10 个 tag 应在**单条 SQL**里写完(旧版是 10 条独立 INSERT)。
    ///
    /// 这里**不**走 `Storage::update_note_and_tags`:Storage 在
    /// `Arc<Mutex<Connection>>` 之上包装了一层 `tokio::sync::Mutex`,
    /// 在外层挂 hook 后再让它进 `with_conn` 会嵌套死锁。
    /// 直接拿裸 `Connection` 跑等价 SQL,验 SQL 层面的行为(W1 改的就是 SQL)。
    ///
    /// 关于**行级 vs. 语句级**计数:rusqlite 0.32 不暴露 `sqlite3_trace_v2`,
    /// `update_hook` 是行级触发(`Insert` 多行被触发 N 次),两者都无法稳定
    /// 区分新旧实现。这里采用**两条互补的契约**:
    ///
    /// 1. **等价 SQL 的 statement-level 计数**:`tx.execute(sql, params)` 返
    ///    回值 = `changes()` = 受影响行数。多行 INSERT 一次 `execute()` 应
    ///    返回 10。10 条独立 INSERT 会每次返回 1,合起来才 10——但 execute()
    ///    调用次数不同。这里直接断言 `execute()` 返回 10,**且**只调用 1 次。
    /// 2. **Storage 源码契约**:`Storage::update_note_and_tags` 内部必须用
    ///    `json_each`,否则仍然是 N 个独立 `tx.execute("INSERT INTO tags")`。
    ///    静态断言源码里有且仅有 1 处 `INSERT INTO tags` 字面量 —— 这是最直接
    ///    的「这条改动真的落地了」的合约。
    #[tokio::test]
    async fn update_note_and_tags_with_10_tags_uses_json_each() {
        use rusqlite::{Connection, params};

        let mut conn = Connection::open_in_memory().unwrap();
        clipvault_core::schema::run_migrations(&conn).unwrap();

        conn.execute(
            "INSERT INTO entries (content_hash, content) VALUES ('w1tags', 'hello')",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();

        let tags: Vec<String> = (0..10).map(|i| format!("tag{i}")).collect();
        let tags_json = serde_json::to_string(&tags).unwrap();

        // 等价 SQL:一条 INSERT 写完所有标签。`tx.execute` 的返回值是受影响行数。
        let tx = conn.transaction().unwrap();
        tx.execute("DELETE FROM tags WHERE entry_id = ?1", params![id])
            .unwrap();
        let changes = tx
            .execute(
                "INSERT INTO tags (entry_id, tag) SELECT ?1, value FROM json_each(?2)",
                params![id, tags_json],
            )
            .unwrap();
        tx.commit().unwrap();

        assert_eq!(
            changes, 10,
            "单条多行 INSERT 必须一次性报 10 行受影响(W1 / R5 合约)"
        );
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE entry_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 10, "10 个标签仍都进了库(语义不变)");
    }

    /// 性能优化 W1 / R5 的**静态合约**:`Storage::update_note_and_tags` 的实现
    /// 字面包含 `json_each`,且只有 1 处 `INSERT INTO tags` 字面量。这条对应
    /// 「改动落地」——读源码能直接看到 `for tag in tags { tx.execute(...) }`
    /// 的旧路径已消失。
    #[test]
    fn update_note_and_tags_source_uses_json_each() {
        let src = include_str!("storage.rs");
        // 抓 `pub async fn update_note_and_tags` 函数体:平衡花括号。
        let start = src
            .find("pub async fn update_note_and_tags")
            .expect("找不到 update_note_and_tags 函数");
        let after_sig = &src[start..];
        // 找第一个 `{` 之后的整段函数体。
        let body_start = after_sig
            .find('{')
            .map(|idx| idx + start + 1)
            .expect("找不到 {");
        // 按 `{` 与 `}` 字节数配对(本函数内部没有字符串字面量含 `}` )。
        let mut depth: i32 = 1;
        let mut i = body_start;
        let bytes = src.as_bytes();
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let body = &src[body_start..i - 1];
        assert!(
            body.contains("json_each"),
            "update_note_and_tags 必须用 `json_each` 多行 INSERT(W1 / R5)"
        );
        let literal_inserts = body.matches("INSERT INTO tags").count();
        assert_eq!(
            literal_inserts, 1,
            "update_note_and_tags 函数体内必须只有 1 处 `INSERT INTO tags` 字面量, \
             多于 1 即回到旧路径(N 次单 INSERT);实际看到 {literal_inserts}"
        );
    }

    /// 性能优化 W1 / R6:`list_recent` 走 `ENTRY_LIST_COLS`,`payload` /
    /// `payload_nonce` 列用 `NULL AS` 占位——rusqlite 读 `NULL` BLOB
    /// 为 `None`,不 fetch 真实 BLOB。DTrace/Perf 上的直接观感是
    /// 30 条 × 平均几 MB payload 从 ~60 MB 一次性 alloc 降到 0。
    ///
    /// 这条用例钉住列表侧的契约:`list_recent` 返回的 `Entry::payload`
    /// 始终是 `None`,即便库里那一行有真实 BLOB。
    #[tokio::test]
    async fn list_recent_does_not_fetch_payload_blob() {
        let db = open_test_db();
        // 64 KiB 的 BLOB,够大,即便没有 30 条 × 几 MB 那么极端也能看出来
        // 「读出来不为空」的对照。
        let big_payload = vec![0xABu8; 64 * 1024];
        let id = db
            .insert_entry_binary(
                "listreclo",
                "image",
                Some("image/png"),
                &big_payload,
                None,
                Some(r#"{"width":1,"height":1}"#),
            )
            .await
            .unwrap();

        // 直接 read 一次验证真实 BLOB 写入了。
        let raw: Vec<u8> = db
            .with_conn(move |conn| {
                conn.query_row("SELECT payload FROM entries WHERE id = ?1", [id], |row| {
                    row.get(0)
                })
            })
            .await
            .unwrap();
        assert_eq!(raw.len(), big_payload.len(), "payload 列实际写入了 BLOB");

        // list_recent 走 ENTRY_LIST_COLS,payload 列在 SELECT 层就是
        // `NULL AS payload`——返回的 Entry.payload 必须恒为 None。
        let entries = db.list_recent(10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].payload.is_none(),
            "list_recent 走 ENTRY_LIST_COLS, payload 必须为 None（不 fetch BLOB）"
        );
        assert!(
            entries[0].payload_nonce.is_none(),
            "list_recent payload_nonce 必须为 None"
        );
        // 其他字段必须照常读出来——NULL AS 占位只针对 payload/payload_nonce。
        assert_eq!(entries[0].content_kind, "image");
        assert_eq!(entries[0].auto_category, "image");
    }

    /// 性能优化 W4 / R16:`list_recent_with_tags` 用一次 `LEFT JOIN + GROUP_CONCAT`
    /// 把标签一起带回,有标签的条目给标签、无标签的给空 vec。
    #[tokio::test]
    async fn list_recent_with_tags_returns_tags_alongside_entries() {
        let db = open_test_db();
        let tagged = db.insert_entry("rt1", "first", "plain").await.unwrap();
        db.update_note_and_tags(tagged, "", &["alpha".to_string(), "beta".to_string()])
            .await
            .unwrap();
        let untagged = db.insert_entry("rt2", "second", "plain").await.unwrap();

        let rows = db.list_recent_with_tags(10).await.unwrap();
        assert_eq!(rows.len(), 2, "两条都应返回");

        let (_, tags_of_tagged) = rows
            .iter()
            .find(|(e, _)| e.id == tagged)
            .expect("有标签的条目必须在结果里");
        assert_eq!(
            tags_of_tagged,
            &vec!["alpha".to_string(), "beta".to_string()],
            "标签必须按 entry_id, tag 顺序一次带回"
        );

        let (_, tags_of_untagged) = rows
            .iter()
            .find(|(e, _)| e.id == untagged)
            .expect("无标签的条目也必须在结果里");
        assert!(
            tags_of_untagged.is_empty(),
            "LEFT JOIN 下无标签条目的 GROUP_CONCAT 为 NULL,必须解为空 vec"
        );
    }

    /// 性能优化 W4 / R16:分隔符必须用 `CHAR(31)`(ASCII Unit Separator)而不是
    /// 逗号 —— 标签本身可能含逗号,用逗号会把「一个 `a,b` 标签」错拆成
    /// 「`a` 与 `b` 两个标签」。
    #[tokio::test]
    async fn tags_containing_commas_are_not_split_by_the_concat_delimiter() {
        let db = open_test_db();
        let id = db.insert_entry("comma", "x", "plain").await.unwrap();
        db.update_note_and_tags(id, "", &["a,b".to_string(), "c,d".to_string()])
            .await
            .unwrap();

        let rows = db.list_recent_with_tags(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1,
            vec!["a,b".to_string(), "c,d".to_string()],
            "含逗号的标签必须原样保留,不被分隔符拆开"
        );
    }

    /// 性能优化 W4 / R16:`search_entries_with_tags` 端到端 —— 一次查询把命中
    /// 条目的标签也带回来。
    #[tokio::test]
    async fn search_entries_with_tags_returns_tags_alongside_hits() {
        let db = open_test_db();
        let hit = db
            .insert_entry("st1", "git webhook endpoint", "code")
            .await
            .unwrap();
        db.update_note_and_tags(hit, "", &["workflow".to_string()])
            .await
            .unwrap();
        // 旁观条目:证明返回的标签属于命中条目,而不是「库里任何标签」。
        let other = db.insert_entry("st2", "unrelated", "plain").await.unwrap();
        db.update_note_and_tags(other, "", &["other".to_string()])
            .await
            .unwrap();

        let plan = crate::search::build_search_query_with_tags(
            "webhook",
            &crate::search::parse_tab("all"),
            10,
        );
        let rows = db.search_entries_with_tags(plan).await.unwrap();
        assert_eq!(rows.len(), 1, "只有一条命中 'webhook'");
        assert_eq!(rows[0].0.id, hit);
        assert_eq!(
            rows[0].1,
            vec!["workflow".to_string()],
            "命中条目必须带自己的标签,且不混入旁观条目的标签"
        );
    }

    /// 性能优化 W1 / R4:`entries_fts_update` 触发器加了
    /// `WHEN OLD.is_vault = 0 OR NEW.is_vault = 0`,纯 vault UPDATE
    /// 不再触发空 DELETE + 空 INSERT。
    ///
    /// 这条用例直接从 `sqlite_master` 读触发器 SQL 文本,把契约钉死
    /// ——「V5 之后这个 trigger 必须有 WHEN」是文档事实,文本是源头。
    /// vault 不在 `entries_fts` 里(INSERT 触发器 WHEN 已短路),所以
    /// 端到端的「FTS doc count 不变」无法区分 trigger 是否真生效;
    /// 但「trigger 字符串里没有 WHEN」就能稳定地被打回去。
    #[tokio::test]
    async fn entries_fts_update_trigger_has_when_clause_to_skip_pure_vault_updates() {
        use rusqlite::Connection;

        let conn = Connection::open_in_memory().unwrap();
        clipvault_core::schema::run_migrations(&conn).unwrap();

        let trigger_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master \
                 WHERE type='trigger' AND name='entries_fts_update'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            trigger_sql.contains("WHEN"),
            "entries_fts_update 必须有 WHEN, 没有的话纯 vault UPDATE \
             (pin toggle / verify_decryptable / set_vault_flag / encrypt_into_vault) \
             都会空 DELETE + 空 INSERT —— 是 W1 / R4 要修掉的开销"
        );
    }

    /// W3 / R11:`get_settings_batch` 一次读多个键,只返回实际存在的键。
    ///
    /// `main::setup` 原路径对 4 个键各做一次 `SELECT value FROM settings WHERE
    /// key = ?`,4 次独立 `block_on` 串行抢同一把 `conn` mutex。改由这里一次
    /// 往返;因此「缺键不在 map」与「多键一次读」两条语义都必须在测试里钉死:
    /// 前者决定调用方要用 `Option<&str>` 处理缺键,后者是 R11 本身的收益。
    #[tokio::test]
    async fn get_settings_batch_reads_several_keys_and_omits_missing() {
        let db = open_test_db();
        db.set_setting("guest_mode", "1").await.unwrap();
        db.set_setting("panel_opacity", "78").await.unwrap();
        db.set_setting("hotkeys", r#"{"all":[]}"#).await.unwrap();
        // `log_level` 故意不设 —— 启动路径上「用户没设过」是常见状态。

        let batch = db
            .get_settings_batch(&["guest_mode", "panel_opacity", "hotkeys", "log_level"])
            .await
            .unwrap();

        assert_eq!(
            batch.len(),
            3,
            "读了 3 个存在的键;缺失的 log_level 不在 map"
        );
        assert_eq!(batch.get("guest_mode").map(String::as_str), Some("1"));
        assert_eq!(batch.get("panel_opacity").map(String::as_str), Some("78"));
        assert_eq!(
            batch.get("hotkeys").map(String::as_str),
            Some(r#"{"all":[]}"#)
        );
        assert!(
            !batch.contains_key("log_level"),
            "缺失键必须不在 map 里(调用方用 Option<&str> 处理)"
        );
    }

    /// `get_settings_batch` 空输入不进 SQL(`WHERE key IN ()` 是非法语法)。
    #[tokio::test]
    async fn get_settings_batch_with_empty_keys_returns_empty_map() {
        let db = open_test_db();
        let batch = db.get_settings_batch(&[]).await.unwrap();
        assert!(batch.is_empty(), "空 keys 应直接返回空 map, 不构造非法 SQL");
    }

    /// 删除单条：条目行消失、它的标签行同时清掉（连接没开 `PRAGMA foreign_keys`，
    /// `ON DELETE CASCADE` 不生效，必须显式删）、FTS 索引由触发器同步移除。
    #[tokio::test]
    async fn delete_entry_removes_row_tags_and_search_index() {
        let db = open_test_db();
        let id = db
            .insert_entry("del1", "git webhook endpoint", "code")
            .await
            .unwrap();
        db.update_note_and_tags(id, "note", &["doomed".to_string()])
            .await
            .unwrap();
        // 旁观条目：它证明下面「标签清空」的断言是因为删了这一条，而不是库本来就空。
        let keep = db.insert_entry("del2", "keep me", "plain").await.unwrap();
        db.update_note_and_tags(keep, "", &["survivor".to_string()])
            .await
            .unwrap();

        async fn hits(db: &super::Storage, q: &str) -> Vec<clipvault_core::Entry> {
            db.search_entries(crate::search::build_search_query(
                q,
                &crate::search::parse_tab("all"),
                10,
            ))
            .await
            .unwrap()
        }
        // 直接探 `entries_fts` 本身。只断言「搜不到」是不够的：`search_entries` 要 join
        // `entries`，所以条目行删掉后即使 FTS 里留着索引行也照样搜不到 —— 触发器坏掉
        // 时那条断言仍是绿的。
        async fn fts_rows(db: &super::Storage, id: i64) -> i64 {
            db.with_conn(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM entries_fts WHERE rowid = ?1",
                    [id],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap()
        }
        assert!(!hits(&db, "webhook").await.is_empty(), "删前必须能搜到");
        assert_eq!(fts_rows(&db, id).await, 1, "删前 FTS 里应有它的索引行");

        assert_eq!(db.delete_entry(id).await.unwrap(), 1, "应删除 1 行");

        assert!(db.get_by_id(id).await.unwrap().is_none(), "条目应已删除");
        assert_eq!(
            db.get_all_tags().await.unwrap(),
            vec!["survivor".to_string()],
            "被删条目的标签必须清掉，旁观条目的标签必须留下"
        );

        assert!(
            hits(&db, "webhook").await.is_empty(),
            "删除后全文搜索不应再命中"
        );
        assert_eq!(fts_rows(&db, id).await, 0, "FTS 索引行必须由触发器同步移除");
        assert_eq!(fts_rows(&db, keep).await, 1, "旁观条目的索引行必须留下");
        assert!(!hits(&db, "keep").await.is_empty(), "旁观条目仍应搜得到");

        // 幂等：再删同一个 id 返回 0，不报错。
        assert_eq!(db.delete_entry(id).await.unwrap(), 0, "重复删除应返回 0");
    }

    /// 删除**加密区条目**（`is_vault = 1`）走的是同一条 `delete_entry`，但它有两处
    /// 与普通条目不同，这里各钉一条：
    /// - 它本来就没有 `entries_fts` 索引行（INSERT 触发器 `WHEN is_vault = 0`）；
    /// - `tags_fts_delete` 的 WHEN 对它恒为假（`is_vault = 1`），删它的标签必须**短路**，
    ///   不能顺手去改别的 FTS 行 —— 若它被当成普通条目处理，下面的 `plaintag` 断言会红。
    #[tokio::test]
    async fn delete_entry_handles_vault_row_without_touching_others_fts() {
        let db = open_test_db();
        // 直接落一行 `is_vault = 1` 的密文（不经过 Keychain / 真加密）。
        let vault_id: i64 = db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO entries (content_hash, content, is_vault, is_decryptable)
                     VALUES ('vaultdel', 'c2VjcmV0', 1, 1)",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .unwrap();
        db.update_note_and_tags(vault_id, "vault note", &["vaulttag".to_string()])
            .await
            .unwrap();
        // 旁观普通条目：证明删加密区条目不会动到别人的 FTS 行。
        let plain = db
            .insert_entry("plaindel", "ordinary text", "plain")
            .await
            .unwrap();
        db.update_note_and_tags(plain, "", &["plaintag".to_string()])
            .await
            .unwrap();

        async fn fts_tags_text(db: &super::Storage, id: i64) -> String {
            db.with_conn(move |conn| {
                conn.query_row(
                    "SELECT tags_text FROM entries_fts WHERE rowid = ?1",
                    [id],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap()
        }
        assert_eq!(fts_tags_text(&db, plain).await, "plaintag");

        assert_eq!(db.delete_entry(vault_id).await.unwrap(), 1, "应删除 1 行");

        assert!(
            db.get_by_id(vault_id).await.unwrap().is_none(),
            "加密区条目应已删除"
        );
        assert_eq!(
            db.get_all_tags().await.unwrap(),
            vec!["plaintag".to_string()],
            "加密区条目的标签必须清掉，普通条目的标签必须留下"
        );
        let vault_fts: i64 = db
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM entries_fts WHERE rowid = ?1",
                    [vault_id],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(vault_fts, 0, "加密区条目不该出现在 entries_fts 里");
        assert_eq!(
            fts_tags_text(&db, plain).await,
            "plaintag",
            "旁观条目的 FTS 行不得被改动"
        );
    }

    /// `clear_undecryptable` 除了删条目，还必须删掉它们的标签行：`tags` 没有外键级联
    /// （连接不开 `PRAGMA foreign_keys`），留下就是永远指不回条目的幽灵标签 —— 标签云与
    /// `tag_counts` 会把它们算进去。
    ///
    /// 加密 / 解密标记那条端到端路径由 `crypto_test` 的
    /// `undecryptable_marked_and_cleared` 覆盖，这里只钉「标签一起删」这一条不变量。
    #[tokio::test]
    async fn clear_undecryptable_also_removes_tags() {
        let db = open_test_db();
        let doomed = db
            .insert_entry("ghost1", "unreadable", "plain")
            .await
            .unwrap();
        db.update_note_and_tags(doomed, "", &["ghosttag".to_string()])
            .await
            .unwrap();
        db.set_vault_flag(doomed, 1).await.unwrap();
        // 判据是 `is_vault = 1 AND is_decryptable = 0`。把可解密位翻成 0 等价于
        // 「换机器后密钥对不上」的启动校验结果，不必引入 Keychain。
        db.with_conn(move |conn| {
            conn.execute(
                "UPDATE entries SET is_decryptable = 0 WHERE id = ?1",
                [doomed],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let survivor = db
            .insert_entry("ghost2", "readable", "plain")
            .await
            .unwrap();
        db.update_note_and_tags(survivor, "", &["survivortag".to_string()])
            .await
            .unwrap();

        assert_eq!(db.clear_undecryptable().await.unwrap(), 1);

        assert!(db.get_by_id(doomed).await.unwrap().is_none());
        assert_eq!(
            db.get_all_tags().await.unwrap(),
            vec!["survivortag".to_string()],
            "不可解密条目的标签必须一起清掉（否则是幽灵标签），可见条目的标签必须留下"
        );
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
    async fn schema_migration_zero_to_latest() {
        use rusqlite::Connection;

        // Simulate a fresh DB with user_version=0.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA user_version = 0", []).unwrap();

        clipvault_core::schema::run_migrations(&conn).unwrap();

        let uv: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, 6, "schema 已到 V6（#38 trigram），旧断言与之一致更新");

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

        // 真正的 round trip：用 `payload_nonce` 解 `payload` 必须还原原始字节。
        // 回贴走的正是这条路（`paste_entry` → `perform_paste` → `decrypt_payload`）。
        use crate::crypto::Decryptor as _;
        let nonce = entry.payload_nonce.clone().unwrap();
        let plain = cipher
            .decrypt_bytes(stored_payload, &nonce)
            .expect("vault 图片 payload 应能用 payload_nonce 解密");
        assert_eq!(plain, b"\x89PNG ORIGINAL BYTES");
    }

    #[tokio::test]
    async fn vault_binary_nonce_columns_are_independent() {
        // 多类型 plan U4/U6 回归：vault 二进制条目的 payload 由 `payload_nonce`
        // 列保护，与保护文本 content 的 `nonce` 列是两个独立的 12 字节值
        // （见 `Storage::encrypt_into_vault`）。回贴时 `paste_entry` 必须取
        // `Entry::payload_nonce`；若误用 `get_vault_nonce()`（读 `nonce` 列），
        // GCM 认证必然失败 → 每次都报「加密条目解密失败」。
        use crate::crypto::{Decryptor as _, VaultCipher};
        use clipvault_core::crypto::KeyProvider;

        struct FixedKey([u8; 32]);
        impl KeyProvider for FixedKey {
            fn data_key(&self) -> Result<[u8; 32], clipvault_core::crypto::CryptoError> {
                Ok(self.0)
            }
        }

        let db = open_test_db();
        let cipher = VaultCipher::new(FixedKey([9u8; 32]));
        let id = db
            .insert_entry_binary(
                "vimg-nonce",
                "image",
                Some("image/png"),
                b"\x89PNG NONCE COLUMN TEST",
                None,
                None,
            )
            .await
            .unwrap();
        db.move_to_vault(id, &cipher).await.unwrap();

        let entry = db.get_by_id(id).await.unwrap().unwrap();
        let payload = entry.payload.clone().unwrap();
        let payload_nonce = entry.payload_nonce.clone().expect("payload_nonce 必须写入");
        let content_nonce = db
            .get_vault_nonce(id)
            .await
            .unwrap()
            .expect("nonce 列（文本 content 的 nonce）必须写入");

        assert_ne!(
            payload_nonce, content_nonce,
            "payload_nonce 与 nonce 必须是两个独立随机值"
        );
        assert!(
            cipher.decrypt_bytes(&payload, &content_nonce).is_err(),
            "用 nonce 列的值解 payload 必须失败：这正是误用 get_vault_nonce 的症状"
        );
        assert_eq!(
            cipher.decrypt_bytes(&payload, &payload_nonce).unwrap(),
            b"\x89PNG NONCE COLUMN TEST"
        );
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
