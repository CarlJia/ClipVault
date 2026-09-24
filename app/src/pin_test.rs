//! U7 集成测试：Pin 切换、`pinned_at` 语义、跨视图/跨筛选/跨重启的
//! Pin 优先排序（AE8），以及「置顶胜过时间新近」的排序断言。
//!
//! - 纯 toggle 语义测试走 `open_test_db()`（`:memory:`）。
//! - 需要「重启（重开 DB 句柄）」的测试用**临时文件** DB，`:memory:`
//!   无法在句柄关闭后重开。
//!
//! 排序期望遵循 U5 的 `PIN_ORDER`：`is_pinned DESC, pinned_at DESC,
//! created_at DESC`。AE8 场景「先置顶 C(t0)、再 A(t1)、再 B(t2)」→ 按
//! `pinned_at DESC` → 顺序 B、A、C。

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clipvault_core::Entry;
use rusqlite::params;

use crate::pin::toggle_pin;
use crate::search::{build_search_query, parse_tab};
use crate::storage::Storage;
use crate::storage::test_helpers::open_test_db;

/// 显式写入一条 entry（可控 `pinned_at` / `created_at`），返回 rowid。
#[allow(clippy::too_many_arguments)]
async fn insert(
    db: &Storage,
    hash: &str,
    content: &str,
    category: &str,
    is_pinned: i64,
    pinned_at: Option<i64>,
    created_at: i64,
) -> i64 {
    db.with_conn(move |conn| {
        conn.execute(
            "INSERT INTO entries
                (content_hash, content, note, auto_category,
                 is_pinned, pinned_at, is_vault, is_decryptable, created_at)
             VALUES (?1, ?2, '', ?3, ?4, ?5, 0, 1, ?6)",
            params![hash, content, category, is_pinned, pinned_at, created_at],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .await
    .unwrap()
}

/// 给某条 entry 追加标签（触发 FTS `tags_text` 同步）。
async fn tag(db: &Storage, entry_id: i64, tag: &str) {
    let tag = tag.to_string();
    db.with_conn(move |conn| {
        conn.execute(
            "INSERT INTO tags (entry_id, tag) VALUES (?1, ?2)",
            params![entry_id, tag],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// 模拟命令层：构建搜索计划 + 执行，返回有序 id。
async fn ids(db: &Storage, query: &str, tab: &str) -> Vec<i64> {
    let plan = build_search_query(query, &parse_tab(tab), 50);
    db.search_entries(plan)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect()
}

/// 唯一临时文件 DB 路径（用于「重启」类测试）。
fn temp_db_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!(
        "clipvault_pin_test_{label}_{}_{nanos}.db",
        std::process::id()
    ));
    p
}

/// 单条 entry 的置顶状态快照。
async fn snapshot(db: &Storage, id: i64) -> Entry {
    db.get_by_id(id).await.unwrap().unwrap()
}

// ----------------------------------------------------------------
// 1. toggle 语义
// ----------------------------------------------------------------

#[tokio::test]
async fn toggle_pin_sets_and_clears() {
    let db = open_test_db();
    let id = db.insert_entry("h1", "hello", "plain").await.unwrap();

    // 初始未置顶。
    let before = snapshot(&db, id).await;
    assert!(!before.is_pinned);
    assert!(before.pinned_at.is_none());

    // 第一次切换：置顶 + 写 pinned_at。
    let pinned = toggle_pin(&db, id).await.unwrap();
    assert!(pinned.is_pinned, "第一次切换应置顶");
    assert!(pinned.pinned_at.is_some(), "置顶时必须写 pinned_at");

    // 第二次切换：取消置顶 + 清 pinned_at（NULL 策略）。
    let unpinned = toggle_pin(&db, id).await.unwrap();
    assert!(!unpinned.is_pinned, "第二次切换应取消置顶");
    assert!(
        unpinned.pinned_at.is_none(),
        "取消置顶时 pinned_at 应清为 NULL"
    );
}

#[tokio::test]
async fn toggle_pin_missing_id_errors() {
    let db = open_test_db();
    let err = toggle_pin(&db, 9999).await.unwrap_err();
    assert!(
        matches!(err, rusqlite::Error::QueryReturnedNoRows),
        "对不存在的 id 切换应返回 QueryReturnedNoRows，实际：{err}"
    );
}

// ----------------------------------------------------------------
// 2. 跨重启保留
// ----------------------------------------------------------------

#[tokio::test]
async fn pin_persists_across_reopen() {
    let path = temp_db_path("persist");
    let id;
    {
        let db = Storage::open(&path).unwrap();
        id = db.insert_entry("h1", "durable pin", "plain").await.unwrap();
        let pinned = toggle_pin(&db, id).await.unwrap();
        assert!(pinned.is_pinned);
        assert!(pinned.pinned_at.is_some());
        // db 在此 drop，关闭连接句柄。
    }

    // 重开同一文件 DB，pin 状态与 pinned_at 应完好。
    {
        let db = Storage::open(&path).unwrap();
        let after = snapshot(&db, id).await;
        assert!(after.is_pinned, "重启后仍应保持置顶");
        assert!(after.pinned_at.is_some(), "重启后 pinned_at 应保留");
    }

    std::fs::remove_file(&path).ok();
}

// ----------------------------------------------------------------
// 3. AE8：跨视图 / 跨筛选 / 跨重启 Pin 优先（mandated）
// ----------------------------------------------------------------

#[tokio::test]
async fn cross_view_pin_order() {
    let path = temp_db_path("crossview");

    // 三条条目共享类别 plain 与标签 proj，内容都含 "match"（便于 FTS 命中）。
    // 置顶次序：C(t0=1000) → A(t1=2000) → B(t2=3000)，distinct pinned_at。
    // 期望顺序（pinned_at DESC）：B、A、C。
    let (a, b, c);
    {
        let db = Storage::open(&path).unwrap();
        a = insert(&db, "a", "match apple", "plain", 1, Some(2000), 100).await;
        b = insert(&db, "b", "match banana", "plain", 1, Some(3000), 200).await;
        c = insert(&db, "c", "match cherry", "plain", 1, Some(1000), 300).await;
        tag(&db, a, "proj").await;
        tag(&db, b, "proj").await;
        tag(&db, c, "proj").await;

        let expected = vec![b, a, c];
        assert_eq!(ids(&db, "", "recent").await, expected, "recent tab 顺序");
        assert_eq!(ids(&db, "", "all").await, expected, "all tab 顺序");
        assert_eq!(ids(&db, "", "tag:proj").await, expected, "tag 筛选顺序");
        assert_eq!(
            ids(&db, "", "category:plain").await,
            expected,
            "category 筛选顺序"
        );
        assert_eq!(ids(&db, "match", "all").await, expected, "搜索查询下的顺序");
        // db drop → 关闭句柄，模拟应用退出。
    }

    // 模拟重启：重开同一文件 DB，全部视图顺序应保持 B、A、C。
    {
        let db = Storage::open(&path).unwrap();
        let expected = vec![b, a, c];
        assert_eq!(
            ids(&db, "", "recent").await,
            expected,
            "重启后 recent tab 顺序"
        );
        assert_eq!(ids(&db, "", "all").await, expected, "重启后 all tab 顺序");
        assert_eq!(
            ids(&db, "", "tag:proj").await,
            expected,
            "重启后 tag 筛选顺序"
        );
        assert_eq!(
            ids(&db, "", "category:plain").await,
            expected,
            "重启后 category 筛选顺序"
        );
        assert_eq!(
            ids(&db, "match", "all").await,
            expected,
            "重启后搜索查询下的顺序"
        );
    }

    std::fs::remove_file(&path).ok();
}

// ----------------------------------------------------------------
// 4. 置顶胜过时间新近
// ----------------------------------------------------------------

#[tokio::test]
async fn pinned_sort_beats_recency() {
    let db = open_test_db();
    // 置顶的旧条目（created_at 极小），未置顶的新条目（created_at 极大）。
    let old_pinned = insert(&db, "op", "old pinned", "plain", 1, Some(1000), 1).await;
    let new_unpinned = insert(&db, "nu", "new unpinned", "plain", 0, None, 999_999).await;

    let order = ids(&db, "", "all").await;
    assert_eq!(
        order,
        vec![old_pinned, new_unpinned],
        "置顶的旧条目应排在未置顶的新条目之前"
    );
}
