//! U6 集成测试：备注 / 标签编辑的持久化、标签整组替换、备注入搜索索引、
//! 以及 2000 字符上限的服务端强制。
//!
//! 全部走 `storage::test_helpers::open_test_db()`（`:memory:`），确定性,
//! 直接调用存储层 [`Storage::update_note_and_tags`]（`update_entry` IPC
//! 命令的落地路径）。

use crate::storage::MAX_NOTE_CHARS;
use crate::storage::test_helpers::open_test_db;

/// 备注 + 两个标签写入后可原样读回（R7、R8）。
#[tokio::test]
async fn update_note_and_tags_persists() {
    let db = open_test_db();
    let id = db
        .insert_entry("h1", "some uuid content", "plain")
        .await
        .unwrap();

    db.update_note_and_tags(
        id,
        "需要跟进 Q3 升级",
        &["client-acme".to_string(), "todo".to_string()],
    )
    .await
    .unwrap();

    let entry = db.get_by_id(id).await.unwrap().unwrap();
    assert_eq!(entry.note, "需要跟进 Q3 升级");

    let tags = db.get_all_tags().await.unwrap();
    assert!(tags.contains(&"client-acme".to_string()));
    assert!(tags.contains(&"todo".to_string()));
    assert_eq!(tags.len(), 2, "应恰好持久化两个标签，实际：{tags:?}");
}

/// 标签是集合：从 [a,b] 更新为 [b,c] 后最终集合应为 {b,c}，而非并集。
#[tokio::test]
async fn update_replaces_tags() {
    let db = open_test_db();
    let id = db.insert_entry("h2", "content", "plain").await.unwrap();

    db.update_note_and_tags(id, "", &["a".to_string(), "b".to_string()])
        .await
        .unwrap();
    db.update_note_and_tags(id, "", &["b".to_string(), "c".to_string()])
        .await
        .unwrap();

    let mut tags = db.get_all_tags().await.unwrap();
    tags.sort();
    assert_eq!(
        tags,
        vec!["b".to_string(), "c".to_string()],
        "标签应被整组替换为 {{b,c}}，不应保留旧标签 a"
    );
}

/// 更新备注后，搜索备注中的关键词可命中（AE2：备注入 FTS，走 U2 触发器）。
#[tokio::test]
async fn note_search_hit_after_update() {
    let db = open_test_db();
    // 内容字段本身不含 "Q3"，命中必须来自备注。
    let id = db
        .insert_entry("h3", "550e8400-e29b-41d4-a716-446655440000", "plain")
        .await
        .unwrap();

    db.update_note_and_tags(id, "需要跟进 Q3 升级", &[])
        .await
        .unwrap();

    let results = db.search("Q3", "all", 10).await.unwrap();
    assert!(
        results.iter().any(|e| e.id == id),
        "搜索 'Q3' 应命中备注被更新的条目（AE2）"
    );
}

/// 超过 2000 字符的备注被拒绝，且不写入（服务端强制 R8）。
#[tokio::test]
async fn note_char_cap_enforced() {
    let db = open_test_db();
    let id = db.insert_entry("h4", "content", "plain").await.unwrap();

    // 先写入一条合法备注，确认后续超限写入不会覆盖它。
    db.update_note_and_tags(id, "原始备注", &["keep".to_string()])
        .await
        .unwrap();

    let too_long: String = "x".repeat(MAX_NOTE_CHARS + 1);
    let err = db.update_note_and_tags(id, &too_long, &[]).await;
    assert!(err.is_err(), "超过 {MAX_NOTE_CHARS} 字符的备注应被拒绝");

    // 拒绝时不应有任何副作用：备注与标签保持原状。
    let entry = db.get_by_id(id).await.unwrap().unwrap();
    assert_eq!(entry.note, "原始备注", "超限写入不应覆盖既有备注");
    let tags = db.get_all_tags().await.unwrap();
    assert_eq!(tags, vec!["keep".to_string()], "超限写入不应改动标签");

    // 恰好 2000 字符应被接受（边界）。
    let exact: String = "y".repeat(MAX_NOTE_CHARS);
    db.update_note_and_tags(id, &exact, &[]).await.unwrap();
    let entry = db.get_by_id(id).await.unwrap().unwrap();
    assert_eq!(entry.note.chars().count(), MAX_NOTE_CHARS);
}
