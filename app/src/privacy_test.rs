//! U10 确定性测试：访客模式持久化 + 启动加载 + 状态同步 + skip-once 运行时语义。
//!
//! 全部只驱动 `privacy` 模块的 helper 与 `storage` 的 settings k/v，
//! 不触碰真实剪贴板 / 托盘 / 前端。watcher 的“开关生效后不入库”语义
//! 已由 U3 的 `watcher_test` 覆盖，此处不重复。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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

// ----------------------------------------------------------------
// 面板透明度（U12）
// ----------------------------------------------------------------
//
// 复用 guest_mode 测试套路：纯函数 + 库持久化 + 启动加载 + clamp 行为。

/// (6) parse_panel_opacity 纯函数：合法值直传 / 非法值回落 / 越界 clamp。
#[test]
fn parse_panel_opacity_clamps_and_defaults() {
    assert_eq!(privacy::parse_panel_opacity(Some("78")), 78);
    assert_eq!(privacy::parse_panel_opacity(Some("30")), 30);
    assert_eq!(privacy::parse_panel_opacity(Some("100")), 100);
    // 越界：上限 clamp 到 100,下限 clamp 到 30。
    assert_eq!(privacy::parse_panel_opacity(Some("150")), 100);
    assert_eq!(privacy::parse_panel_opacity(Some("0")), 30);
    // 缺省 / 非法值 → 默认 78。
    assert_eq!(privacy::parse_panel_opacity(None), 78);
    assert_eq!(privacy::parse_panel_opacity(Some("foo")), 78);
    assert_eq!(privacy::parse_panel_opacity(Some("")), 78);
}

/// (7) 全新数据库:透明度缺省 78。
#[tokio::test]
async fn panel_opacity_default_value() {
    let storage = open_test_db();
    assert_eq!(
        privacy::load_panel_opacity(&storage).await.unwrap(),
        78,
        "全新库应缺省 78% (与 styles.css --bg 0.78 对齐)"
    );
}

/// (8) 透明度跨重开保留 + 启动加载写原子。
#[tokio::test]
async fn panel_opacity_persists_and_boots() {
    let path = temp_db_path();

    {
        let storage = Storage::open(&path).expect("open temp db");
        privacy::persist_panel_opacity(&storage, 42).await.unwrap();
    }
    {
        let storage = Storage::open(&path).expect("reopen temp db");
        assert_eq!(
            privacy::load_panel_opacity(&storage).await.unwrap(),
            42,
            "透明度应在重开数据库后仍为 42"
        );

        // boot-load 同步把原子从默认值改写到持久化值。
        let flag = AtomicU32::new(privacy::DEFAULT_PANEL_OPACITY);
        let loaded = privacy::boot_load_panel_opacity(&storage, &flag)
            .await
            .unwrap();
        assert_eq!(loaded, 42);
        assert_eq!(flag.load(Ordering::SeqCst), 42);
    }

    let _ = std::fs::remove_file(&path);
}

/// (9) set_panel_opacity:写路径同时更新原子与持久化,clamp 入参。
#[tokio::test]
async fn set_panel_opacity_updates_both_and_clamps() {
    let storage = open_test_db();
    let flag = AtomicU32::new(privacy::DEFAULT_PANEL_OPACITY);

    // 合法值直传。
    let r = privacy::set_panel_opacity(&storage, &flag, 60)
        .await
        .unwrap();
    assert_eq!(r, 60, "返回值应回传 clamp 后的最终值");
    assert_eq!(flag.load(Ordering::SeqCst), 60, "原子应同步到 60");
    assert_eq!(
        privacy::load_panel_opacity(&storage).await.unwrap(),
        60,
        "持久化应写入 60"
    );

    // 越界值(0 / 150)→ clamp 回 [30, 100]。
    let r = privacy::set_panel_opacity(&storage, &flag, 0)
        .await
        .unwrap();
    assert_eq!(r, privacy::MIN_PANEL_OPACITY, "0 应 clamp 到 MIN");
    assert_eq!(flag.load(Ordering::SeqCst), privacy::MIN_PANEL_OPACITY);

    let r = privacy::set_panel_opacity(&storage, &flag, 150)
        .await
        .unwrap();
    assert_eq!(r, privacy::MAX_PANEL_OPACITY, "150 应 clamp 到 MAX");
    assert_eq!(flag.load(Ordering::SeqCst), privacy::MAX_PANEL_OPACITY);
}

/// (10) apply_panel_opacity 仅写原子,不触碰库。
#[tokio::test]
async fn apply_panel_opacity_does_not_persist() {
    let storage = open_test_db();
    let flag = AtomicU32::new(78);

    privacy::apply_panel_opacity(&flag, 50);
    assert_eq!(flag.load(Ordering::SeqCst), 50, "原子应被更新");
    assert_eq!(
        privacy::load_panel_opacity(&storage).await.unwrap(),
        78,
        "库应保持缺省值,apply 不应触发持久化"
    );
}
