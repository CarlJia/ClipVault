//! U3 确定性集成测试。
//!
//! 全部测试都**不**驱动真实剪贴板:分类测试直接调用 `classify`,
//! 去重测试调用纯函数 `should_dedup`,flag / 入库语义测试直接调用
//! `Watcher::process` 并用内存数据库断言行数。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clipvault_core::ContentKind;

use crate::capture::Captured;
use crate::classify::{AutoCategory, classify};
use crate::storage::Storage;
use crate::storage::test_helpers::open_test_db;
use crate::watcher::{Watcher, should_dedup};

/// 组装一个由内存数据库支撑的 Watcher。
///
/// 返回 Watcher、底层 Storage、以及 skip-once flag 的共享句柄,便于断言。
fn make_watcher(guest: bool, skip: bool) -> (Watcher, Arc<Storage>, Arc<AtomicBool>) {
    let storage = Arc::new(open_test_db());
    let guest_mode = Arc::new(AtomicBool::new(guest));
    let skip_next = Arc::new(AtomicBool::new(skip));
    let watcher = Watcher::new(storage.clone(), guest_mode, skip_next.clone());
    (watcher, storage, skip_next)
}

async fn row_count(storage: &Storage) -> usize {
    storage.list_recent(1000).await.unwrap().len()
}

#[test]
fn classify_url_email_path_json_plain() {
    // AE1 的四段样本 → URL / code / JSON / plain。
    assert_eq!(
        classify("https://example.com/api/v1/items?limit=20"),
        AutoCategory::Url
    );
    assert_eq!(classify("def foo(x): return x + 1"), AutoCategory::Code);
    assert_eq!(classify(r#"{"a": 1}"#), AutoCategory::Json);
    assert_eq!(classify("hello world"), AutoCategory::Plain);

    // 额外覆盖 Email 与 Path 两个 AE1 未直接列出但 R4 要求的维度。
    assert_eq!(classify("alice@example.com"), AutoCategory::Email);
    assert_eq!(classify("/usr/local/bin"), AutoCategory::Path);
}

#[test]
fn classify_code_heuristic() {
    // 含花括号、分号、括号的代码片段 → Code。
    assert_eq!(
        classify("fn main() { let x = 1; println!(\"{}\", x); }"),
        AutoCategory::Code
    );
}

#[test]
fn should_dedup_within_window() {
    let now = Instant::now();
    let earlier = now - Duration::from_secs(2);
    assert!(should_dedup(
        Some(("abcd", earlier)),
        "abcd",
        now,
        Duration::from_secs(5),
    ));
}

#[test]
fn should_dedup_outside_window() {
    let now = Instant::now();
    let long_ago = now - Duration::from_secs(6);
    assert!(!should_dedup(
        Some(("abcd", long_ago)),
        "abcd",
        now,
        Duration::from_secs(5),
    ));
    // 指纹不同 → 不去重,即便在窗口内。
    let recent = now - Duration::from_secs(1);
    assert!(!should_dedup(
        Some(("abcd", recent)),
        "efgh",
        now,
        Duration::from_secs(5),
    ));
    // 无历史 → 不去重。
    assert!(!should_dedup(None, "abcd", now, Duration::from_secs(5)));
}

#[tokio::test]
async fn process_respects_skip_next() {
    let (watcher, storage, skip_next) = make_watcher(false, true);

    // skip-once 置位:本次不入库,并复位 flag。
    watcher.process(Captured::text("first copy")).await;
    assert_eq!(row_count(&storage).await, 0, "skip-once 应阻止本次入库");
    assert!(
        !skip_next.load(Ordering::SeqCst),
        "skip-once 应在消费后复位为 false"
    );

    // 再次复制(不同内容,避开去重窗口)应正常入库。
    watcher.process(Captured::text("second copy")).await;
    assert_eq!(row_count(&storage).await, 1, "复位后应恢复入库");
}

#[tokio::test]
async fn process_respects_guest_mode() {
    let (watcher, storage, _skip) = make_watcher(true, false);
    watcher
        .process(Captured::text("captured while guest"))
        .await;
    assert_eq!(row_count(&storage).await, 0, "访客模式下不应入库");
}

#[tokio::test]
async fn process_dedups_within_window() {
    let (watcher, storage, _skip) = make_watcher(false, false);
    watcher.process(Captured::text("repeated text")).await;
    watcher.process(Captured::text("repeated text")).await;
    assert_eq!(
        row_count(&storage).await,
        1,
        "5 秒窗口内的同内容重复只应入库一次"
    );
}

#[tokio::test]
async fn process_handles_binary_capture() {
    // 多类型 plan U2:图片捕获走 binary 路径,auto_category = "image"。
    let (watcher, storage, _skip) = make_watcher(false, false);
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
    watcher
        .process(Captured::image("image/png", png.to_vec(), 1, 1))
        .await;
    let entries = storage.list_recent(10).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content_kind, ContentKind::Image.as_str());
    assert_eq!(entries[0].auto_category, "image");
    assert!(entries[0].payload.is_some());
}

#[tokio::test]
async fn process_keeps_two_distinct_images_in_history() {
    // 回归（用户症状）：连续复制两张不同的图片，历史里必须两条都在。
    // 曾经 binary 指纹只哈希 kind 名，第二张与第一张指纹相同，撞
    // entries.content_hash UNIQUE 后被静默丢弃 —— 表现为「复制图片不进历史」。
    let (watcher, storage, _skip) = make_watcher(false, false);
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
    watcher
        .process(Captured::image("image/png", png.to_vec(), 1, 1))
        .await;

    let mut second = png.to_vec();
    second.push(0);
    watcher
        .process(Captured::image("image/png", second, 1, 1))
        .await;

    let entries = storage.list_recent(10).await.unwrap();
    assert_eq!(entries.len(), 2, "两张不同图片应各占一条历史");
    assert!(
        entries
            .iter()
            .all(|e| e.content_kind == ContentKind::Image.as_str())
    );
}

#[tokio::test]
async fn process_skips_payload_over_25mb() {
    // R23: payload > 25MB 时不入库,日志一行。
    let (watcher, storage, _skip) = make_watcher(false, false);
    let big = vec![0u8; 26 * 1024 * 1024];
    watcher
        .process(Captured::image("image/png", big, 1, 1))
        .await;
    assert_eq!(row_count(&storage).await, 0, "25MB 超限应跳过");
}
