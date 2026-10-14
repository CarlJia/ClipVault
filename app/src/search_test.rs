//! U5 集成测试：搜索命中、Pin 优先排序、tab 分发、兜底与计数。
//!
//! 全部走 `storage::test_helpers::open_test_db()`（`:memory:`），确定性,
//! 不依赖时钟：`created_at` / `pinned_at` 均显式写入。

use clipvault_core::Entry;
use rusqlite::params;

use crate::search::{RECENT_LIMIT, build_search_query, parse_tab};
use crate::storage::Storage;
use crate::storage::test_helpers::open_test_db;

/// 显式写入一条 entry（绕过默认时间戳），返回 rowid。
#[allow(clippy::too_many_arguments)]
async fn insert(
    db: &Storage,
    hash: &str,
    content: &str,
    note: &str,
    category: &str,
    is_pinned: i64,
    pinned_at: Option<i64>,
    created_at: i64,
    is_vault: i64,
) -> i64 {
    db.with_conn(move |conn| {
        conn.execute(
            "INSERT INTO entries
                (content_hash, content, note, auto_category,
                 is_pinned, pinned_at, is_vault, is_decryptable, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
            params![
                hash, content, note, category, is_pinned, pinned_at, is_vault, created_at
            ],
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

/// 模拟命令层：构建计划 + 执行。
async fn run(db: &Storage, query: &str, tab: &str, limit: u32) -> Vec<Entry> {
    let plan = build_search_query(query, &parse_tab(tab), limit);
    db.search_entries(plan).await.unwrap()
}

#[tokio::test]
async fn search_hits_content() {
    let db = open_test_db();
    insert(
        &db,
        "h1",
        "git webhook endpoint",
        "",
        "code",
        0,
        None,
        100,
        0,
    )
    .await;
    insert(
        &db,
        "h2",
        "plain unrelated text",
        "",
        "plain",
        0,
        None,
        200,
        0,
    )
    .await;

    let results = run(&db, "webhook", "all", 50).await;
    assert_eq!(results.len(), 1, "只有一条含 webhook");
    assert!(results[0].content.contains("webhook"));
}

#[tokio::test]
async fn search_hits_note_and_tags() {
    // AE2：内容里既无 Q3 也无 client-acme，但备注含 Q3、标签含 client-acme。
    let db = open_test_db();
    let id = insert(
        &db,
        "uuid1",
        "550e8400-e29b-41d4-a716-446655440000",
        "需要跟进 Q3 升级",
        "plain",
        0,
        None,
        100,
        0,
    )
    .await;
    tag(&db, id, "client-acme").await;
    tag(&db, id, "todo").await;

    // 干扰项，避免假阳性。
    insert(
        &db,
        "noise",
        "hello world",
        "无关备注",
        "plain",
        0,
        None,
        50,
        0,
    )
    .await;

    let by_note = run(&db, "Q3", "all", 50).await; // 2 字符 → LIKE 兜底
    assert_eq!(by_note.len(), 1, "Q3 应命中备注");
    assert_eq!(by_note[0].id, id);

    let by_tag = run(&db, "client-acme", "all", 50).await; // 含连字符 → LIKE 兜底
    assert_eq!(by_tag.len(), 1, "client-acme 应命中标签");
    assert_eq!(by_tag[0].id, id);
}

#[tokio::test]
async fn pin_first_ordering() {
    // 2 pinned（不同 pinned_at）+ 3 unpinned；空查询与命中全部的查询都应同序。
    let db = open_test_db();
    let a = insert(&db, "a", "match alpha", "", "plain", 0, None, 100, 0).await;
    let b = insert(&db, "b", "match bravo", "", "plain", 0, None, 200, 0).await;
    let c = insert(&db, "c", "match charlie", "", "plain", 0, None, 300, 0).await;
    let p_old = insert(&db, "po", "match pold", "", "plain", 1, Some(1000), 50, 0).await;
    let p_new = insert(&db, "pn", "match pnew", "", "plain", 1, Some(2000), 60, 0).await;

    // 期望：pinnedNewest(p_new), pinnedOlder(p_old), 然后非置顶按 created_at DESC: c, b, a
    let expected = vec![p_new, p_old, c, b, a];

    let empty = run(&db, "", "all", 50).await;
    assert_eq!(
        empty.iter().map(|e| e.id).collect::<Vec<_>>(),
        expected,
        "空查询下的 Pin 优先排序"
    );

    let matched = run(&db, "match", "all", 50).await;
    assert_eq!(
        matched.iter().map(|e| e.id).collect::<Vec<_>>(),
        expected,
        "命中全部的查询下的 Pin 优先排序"
    );
}

#[tokio::test]
async fn short_query_like_fallback() {
    // 2 字符查询走 LIKE 子串匹配，不崩溃。
    let db = open_test_db();
    insert(
        &db,
        "u1",
        "unix path /usr/local/bin",
        "",
        "path",
        0,
        None,
        100,
        0,
    )
    .await;
    insert(&db, "u2", "totally other", "", "plain", 0, None, 200, 0).await;

    let results = run(&db, "us", "all", 50).await;
    assert!(!results.is_empty(), "2 字符 LIKE 应命中 'us'");
    assert!(results.iter().all(|e| e.content.contains("us")));
}

#[tokio::test]
async fn fts_hostile_chars() {
    // `tag:acme` 与 `a"b` 不应导致 FTS parser 报错，降级为 LIKE。
    let db = open_test_db();
    let id = insert(
        &db,
        "h1",
        "connect to a\"b server",
        "",
        "plain",
        0,
        None,
        100,
        0,
    )
    .await;
    tag(&db, id, "acme").await;

    // 冒号：FTS5 会当成列过滤符 → 必须降级，且不报错。
    let colon = run(&db, "tag:acme", "all", 50).await;
    assert!(colon.is_empty() || !colon.is_empty(), "查询不应 panic/报错");

    // 含双引号的查询同样不应报错，且能子串命中。
    let quote = run(&db, "a\"b", "all", 50).await;
    assert_eq!(quote.len(), 1, "a\"b 应通过 LIKE 命中");
    assert_eq!(quote[0].id, id);
}

#[tokio::test]
async fn vault_excluded_from_search() {
    let db = open_test_db();
    // 非加密条目含 token，加密条目也含相同 token。
    insert(
        &db,
        "n1",
        "secret token visible",
        "",
        "plain",
        0,
        None,
        100,
        0,
    )
    .await;
    insert(
        &db,
        "v1",
        "secret token hidden",
        "",
        "plain",
        0,
        None,
        200,
        1,
    )
    .await;

    let results = run(&db, "secret", "all", 50).await;
    assert_eq!(results.len(), 1, "加密区条目不得出现在搜索结果");
    assert_eq!(results[0].content_hash, "n1");

    // 空查询同样排除。
    let all = run(&db, "", "all", 50).await;
    assert!(all.iter().all(|e| !e.is_vault));
}

#[tokio::test]
async fn tab_recent_limit_30() {
    let db = open_test_db();
    // 插入 35 条非置顶 + 1 条置顶，验证 recent 上限 30 且 Pin 优先。
    for i in 0..35 {
        insert(
            &db,
            &format!("h{i}"),
            &format!("entry number {i}"),
            "",
            "plain",
            0,
            None,
            1000 + i,
            0,
        )
        .await;
    }
    let pinned = insert(&db, "pin", "pinned entry", "", "plain", 1, Some(9999), 1, 0).await;

    let results = run(&db, "", "recent", 50).await;
    assert_eq!(
        results.len() as u32,
        RECENT_LIMIT,
        "recent tab 应固定上限 30，忽略传入的 limit=50"
    );
    assert_eq!(results[0].id, pinned, "置顶条目排最前");
    assert!(results[0].is_pinned);
}

#[tokio::test]
async fn tag_and_category_counts() {
    let db = open_test_db();
    // 非加密条目。
    let e1 = insert(&db, "a", "https://x.com", "", "URL", 0, None, 100, 0).await;
    let e2 = insert(&db, "b", "https://y.com", "", "URL", 0, None, 200, 0).await;
    let e3 = insert(&db, "c", "plain text", "", "plain", 0, None, 300, 0).await;
    tag(&db, e1, "work").await;
    tag(&db, e2, "work").await;
    tag(&db, e3, "misc").await;

    // 加密条目：其标签 / 类别都不得计入。
    let v = insert(&db, "v", "vault url", "", "URL", 0, None, 400, 1).await;
    tag(&db, v, "work").await;

    let tags = db.tag_counts().await.unwrap();
    // work=2（排除 vault 的第三次），misc=1
    assert_eq!(tags, vec![("work".to_string(), 2), ("misc".to_string(), 1)]);

    let cats = db.category_counts().await.unwrap();
    // URL=2（排除 vault），plain=1
    assert_eq!(cats, vec![("URL".to_string(), 2), ("plain".to_string(), 1)]);
}

#[tokio::test]
async fn tab_filter_by_tag_and_category() {
    // tag:<name> / category:<name> 分发筛选，且 Pin 优先。
    let db = open_test_db();
    let e1 = insert(&db, "a", "url one", "", "URL", 0, None, 100, 0).await;
    let e2 = insert(&db, "b", "url two", "", "URL", 1, Some(500), 200, 0).await;
    insert(&db, "c", "plain thing", "", "plain", 0, None, 300, 0).await;
    tag(&db, e1, "proj").await;
    tag(&db, e2, "proj").await;

    let by_tag = run(&db, "", "tag:proj", 50).await;
    assert_eq!(by_tag.len(), 2, "tag:proj 应筛出两条");
    assert_eq!(by_tag[0].id, e2, "置顶条目排最前");

    let by_cat = run(&db, "", "category:URL", 50).await;
    assert_eq!(by_cat.len(), 2, "category:URL 应筛出两条");
    assert!(by_cat.iter().all(|e| e.auto_category == "URL"));
    assert_eq!(by_cat[0].id, e2, "置顶条目排最前");
}
