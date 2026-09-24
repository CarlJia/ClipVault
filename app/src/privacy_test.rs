//! U10 确定性测试：访客模式持久化 + 启动加载 + 状态同步 + skip-once 运行时语义。
//!
//! 全部只驱动 `privacy` 模块的 helper 与 `storage` 的 settings k/v，
//! 不触碰真实剪贴板 / 托盘 / 前端。watcher 的“开关生效后不入库”语义
//! 已由 U3 的 `watcher_test` 覆盖，此处不重复。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::privacy;
use crate::storage::Storage;
use crate::storage::test_helpers::open_test_db;

/// 生成一个进程内唯一的临时 DB 路径（用于验证“重开后仍在”）。
fn temp_db_path() -> PathBuf {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!(
        "clipvault_privacy_test_{}_{n}_{nanos}.db",
        std::process::id()
    ));
    p
}

/// (1) 访客模式跨重开保留：写库 → 丢弃连接 → 重开 → 仍为开。
#[tokio::test]
async fn guest_mode_persists() {
    let path = temp_db_path();

    {
        let storage = Storage::open(&path).expect("open temp db");
        privacy::persist_guest_mode(&storage, true).await.unwrap();
    }
    {
        let storage = Storage::open(&path).expect("reopen temp db");
        assert!(
            privacy::load_guest_mode(&storage).await.unwrap(),
            "访客模式应在重开数据库后仍为开启"
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// (2) 全新数据库：访客模式缺省为关。
#[tokio::test]
async fn guest_mode_default_off() {
    let storage = open_test_db();
    assert!(
        !privacy::load_guest_mode(&storage).await.unwrap(),
        "全新库应缺省关闭访客模式"
    );
}

/// (3) skip-once 是纯运行时开关：置位不写库，启动也不加载它。
#[tokio::test]
async fn skip_next_not_persisted() {
    let storage = open_test_db();

    let skip = AtomicBool::new(false);
    privacy::set_skip_next(&skip);
    assert!(
        skip.load(Ordering::SeqCst),
        "set_skip_next 应置位运行时原子"
    );

    // 绝不落库：settings 表里不应出现任何 skip 相关键。
    assert!(
        storage
            .get_setting("skip_next_copy")
            .await
            .unwrap()
            .is_none()
    );
    assert!(storage.get_setting("skip_next").await.unwrap().is_none());

    // 启动加载只碰 guest_mode，新原子始终从 false 起步。
    let boot_skip = AtomicBool::new(false);
    let _ = privacy::boot_load_guest_mode(&storage, &AtomicBool::new(false))
        .await
        .unwrap();
    assert!(
        !boot_skip.load(Ordering::SeqCst),
        "skip-once 开机应始终为 false"
    );
}

/// (4) 启动加载：库里写 "1" → boot-load → 原子被置为 true。
#[tokio::test]
async fn guest_mode_load_sets_flag() {
    let storage = open_test_db();
    storage
        .set_setting(privacy::GUEST_MODE_KEY, "1")
        .await
        .unwrap();

    let flag = AtomicBool::new(false);
    let loaded = privacy::boot_load_guest_mode(&storage, &flag)
        .await
        .unwrap();

    assert!(loaded, "boot-load 应返回 true");
    assert!(flag.load(Ordering::SeqCst), "boot-load 应把原子置为 true");
}

/// (5) 写路径同时更新原子与持久化。
#[tokio::test]
async fn set_guest_mode_updates_both() {
    let storage = open_test_db();
    let flag = AtomicBool::new(false);

    privacy::set_guest_mode(&storage, &flag, true)
        .await
        .unwrap();
    assert!(flag.load(Ordering::SeqCst), "开启应更新原子");
    assert!(
        privacy::load_guest_mode(&storage).await.unwrap(),
        "开启应已持久化到库"
    );

    privacy::set_guest_mode(&storage, &flag, false)
        .await
        .unwrap();
    assert!(!flag.load(Ordering::SeqCst), "关闭应更新原子");
    assert!(
        !privacy::load_guest_mode(&storage).await.unwrap(),
        "关闭应已持久化到库"
    );
}
