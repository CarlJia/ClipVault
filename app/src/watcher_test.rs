//! U3 确定性集成测试。
//!
//! 全部测试都**不**驱动真实剪贴板:分类测试直接调用 `classify`,
//! 去重测试调用纯函数 `should_dedup`,flag / 入库语义测试直接调用
//! `Watcher::process` 并用内存数据库断言行数。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clipvault_core::ContentKind;

use crate::capture::{Captured, poll_first_non_empty};
use crate::classify::{AutoCategory, classify};
use crate::storage::Storage;
use crate::storage::test_helpers::open_test_db;
use crate::watcher::{Watcher, duplicate_fingerprint_log, initial_change_count, should_dedup};

/// 组装一个由内存数据库支撑的 Watcher。
///
/// 返回 Watcher、底层 Storage、以及 skip-once flag 的共享句柄,便于断言。
fn make_watcher(guest: bool, skip: bool) -> (Watcher, Arc<Storage>, Arc<AtomicBool>) {
    let storage = Arc::new(open_test_db());
    let guest_mode = Arc::new(AtomicBool::new(guest));
    let skip_next = Arc::new(AtomicBool::new(skip));
    // W7 / R30：多数用例不关心自写抑制槽，给一份私有的即可。
    let self_writes = Arc::new(tokio::sync::Mutex::new(None));
    let watcher = Watcher::new(storage.clone(), guest_mode, skip_next.clone(), self_writes);
    (watcher, storage, skip_next)
}

/// W7 / R30 用：把自写抑制槽也交出来，让用例能模拟 `commands::copy_log_path`
/// 的「写前登记指纹」这一步。
fn make_watcher_with_self_writes(
    guest: bool,
    skip: bool,
) -> (
    Watcher,
    Arc<Storage>,
    Arc<tokio::sync::Mutex<Option<String>>>,
) {
    let storage = Arc::new(open_test_db());
    let guest_mode = Arc::new(AtomicBool::new(guest));
    let skip_next = Arc::new(AtomicBool::new(skip));
    let self_writes = Arc::new(tokio::sync::Mutex::new(None));
    let watcher = Watcher::new(storage.clone(), guest_mode, skip_next, self_writes.clone());
    (watcher, storage, self_writes)
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

/// 「启动不重捕既有剪贴板」这条行为的两个前提,各钉一半。
///
/// 前半:`Watcher::new` 把 `last_change_count` 播种成**当前**序列号(而不是 `-1`)。
/// 后半:`poll_first_non_empty` 在 `change_count <= prev` 时直接返回 `None` —— 那句
/// `return` 在 `NSPasteboard` 读取**之前**,所以本用例不碰用户的剪贴板(与文件顶上那条
/// 约定一致)。两半合起来才是完整结论:重启后,库里没有的那份内容不会随既有剪贴板复活
/// —— 这正是「删除能扛过重启」与「加密区条目的明文不会因重启回到普通历史」的机制。
#[test]
fn last_change_count_is_seeded_with_current_sequence_number() {
    assert_eq!(initial_change_count(Some(42)), 42, "拿得到序列号就播种它");
    assert_eq!(initial_change_count(None), -1, "拿不到才退回 -1");
    assert!(
        poll_first_non_empty(42, 42).is_none(),
        "播种后首轮判据必须是不捕获"
    );
    assert!(
        poll_first_non_empty(41, 42).is_none(),
        "序列号没增长(理论上不该发生)同样不捕获"
    );
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

#[test]
fn duplicate_fingerprint_log_keeps_prefix_and_truncates_hash() {
    // 这条日志降到 verbose 之后,它是该分支**唯一可被自动化断言的部分**:测试构建不安装
    // 全局 logger(log.rs 的 install_global_logger 在 cfg(test) 下是空实现),真实的
    // log 记录捕获不到,所以格式契约落在这个纯函数上。
    let msg = duplicate_fingerprint_log(
        "Error code 2067: A UNIQUE constraint failed",
        "abcdef0123456789",
    );
    assert!(
        msg.starts_with("[capture] skipped: "),
        "保持 grep 前缀:{msg}"
    );
    assert!(
        msg.contains("hash abcdef01.."),
        "指纹只留前 8 字符(KTD4):{msg}"
    );
    assert!(
        msg.contains("duplicate fingerprint, no new entry"),
        "文案要说清是重复而非故障:{msg}"
    );

    // 短于 8 字符照旧全量输出;为空时不能打出残缺的 `hash ..`。
    assert!(duplicate_fingerprint_log("e", "abc").contains("hash abc.."));
    assert!(duplicate_fingerprint_log("e", "").contains("hash <empty>"));
}

#[tokio::test]
async fn process_bumps_existing_entry_to_top_on_re_capture() {
    // 库层去重:换一个 Watcher(内存里没有指纹)再捕获同一内容,必然绕过 5 秒窗口去重、
    // 走到 `content_hash` UNIQUE 冲突分支。该分支现在不再"丢",而是把已有行的
    // `created_at` 刷到此刻 —— 不新增行,同时让 `PIN_ORDER` 把该行重新排到列表顶部
    // (修复用户症状「相同内容再次粘贴还在原位置,找不到」)。
    let storage = Arc::new(open_test_db());
    let make = || {
        Watcher::new(
            storage.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(tokio::sync::Mutex::new(None)),
        )
    };

    make().process(Captured::text("same payload twice")).await;
    assert_eq!(row_count(&storage).await, 1);

    let first_row = storage
        .list_recent(10)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("first capture must produce an entry");
    let first_created_at = first_row.created_at;

    // `created_at` 用秒级 `strftime('%s','now')`,跨越 1100ms 才有可见差。
    tokio::time::sleep(Duration::from_millis(1100)).await;

    make().process(Captured::text("same payload twice")).await;

    // 旧契约保留:行数仍 = 1 (不新增 entry)。
    assert_eq!(
        row_count(&storage).await,
        1,
        "二次复制必须仍走 UNIQUE 冲突分支,不 insert 新行"
    );

    // 新契约:已有行的 created_at 被刷到 > 原始时间。
    let after = storage
        .list_recent(10)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("row must still exist");
    assert_eq!(
        after.id, first_row.id,
        "必须仍是同一行 —— bump_to_top 不该 insert"
    );
    assert!(
        after.created_at > first_created_at,
        "二次复制后 created_at 必须刷新:原 {first_created_at},现在 {}",
        after.created_at
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
    // `list_recent` 走 W1 / ENTRY_LIST_COLS (`payload` 列 `NULL AS`) →
    // 列表返回的 `Entry::payload` 恒为 `None`, 即便库里那一行有真实
    // BLOB。要验 payload 真正入库必须走 `get_by_id` (走 `ENTRY_COLS`)。
    let entries = storage.list_recent(10).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content_kind, ContentKind::Image.as_str());
    assert_eq!(entries[0].auto_category, "image");
    assert!(
        entries[0].payload.is_none(),
        "list_recent 走 ENTRY_LIST_COLS, payload 列为 NULL AS,不 fetch BLOB"
    );

    let id = entries[0].id;
    let by_id = storage.get_by_id(id).await.unwrap().unwrap();
    assert!(
        by_id.payload.as_deref() == Some(png.as_slice()),
        "get_by_id 走 ENTRY_COLS, payload 列真值还原"
    );
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

// -------------------------------------------------------------------------
// W2 性能合约:把 `make_thumbnail` 与 `capture::poll_first_non_empty` 移进
// `tauri::async_runtime::spawn_blocking`。这两条都是 CPU/IO 重活,放回
// executor worker 上一旦遇到 20 MB PNG 截图就把整帧吞掉,W2 改动的目的
// 是让 executor worker 在这一帧可以接 IPC 命令(搜索框输入响应不再被吞)。
//
// 不能在 cfg(test) 里直接 spawn 一个 blocking pool 与 watcher 共存来观察
// 「thumbnail 期间 IPC 能插队」(那要 mount tauri runtime + 跨 IPC stub),
// 代价远大于收益。改用静态合约:源码里 `make_thumbnail` 与 spawn_blocking
// 的相对位置。
// -------------------------------------------------------------------------

/// 辅助:在 `watcher.rs` 源码中找 `marker` 起头、第一个未闭合 `{` 之后的
/// 函数体文本。剪过的边界按字节匹配,函数体内不含字符串字面量的 `}` 时
/// 严格正确;本模块测试关注的几个函数体都不含。
fn function_body_after(src: &str, marker: &str) -> String {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("找不到 marker: {marker}"));
    let body_open = src[start..]
        .find('{')
        .unwrap_or_else(|| panic!("marker 之后没有 `{{`: {marker}"))
        + start
        + 1;
    let bytes = src.as_bytes();
    let mut depth: i32 = 1;
    let mut i = body_open;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    src[body_open..i - 1].to_string()
}

/// W2 / R8:`Watcher::process` 内 `make_thumbnail` 必须**只在**
/// `spawn_blocking` 闭包内调用。如果它出现在 `spawn_blocking` 之前(同步
/// 路径),就是 W2 没生效 —— executor worker 会重新被图像解码吞帧。
///
/// 用括号化的字面量 (`make_thumbnail(` / `spawn_blocking(`) 跳过注释里
/// 的"裸字样",只匹配真正的调用。
#[test]
fn watcher_process_runs_make_thumbnail_inside_spawn_blocking() {
    let src = include_str!("watcher.rs");
    let body = function_body_after(src, "pub async fn process(&self, captured: Captured) {");
    let make_thumb_pos = body
        .find("make_thumbnail(")
        .expect("W2 / R8: watcher::process 应仍调 make_thumbnail(...)");
    let spawn_blocking_pos = body
        .find("spawn_blocking(")
        .expect("W2 / R8: watcher::process 必须用 spawn_blocking(...) 包缩略图生成");
    assert!(
        spawn_blocking_pos < make_thumb_pos,
        "`make_thumbnail(` 必须出现在 `spawn_blocking(` 闭包内; \
         spawn_blocking( 第 {spawn_blocking_pos} 字节, make_thumbnail( 第 {make_thumb_pos} 字节"
    );
}

/// W2 / R9(macOS):`NSPasteboard::generalPasteboard()` 不能出现在
/// `spawn_blocking` 之前 —— `Retained<NSPasteboard>` 是 `!Send`,不能
/// 跨 executor 的 .await 边界持住。所以读 + 探测整段必须闭进
/// spawn_blocking,符号顺序也得是这样。
#[cfg(target_os = "macos")]
#[test]
fn watcher_poll_change_async_macos_moves_pasteboard_into_spawn_blocking() {
    let src = include_str!("watcher.rs");
    // 用 macOS 块前的独有 doc 注释作 marker —— source 中另有
    // cfg(not(...)) 与 cfg(windows) 两份同名函数,通用 marker 会抓到错的那份。
    let body = function_body_after(src, "异步读剪贴板变化（macOS 实现 / 多类型扩展后）");
    let pb_pos = body
        .find("generalPasteboard")
        .expect("macOS poll_change_async 仍应读 NSPasteboard");
    let sb_pos = body
        .find("spawn_blocking(")
        .expect("macOS poll_change_async 必须用 spawn_blocking(...)");
    assert!(
        sb_pos < pb_pos,
        "NSPasteboard::generalPasteboard 必须在 spawn_blocking( 闭包内被调; \
         spawn_blocking( 第 {sb_pos} 字节, generalPasteboard 第 {pb_pos} 字节"
    );
}

/// W2 / R9(Windows):`GetClipboardSequenceNumber` 与
/// `capture::poll_first_non_empty` 都必须落在 `spawn_blocking` 内(后者
/// 要打开剪贴板、调 `GetClipboardData` 等,都是阻塞 syscall)。
#[cfg(target_os = "windows")]
#[test]
fn watcher_poll_change_async_windows_moves_clipboard_io_into_spawn_blocking() {
    let src = include_str!("watcher.rs");
    // 同上:用 Windows 块前的独有 doc 注释作 marker,避免抓到 macOS 版。
    let body = function_body_after(src, "异步读剪贴板变化(Windows 实现 / U1)");
    let seq_pos = body
        .find("GetClipboardSequenceNumber")
        .expect("Windows poll_change_async 应仍读 sequence number");
    let sb_pos = body
        .find("spawn_blocking(")
        .expect("Windows poll_change_async 必须用 spawn_blocking(...)");
    assert!(
        sb_pos < seq_pos,
        "GetClipboardSequenceNumber 必须在 spawn_blocking( 闭包内; \
         spawn_blocking( 第 {sb_pos} 字节, GetClipboardSequenceNumber 第 {seq_pos} 字节"
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

/// W7 / R30：自写抑制。`commands::copy_log_path` 写剪贴板**之前**把要写的内容的
/// 指纹登记进共享槽；watcher 的下一轮捕获见到同一指纹就跳过，不让它变成一条
/// 剪贴板历史。
///
/// 两条不变量：登记过的内容不入库；**登记是一次性的** —— 同一份内容随后被用户
/// 真的复制时，必须正常入库（否则「复制日志路径」会永久毒掉那段文本）。
#[tokio::test]
async fn process_skips_self_written_content() {
    let (watcher, storage, self_writes) = make_watcher_with_self_writes(false, false);

    // 模拟 copy_log_path：写剪贴板前登记指纹。
    let path = "/Users/x/Library/Application Support/ClipVault/clipvault.log";
    *self_writes.lock().await = Some(Captured::text(path).fingerprint());

    watcher.process(Captured::text(path)).await;
    assert_eq!(row_count(&storage).await, 0, "自写内容不该进历史");

    // 抑制路径没有更新 5 秒去重窗口的 last_hash（它在第 4 步才更新，我们在第 3.5
    // 步就 return 了），所以这一次是「用户真的复制了这段文本」，必须入库。
    watcher.process(Captured::text(path)).await;
    assert_eq!(
        row_count(&storage).await,
        1,
        "自写抑制一次性；消费后同一内容的真实复制应正常入库"
    );
}

/// W7 / R30：登记与下一轮捕获**不匹配**时（写剪贴板失败、或用户抢先复制了别的），
/// 那条真实内容必须照常入库；而且登记要被消费掉，不能留成一颗误伤后续复制的雷。
#[tokio::test]
async fn self_write_slot_does_not_swallow_a_different_capture() {
    let (watcher, storage, self_writes) = make_watcher_with_self_writes(false, false);
    *self_writes.lock().await = Some(Captured::text("/some/log/path").fingerprint());

    watcher
        .process(Captured::text("user copied something else"))
        .await;
    assert_eq!(row_count(&storage).await, 1, "内容不同时不得误跳过");
    assert!(
        self_writes.lock().await.is_none(),
        "登记必须被消费掉（成功匹配与否都消费）"
    );

    // 槽已空：同一份 log path 随后被真的复制时照常入库。
    watcher.process(Captured::text("/some/log/path")).await;
    assert_eq!(row_count(&storage).await, 2);
}
