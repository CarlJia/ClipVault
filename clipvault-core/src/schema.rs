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
CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
    content,
    note,
    tags_text,
    tokenize='unicode61'
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
CREATE TRIGGER IF NOT EXISTS entries_fts_update
AFTER UPDATE ON entries
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

/// Run all schema migrations, bringing the database from whatever
/// `user_version` it currently has up to date.
///
/// Handles 0 → 1 (V1 base schema), 1 → 2 (add `nonce`), 2 → 3 (add
/// content_kind / mime_type / payload / payload_nonce / metadata_json), and
/// 3 → 4 (index `tags.entry_id`).
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
    const LATEST_VERSION: i32 = 4;

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
