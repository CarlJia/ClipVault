//! Schema definitions and migration runner for ClipVault SQLite database.
//!
//! Schema version managed via `PRAGMA user_version`.
//! - V1 (U2): entries / tags / settings + FTS5 `entries_fts` with sync triggers.
//! - V2 (U9): adds `entries.nonce BLOB` for AES-GCM vault ciphertext nonces.
//! - V3 (multi-type plan): adds `content_kind` / `mime_type` / `payload` /
//!   `payload_nonce` / `metadata_json` for non-text clipboard entries
//!   (image/file/rtf/color).
//! - V4 (删除单条记录): indexes `tags.entry_id`. Every delete path looks tags up
//!   by `entry_id` (`WHERE entry_id = ?` / `IN (SELECT id …)`) and the
//!   `tags_fts_*` triggers read tags the same way, so without this index each
//!   deleted entry full-scans `tags`.
//! - V5 (性能优化 W1): adds 4 indexes covering hot paths
//!   (`Storage::list_recent` / `tag_counts` / `category_counts` /
//!   `verify_decryptable_on_startup` / `clear_undecryptable`) and gates
//!   `entries_fts_update` with a `WHEN OLD.is_vault = 0 OR NEW.is_vault = 0`
//!   so non-vault↔vault transitions no longer empty-delete / re-insert the
//!   FTS row when nothing has actually changed for FTS.
//! - V6 (#38 子串搜索): rebuilds `entries_fts` with the trigram tokenizer and
//!   reindexes existing rows. unicode61 only matched whole tokens/prefixes, so
//!   a substring inside a token (`Fx3` in `asdf34Fx3`) was unfindable — R5
//!   requires substring matching.

use rusqlite::{Connection, Result};

/// DDL for schema version 1.
/// Tables: entries, tags, settings.
/// FTS5 virtual table: entries_fts with sync triggers.
pub const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    content_hash    TEXT    NOT NULL UNIQUE,
    content         TEXT    NOT NULL,
    note            TEXT    NOT NULL DEFAULT '',
    auto_category   TEXT    NOT NULL DEFAULT 'plain',
    is_pinned       INTEGER NOT NULL DEFAULT 0,
    pinned_at       INTEGER,
    is_vault        INTEGER NOT NULL DEFAULT 0,
    is_decryptable  INTEGER NOT NULL DEFAULT 1,
    created_at      INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
);

CREATE TABLE IF NOT EXISTS tags (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id INTEGER NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    tag      TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- FTS5 virtual table for full-text search on non-vault entries.
-- #38：`tokenize='trigram'` —— R5 要求子串精确匹配，unicode61 只按整 token /
-- token 前缀命中（`dsdf@asdf34Fx3%` 里搜 `Fx3` 永远落空）。trigram 从 SQLite
-- 3.34 起随 FTS5 内置，bundled 版本已支持（`SQLITE_FTS5_TOKENIZER_TRIGRAMS`
-- 控制的是测试专用的 `fts5_trigram()` 函数，不是 tokenizer 本身）。
-- `remove_diacritics 1` 保留 unicode61 的变音符号折叠（`cafe` 仍能命中 `café`）。
-- 已有库由 `MIGRATION_V6` 重建。
CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
    content,
    note,
    tags_text,
    tokenize='trigram remove_diacritics 1'
);

-- Sync trigger: INSERT on entries (skip vault rows)
-- U6 multi-type plan: file kind entries get paths from metadata_json into
-- the FTS content column (filename search via R22). Image/rtf/color leave
-- content empty (R22: those kinds do not contribute to FTS).
CREATE TRIGGER IF NOT EXISTS entries_fts_insert
AFTER INSERT ON entries
WHEN NEW.is_vault = 0
BEGIN
    INSERT INTO entries_fts(rowid, content, note, tags_text)
    VALUES (
        NEW.id,
        CASE WHEN NEW.content_kind = 'file'
             THEN COALESCE(json_extract(NEW.metadata_json, '$.paths'), '')
             ELSE NEW.content
        END,
        NEW.note,
        COALESCE((SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = NEW.id), '')
    );
END;

-- Sync trigger: UPDATE on entries (handle is_vault transitions and kind changes)
-- U6: same kind-aware content extraction as the INSERT trigger so that
-- kind transitions (e.g. file -> vault -> file) correctly re-index.
-- V5 (性能优化): 加 `WHEN OLD.is_vault = 0 OR NEW.is_vault = 0` —— vault
-- 条目本不在 FTS 里 (`entries_fts_insert` 的 WHEN 已短路), 纯 vault 内的
-- UPDATE (pin 切换 / `verify_decryptable_on_startup` 翻 is_decryptable /
-- `set_vault_flag` 推进 pending / `encrypt_into_vault` 写密文) 走到这里
-- 会做一次空 DELETE 加一次空 INSERT, 既浪费又让 N 条 vault UPDATE 串成
-- O(N) FTS 维护. vault↔plain 跨边界仍正确重索引 (任一端是明文就要重做).
CREATE TRIGGER IF NOT EXISTS entries_fts_update
AFTER UPDATE ON entries
WHEN OLD.is_vault = 0 OR NEW.is_vault = 0
BEGIN
    DELETE FROM entries_fts WHERE rowid = OLD.id;
    INSERT INTO entries_fts(rowid, content, note, tags_text)
    SELECT
        NEW.id,
        CASE WHEN NEW.is_vault = 0
             THEN CASE WHEN NEW.content_kind = 'file'
                       THEN COALESCE(json_extract(NEW.metadata_json, '$.paths'), '')
                       ELSE NEW.content
                  END
             ELSE ''
        END,
        CASE WHEN NEW.is_vault = 0 THEN NEW.note ELSE '' END,
        CASE WHEN NEW.is_vault = 0 THEN COALESCE((SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = NEW.id), '') ELSE '' END
    WHERE NEW.is_vault = 0;
END;

-- Sync trigger: DELETE on entries
CREATE TRIGGER IF NOT EXISTS entries_fts_delete
AFTER DELETE ON entries
BEGIN
    DELETE FROM entries_fts WHERE rowid = OLD.id;
END;

-- Sync trigger: INSERT on tags — update tags_text in FTS for the associated entry
CREATE TRIGGER IF NOT EXISTS tags_fts_insert
AFTER INSERT ON tags
WHEN (SELECT is_vault FROM entries WHERE id = NEW.entry_id) = 0
BEGIN
    UPDATE entries_fts
    SET tags_text = (
        SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = NEW.entry_id
    )
    WHERE rowid = NEW.entry_id;
END;

-- Sync trigger: DELETE on tags — update tags_text in FTS for the associated entry
CREATE TRIGGER IF NOT EXISTS tags_fts_delete
AFTER DELETE ON tags
WHEN (SELECT is_vault FROM entries WHERE id = OLD.entry_id) = 0
BEGIN
    UPDATE entries_fts
    SET tags_text = (
        SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = OLD.entry_id
    )
    WHERE rowid = OLD.entry_id;
END;
"#;

/// DDL for schema version 2 (U9).
///
/// Adds the `nonce` column holding the 12-byte AES-GCM nonce for vault
/// entries.  `NULL` for non-vault rows.  Additive migration: existing V1
/// databases upgrade in place, no data rewrite.  The ciphertext itself is
/// stored base64-encoded in the existing `content` TEXT column so that
/// `Entry.content` stays valid UTF-8 for every read path.
pub const MIGRATION_V2: &str = r#"
ALTER TABLE entries ADD COLUMN nonce BLOB;
"#;

/// DDL for schema version 3 (multi-type plan).
///
/// Adds five columns supporting non-text clipboard kinds (image/file/rtf/color):
/// - `content_kind TEXT NOT NULL DEFAULT 'text'`: stable kind label written by
///   [`ContentKind::as_str`]. Existing V2 rows back-fill to `'text'`.
/// - `mime_type TEXT`: MIME type for binary payloads (e.g. `image/png`).
///   `NULL` for text entries.
/// - `payload BLOB`: opaque binary content (image bytes / RTF bytes / JSON-
///   encoded file path array / color encoded data). `NULL` for text entries.
/// - `payload_nonce BLOB`: 12-byte AES-GCM nonce for the encrypted payload
///   (vault binary entries). Independent from `nonce` which protects the
///   text `content` ciphertext. `NULL` for non-vault rows.
/// - `metadata_json TEXT`: structured metadata (image dimensions, file paths,
///   RGBA string, etc.). Searchable for `file` kind filename tokens via the
///   `entries_fts_*` triggers.
///
/// All five additions are additive: existing rows keep their values, and the
/// `content_kind` default ensures no NULL constraint violations on V2 data.
pub const MIGRATION_V3: &str = r#"
ALTER TABLE entries ADD COLUMN content_kind TEXT NOT NULL DEFAULT 'text';
ALTER TABLE entries ADD COLUMN mime_type TEXT;
ALTER TABLE entries ADD COLUMN payload BLOB;
ALTER TABLE entries ADD COLUMN payload_nonce BLOB;
ALTER TABLE entries ADD COLUMN metadata_json TEXT;
"#;

/// DDL for schema version 4 (删除单条记录 / 清空不可解密条目).
///
/// Adds `idx_tags_entry_id` on `tags(entry_id)`. Both delete paths
/// (`Storage::delete_entry` and `clear_undecryptable`) resolve the tags of the
/// doomed entries by `entry_id`, and the `tags_fts_insert` / `tags_fts_delete`
/// triggers do the same lookup on every tag write — without the index each of
/// those is a full scan of `tags`.
///
/// Additive: no data rewrite, and `IF NOT EXISTS` keeps it re-runnable.
pub const MIGRATION_V4: &str = r#"
CREATE INDEX IF NOT EXISTS idx_tags_entry_id ON tags(entry_id);
"#;

/// DDL for schema version 5 (性能优化 W1).
///
/// 加 4 个索引覆盖热路径（KTD4）：
/// - `idx_tags_tag` —— 「标签」tab / `Storage::tag_counts` 的 `GROUP BY tag` /
///   `is_fts_hostile` 兜底里的 `tags.tag LIKE ?` 都靠它。
/// - `idx_entries_auto_category` —— 「类别」tab / `Storage::category_counts`
///   / `get_by_category` 的 `WHERE auto_category = ?`。
/// - `idx_entries_vault_decryptable` —— `verify_decryptable_on_startup` 的
///   `WHERE is_vault = 1` + `clear_undecryptable` 的子查询
///   `is_vault = 1 AND is_decryptable = 0`。
/// - `idx_entries_vault_created_at` —— `list_recent` 的
///   `ORDER BY ... is_pinned DESC, ... pinned_at DESC, created_at DESC`
///   配合 `WHERE is_vault = 0` 走 partial index 取消 sort。
///
/// 全部 `IF NOT EXISTS`, 老库升级与全新库同形; 与既有 V2/V3/V4 风格对齐:
/// 一次 `execute_batch` + 一次 `PRAGMA user_version = 5`。
pub const MIGRATION_V5: &str = r#"
CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
CREATE INDEX IF NOT EXISTS idx_entries_auto_category ON entries(auto_category);
CREATE INDEX IF NOT EXISTS idx_entries_vault_decryptable ON entries(is_vault, is_decryptable);
CREATE INDEX IF NOT EXISTS idx_entries_vault_created_at ON entries(is_vault, created_at DESC);
"#;

/// DDL for schema version 6（#38：子串搜索）.
///
/// 把 `entries_fts` 的 tokenizer 从 `unicode61` 换成 `trigram`：
/// - `unicode61` 只按整 token / token 前缀命中。`dsdf@asdf34Fx3%` 的 token 是
///   `dsdf` / `asdf34Fx3`，搜 `Fx3`（token 中段）永远 0 行，违背 R5 的
///   「子串精确匹配」。
/// - trigram 从 SQLite 3.34 起随 FTS5 内置：`MATCH '"Fx3"*'` 能命中上述内容，
///   且有大小写不敏感、走倒排索引的优势（不需要 LIKE 全表扫）。
///   `remove_diacritics 1` 保留 unicode61 的变音符号折叠（`cafe` 仍命中 `café`）。
///
/// tokenizer 是建表参数，改不了，只能重建。`entries_fts` 的 rowid 与三列内容
/// 都能从 `entries` 重算，所以 drop 后按触发器**同一份** kind-aware 逻辑回填
/// （file 取 `metadata_json.paths`，vault 条目不进 FTS）。`json_valid` 守卫：
/// 畸形 `metadata_json` 会让 `json_extract` 抛错、整个迁移事务回滚，之后每次
/// 启动都失败；这里把它降级成空 content（与 NULL 同处理）。
///
/// 顺带重建两个 **entries 侧** FTS 触发器：`CREATE TRIGGER IF NOT EXISTS` 只在
/// 全新库执行，U6 之前创建的库会一直留着不认识 `content_kind` 的旧触发器，
/// 回填后的 file 路径会被下一次普通 UPDATE 改回空 content。
pub const MIGRATION_V6: &str = r#"
DROP TABLE IF EXISTS entries_fts;
CREATE VIRTUAL TABLE entries_fts USING fts5(
    content,
    note,
    tags_text,
    tokenize='trigram remove_diacritics 1'
);
INSERT INTO entries_fts(rowid, content, note, tags_text)
SELECT
    e.id,
    CASE WHEN e.content_kind = 'file' AND json_valid(e.metadata_json)
         THEN COALESCE(json_extract(e.metadata_json, '$.paths'), '')
         WHEN e.content_kind = 'file' THEN ''
         ELSE e.content
    END,
    e.note,
    COALESCE((SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = e.id), '')
FROM entries e
WHERE e.is_vault = 0;
DROP TRIGGER IF EXISTS entries_fts_insert;
DROP TRIGGER IF EXISTS entries_fts_update;
CREATE TRIGGER entries_fts_insert
AFTER INSERT ON entries
WHEN NEW.is_vault = 0
BEGIN
    INSERT INTO entries_fts(rowid, content, note, tags_text)
    VALUES (
        NEW.id,
        CASE WHEN NEW.content_kind = 'file'
             THEN COALESCE(json_extract(NEW.metadata_json, '$.paths'), '')
             ELSE NEW.content
        END,
        NEW.note,
        COALESCE((SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = NEW.id), '')
    );
END;
CREATE TRIGGER entries_fts_update
AFTER UPDATE ON entries
WHEN OLD.is_vault = 0 OR NEW.is_vault = 0
BEGIN
    DELETE FROM entries_fts WHERE rowid = OLD.id;
    INSERT INTO entries_fts(rowid, content, note, tags_text)
    SELECT
        NEW.id,
        CASE WHEN NEW.is_vault = 0
             THEN CASE WHEN NEW.content_kind = 'file'
                       THEN COALESCE(json_extract(NEW.metadata_json, '$.paths'), '')
                       ELSE NEW.content
                  END
             ELSE ''
        END,
        CASE WHEN NEW.is_vault = 0 THEN NEW.note ELSE '' END,
        CASE WHEN NEW.is_vault = 0 THEN COALESCE((SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = NEW.id), '') ELSE '' END
    WHERE NEW.is_vault = 0;
END;
"#;

/// Run all schema migrations, bringing the database from whatever
/// `user_version` it currently has up to date.
///
/// Handles 0 → 1 (V1 base schema), 1 → 2 (add `nonce`), 2 → 3 (add
/// content_kind / mime_type / payload / payload_nonce / metadata_json),
/// 3 → 4 (index `tags.entry_id`), 4 → 5 (4 hot-path indexes +
/// `entries_fts_update` WHEN gate, in `SCHEMA_V1`), and 5 → 6 (rebuild
/// `entries_fts` with the trigram tokenizer + reindex).
pub fn run_migrations(conn: &Connection) -> Result<()> {
    let mut user_version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if user_version == 0 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(SCHEMA_V1)?;
        tx.execute("PRAGMA user_version = 1", [])?;
        tx.commit()?;
        user_version = 1;
    }

    if user_version == 1 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_V2)?;
        tx.execute("PRAGMA user_version = 2", [])?;
        tx.commit()?;
        user_version = 2;
    }

    if user_version == 2 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_V3)?;
        tx.execute("PRAGMA user_version = 3", [])?;
        tx.commit()?;
        user_version = 3;
    }

    if user_version == 3 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_V4)?;
        tx.execute("PRAGMA user_version = 4", [])?;
        tx.commit()?;
        user_version = 4;
    }

    if user_version == 4 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_V5)?;
        tx.execute("PRAGMA user_version = 5", [])?;
        tx.commit()?;
        user_version = 5;
    }

    if user_version == 5 {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATION_V6)?;
        tx.execute("PRAGMA user_version = 6", [])?;
        tx.commit()?;
    }

    Ok(())
}

/// Run migrations on a fresh in-memory database, returning the connection.
/// Used by app-level integration tests.
#[cfg(test)]
pub fn new_test_db() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    run_migrations(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::OpenFlags;

    /// 当前最新 schema 版本。加迁移时改这里一处，其余断言跟着走。
    const LATEST_VERSION: i32 = 6;

    fn in_memory_rw() -> Connection {
        Connection::open_in_memory_with_flags(
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .unwrap()
    }

    #[test]
    fn schema_v1_runs_without_error() {
        let conn = in_memory_rw();
        run_migrations(&conn).unwrap();
        let uv: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, LATEST_VERSION);
    }

    #[test]
    fn tables_exist() {
        let conn = in_memory_rw();
        run_migrations(&conn).unwrap();

        for table in &["entries", "tags", "settings"] {
            let count: i32 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                    ),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table '{table}' should exist");
        }

        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='entries_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "entries_fts should exist");
    }

    #[test]
    fn migration_idempotent() {
        let conn = in_memory_rw();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap(); // second call should be no-op
        let uv: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, LATEST_VERSION);
    }

    #[test]
    fn schema_migration_zero_to_one() {
        // Simulate a fresh DB with user_version=0.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA user_version = 0", []).unwrap();

        run_migrations(&conn).unwrap();

        let uv: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, LATEST_VERSION);

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

    #[test]
    fn migration_one_to_two_adds_nonce() {
        // Simulate a V1 DB (schema applied, user_version=1, no nonce column).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute("PRAGMA user_version = 1", []).unwrap();

        // Pre-migration: nonce column must not exist yet.
        let has_nonce_before = column_exists(&conn, "entries", "nonce");
        assert!(!has_nonce_before, "V1 must not have the nonce column");

        run_migrations(&conn).unwrap();

        let uv: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            uv, LATEST_VERSION,
            "should be at the latest version after migration"
        );
        assert!(
            column_exists(&conn, "entries", "nonce"),
            "V3 must retain the nonce column from V2"
        );
        assert!(
            column_exists(&conn, "entries", "content_kind"),
            "V3 must add content_kind column"
        );
        assert!(
            column_exists(&conn, "entries", "mime_type"),
            "V3 must add mime_type column"
        );
        assert!(
            column_exists(&conn, "entries", "payload"),
            "V3 must add payload column"
        );
        assert!(
            column_exists(&conn, "entries", "payload_nonce"),
            "V3 must add payload_nonce column"
        );
        assert!(
            column_exists(&conn, "entries", "metadata_json"),
            "V3 must add metadata_json column"
        );
    }

    /// V4 的索引必须真的建出来：两条删除路径与 `tags_fts_*` 触发器都按
    /// `entry_id` 查 `tags`，没有索引每一次都是全表扫。
    #[test]
    fn migration_v4_creates_tags_entry_id_index() {
        let index_count = |conn: &Connection| -> i32 {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_tags_entry_id'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        // 全新库：一次迁移到底就带上索引。
        let fresh = in_memory_rw();
        run_migrations(&fresh).unwrap();
        assert_eq!(index_count(&fresh), 1, "全新库应建出 idx_tags_entry_id");

        // 老库升级（V3 → V4 这一段单独跑）：不是只有全新库才建索引。
        let old = Connection::open_in_memory().unwrap();
        old.execute_batch(SCHEMA_V1).unwrap();
        old.execute_batch(MIGRATION_V2).unwrap();
        old.execute_batch(MIGRATION_V3).unwrap();
        old.execute("PRAGMA user_version = 3", []).unwrap();
        assert_eq!(index_count(&old), 0, "V3 库在建索引前不该有它");

        run_migrations(&old).unwrap();
        let uv: i32 = old
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, LATEST_VERSION);
        assert_eq!(index_count(&old), 1, "V3 库升级到 V4 也要补上索引");

        // 幂等：再跑一次不报错（`IF NOT EXISTS`）。
        run_migrations(&old).unwrap();
        assert_eq!(index_count(&old), 1);
    }

    /// V5 的 4 个索引必须真的建出来：覆盖 `list_recent` /
    /// `category_counts` / `tag_counts` / `verify_decryptable_on_startup` /
    /// `clear_undecryptable`。若漏建，搜索/启动的热路径会回到全表扫。
    #[test]
    fn migration_v5_creates_hot_path_indexes() {
        let index_names = [
            "idx_tags_tag",
            "idx_entries_auto_category",
            "idx_entries_vault_decryptable",
            "idx_entries_vault_created_at",
        ];
        let has_index = |conn: &Connection, name: &str| -> bool {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                [name],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
                != 0
        };

        // 全新库：一次迁移到底就带上全部 4 个索引。
        let fresh = in_memory_rw();
        run_migrations(&fresh).unwrap();
        for name in &index_names {
            assert!(
                has_index(&fresh, name),
                "全新库应建出 {name}（V5 索引覆盖 list_recent / category_counts / tag_counts / verify_decryptable）"
            );
        }

        // 老库升级（V4 → V5 这一段单独跑）：不是只有全新库才建索引。
        let old = Connection::open_in_memory().unwrap();
        old.execute_batch(SCHEMA_V1).unwrap();
        old.execute_batch(MIGRATION_V2).unwrap();
        old.execute_batch(MIGRATION_V3).unwrap();
        old.execute_batch(MIGRATION_V4).unwrap();
        old.execute("PRAGMA user_version = 4", []).unwrap();
        for name in &index_names {
            assert!(!has_index(&old, name), "V4 库在建索引前不该有 {name}");
        }

        run_migrations(&old).unwrap();
        let uv: i32 = old
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, LATEST_VERSION);
        for name in &index_names {
            assert!(has_index(&old, name), "V4 库升级到 V5 也要补上 {name}");
        }

        // 幂等：再跑一次不报错（`IF NOT EXISTS`），且不会重复建索引。
        run_migrations(&old).unwrap();
        for name in &index_names {
            assert!(has_index(&old, name));
        }
    }

    /// V6 必须把老库的 `entries_fts` 换成 trigram，并把已有明文条目重建进新
    /// 索引 —— 只换表不重灌等于把历史全搜丢。
    #[test]
    fn migration_v6_rebuilds_fts_with_trigram() {
        let fts_sql = |conn: &Connection| -> String {
            conn.query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='entries_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        let fresh = in_memory_rw();
        run_migrations(&fresh).unwrap();
        assert!(
            fts_sql(&fresh).contains("tokenize='trigram"),
            "全新库的 entries_fts 必须用 trigram：{}",
            fts_sql(&fresh)
        );

        let old = legacy_v5_db();
        old.execute_batch(
            "INSERT INTO entries (content_hash, content, note, auto_category)
                 VALUES ('h1', 'dsdf@asdf34Fx3%', 'renewal-check', 'plain');
             INSERT INTO entries (content_hash, content, is_vault, is_decryptable)
                 VALUES ('v1', 'asdf34Fx3 secret', 1, 1);
             INSERT INTO entries (content_hash, content, auto_category, content_kind, metadata_json)
                 VALUES ('f1', '', 'file', 'file', '{\"paths\":[\"/tmp/report.pdf\"]}');
             INSERT INTO tags (entry_id, tag)
                 SELECT id, 'client-acme' FROM entries WHERE content_hash = 'h1';",
        )
        .unwrap();
        assert!(
            !fts_sql(&old).contains("trigram"),
            "前置：老库的 FTS 必须是 unicode61"
        );

        let infix_hits = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE entries_fts MATCH '\"Fx3\"*'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        run_migrations(&old).unwrap();
        let uv: i32 = old
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, LATEST_VERSION);
        assert!(
            fts_sql(&old).contains("tokenize='trigram"),
            "V6 必须把 FTS 换成 trigram：{}",
            fts_sql(&old)
        );
        assert_eq!(
            infix_hits(&old),
            1,
            "已有明文条目必须被重建进新索引（中段子串可命中）"
        );
        let note_hits: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE entries_fts MATCH '\"newal\"*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(note_hits, 1, "已有备注也必须被回填进 note 列");

        let indexed_vault: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE rowid = (SELECT id FROM entries WHERE content_hash = 'v1')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed_vault, 0, "vault 条目不得被回填进 FTS");

        let file_hits: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM entries_fts
                 WHERE entries_fts MATCH '\"report\"*'
                   AND rowid = (SELECT id FROM entries WHERE content_hash = 'f1')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(file_hits, 1, "file 条目的文件名必须仍可搜到（R22）");

        // 迁移后的一次普通 UPDATE 不能把回填的 file 路径抹掉：旧库若还留着
        // pre-U6 的 update 触发器（只写 NEW.content），这里会掉成 0。
        old.execute(
            "UPDATE entries SET is_pinned = 1 WHERE content_hash = 'f1'",
            [],
        )
        .unwrap();
        let file_hits_after_update: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM entries_fts
                 WHERE entries_fts MATCH '\"report\"*'
                   AND rowid = (SELECT id FROM entries WHERE content_hash = 'f1')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            file_hits_after_update, 1,
            "V6 必须把 FTS 触发器刷新成当前版本，普通 UPDATE 后 file 路径仍可搜"
        );

        let tag_hits: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE entries_fts MATCH '\"client-acme\"*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag_hits, 1, "已有标签必须被回填进 tags_text");
    }

    /// V6 遇到畸形 `metadata_json` 不能炸掉整个升级事务（否则每次启动都失败）。
    #[test]
    fn migration_v6_tolerates_malformed_metadata() {
        let old = legacy_v5_db();
        // 实时触发器会当场拒绝畸形 JSON；先摘掉 insert 触发器，模拟「触发器缺失
        // 时期写进来的坏行」，触发器由 V6 重建。
        old.execute("DROP TRIGGER entries_fts_insert", []).unwrap();
        old.execute_batch(
            "INSERT INTO entries (content_hash, content, auto_category, content_kind, metadata_json)
                 VALUES ('bad', '', 'file', 'file', 'not json');",
        )
        .unwrap();

        run_migrations(&old).unwrap();

        let uv: i32 = old
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(uv, LATEST_VERSION);
        let indexed: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE rowid = (SELECT id FROM entries WHERE content_hash = 'bad')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed, 1,
            "畸形 metadata 的 file 行应索引为空 content，而不是让迁移失败"
        );

        // V6 重建的 insert 触发器必须继续工作。
        old.execute(
            "INSERT INTO entries (content_hash, content, auto_category)
                 VALUES ('ok', 'zzprobe123', 'plain')",
            [],
        )
        .unwrap();
        let hits: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE entries_fts MATCH '\"probe123\"*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "V6 重建的 insert 触发器必须继续工作");
    }

    /// 造一个 V5 形状的老库：unicode61 的 `entries_fts` + **pre-U6** 的 update
    /// 触发器（无 WHEN 门，且只写 `NEW.content`）。
    ///
    /// 触发器必须是老形状：V6 若改走 `UPDATE entries SET content = content`
    /// 借触发器回填，用当前 `SCHEMA_V1`（kind-aware）是测不出来的 —— 换成
    /// pre-U6 形状后，file 条目会落成空 content，V6 的 file 断言才会失败。
    fn legacy_v5_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(MIGRATION_V2).unwrap();
        conn.execute_batch(MIGRATION_V3).unwrap();
        conn.execute_batch(MIGRATION_V4).unwrap();
        conn.execute_batch(MIGRATION_V5).unwrap();
        conn.execute_batch(
            "DROP TABLE entries_fts;
             CREATE VIRTUAL TABLE entries_fts USING fts5(
                 content, note, tags_text, tokenize='unicode61'
             );
             DROP TRIGGER entries_fts_update;
             CREATE TRIGGER entries_fts_update
             AFTER UPDATE ON entries
             BEGIN
                 DELETE FROM entries_fts WHERE rowid = OLD.id;
                 INSERT INTO entries_fts(rowid, content, note, tags_text)
                 SELECT
                     NEW.id,
                     CASE WHEN NEW.is_vault = 0 THEN NEW.content ELSE '' END,
                     CASE WHEN NEW.is_vault = 0 THEN NEW.note ELSE '' END,
                     CASE WHEN NEW.is_vault = 0 THEN COALESCE((SELECT GROUP_CONCAT(tag, ', ') FROM tags WHERE entry_id = NEW.id), '') ELSE '' END
                 WHERE NEW.is_vault = 0;
             END;",
        )
        .unwrap();
        conn.execute("PRAGMA user_version = 5", []).unwrap();
        conn
    }

    /// Helper: does `table` have a column named `col`?
    fn column_exists(conn: &Connection, table: &str, col: &str) -> bool {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        names.iter().any(|n| n == col)
    }
}
