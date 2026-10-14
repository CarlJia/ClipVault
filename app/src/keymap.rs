//! HotkeyMap 持久化层（U1 / U2）。
//!
//! 与 [`crate::privacy::panel_opacity`] 同形：
//! - `parse_hotkeys` — 纯函数，把 DB 原始 JSON 字符串解析为 `HotkeyMap`
//! - `load_persist_hotkeys` — Storage wrapper，读出 parse 失败时返回 `Ok(default())`
//! - `persist_hotkeys` — 写入 `settings` 表 `hotkeys` 行
//! - `boot_load_hotkeys` — 启动路径：load → 写入 `Arc<RwLock<HotkeyMap>>`
//! - `apply_hotkeys` — 运行时更新 in-memory slot
//! - `apply_global_hotkey` — 全局热键 re-register（U2：串起 `hotkey::register`）

use std::sync::RwLock;

use clipvault_core::keymap::{HotkeyMap, parse as parse_keymap_json, serialize};
use rusqlite::Result as SqliteResult;

use crate::storage::Storage;

/// `settings` 表中 hotkeys JSON 的键名。
pub const HOTKEYS_KEY: &str = "hotkeys";

/// 从 `settings` 表读取的原始 JSON 字符串解析为 `HotkeyMap`。
///
/// 非法 JSON / 缺字段 / modifier-only → 返回 `Err`，由 `load_persist_hotkeys` 捕获
/// 并在 `Err` 时返回 `Ok(default())`（R3：静默回落，不修改库）。
pub fn parse_hotkeys(raw: Option<&str>) -> Result<HotkeyMap, String> {
    match raw {
        Some(json) => parse_keymap_json(json),
        None => Ok(HotkeyMap::default_map()),
    }
}

/// 从 `settings` 表加载 HotkeyMap。
///
/// 读出后 parse 失败 → 返回 `Ok(default())` 并**不**写入库（R3 守得住）。
pub async fn load_persist_hotkeys(storage: &Storage) -> SqliteResult<HotkeyMap> {
    let raw = storage.get_setting(HOTKEYS_KEY).await?;
    match parse_hotkeys(raw.as_deref()) {
        Ok(map) => Ok(map),
        Err(e) => {
            eprintln!(
                "[keymap] load_persist_hotkeys: parse 失败（{}），使用默认映射",
                e
            );
            Ok(HotkeyMap::default_map())
        }
    }
}

/// 把 HotkeyMap 持久化到 `settings` 表 `hotkeys` 行。
pub async fn persist_hotkeys(storage: &Storage, map: &HotkeyMap) -> SqliteResult<()> {
    let json = serialize(map);
    storage.set_setting(HOTKEYS_KEY, &json).await
}

/// 运行时应用快捷键映射：更新共享 slot（单一事实来源）。同步、无 I/O。
pub fn apply_hotkeys(slot: &RwLock<HotkeyMap>, map: &HotkeyMap) {
    // 从来不失败，所以不用 Result
    *slot.write().unwrap() = map.clone();
}

/// 全局热键 re-register：清空旧注册 → 对每个 HotkeyAction::OpenPanel 映射重新注册。
///
/// 成功：全部注册成功 → `Ok(())`
/// 失败：任一失败 → `unregister_all()` 清理已注册项 → 返回 `Err(ConflictReason)`
///
/// 平台翻译在 `crate::hotkey::register` 内部完成；本函数只负责串起调用链。
pub fn apply_global_hotkey(
    app: &tauri::AppHandle,
    map: &HotkeyMap,
) -> Result<(), crate::hotkey::ConflictReason> {
    use clipvault_core::keymap::HotkeyAction;

    crate::hotkey::unregister_all();

    // 目前只有 OpenPanel 是全局热键；其余 action 在面板内通过 WebView 处理。
    if let Some(combo) = map.actions.get(&HotkeyAction::OpenPanel) {
        crate::hotkey::register(app, HotkeyAction::OpenPanel, combo)?;
    }

    Ok(())
}

/// 写路径：持久化 → 更新共享 slot（即时生效）。
///
/// 注意：U1 阶段 `apply_hotkey_to_global` 仅更新 in-memory slot，
/// live re-register 步骤在 U2 的 `apply_global_hotkey` 中补全。
pub async fn set_hotkey(
    storage: &Storage,
    slot: &RwLock<HotkeyMap>,
    map: &HotkeyMap,
) -> SqliteResult<()> {
    persist_hotkeys(storage, map).await?;
    apply_hotkeys(slot, map);
    Ok(())
}

/// 启动路径：从 `settings` 表加载 HotkeyMap → 写入共享 slot → 返回该 map。
///
/// 在 spawn watcher 前调用，与 `boot_load_panel_opacity` 并列。
pub async fn boot_load_hotkeys(
    storage: &Storage,
    slot: &RwLock<HotkeyMap>,
) -> SqliteResult<HotkeyMap> {
    let map = load_persist_hotkeys(storage).await?;
    apply_hotkeys(slot, &map);
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_helpers::open_test_db;

    /// 1. DB 中无 hotkeys 行 → 返回 default()，DB 保持无该行
    #[tokio::test]
    async fn load_none_returns_default() {
        let db = open_test_db();
        let map = load_persist_hotkeys(&db).await.expect("load must succeed");
        assert_eq!(map.actions.len(), 13, "无 row时应返回 13 条默认映射");
        // DB 中应仍无该行
        let raw = db
            .get_setting(HOTKEYS_KEY)
            .await
            .expect("get_setting must succeed");
        assert!(raw.is_none(), "无 row 时不应创建该行");
    }

    /// 2. persist → load 往返
    #[tokio::test]
    async fn persist_then_load_roundtrip() {
        let db = open_test_db();
        // HotkeyMap::default() 是派生 Default（空 HashMap）；
        // round-trip 后 parse 以 default_map()（13 条）填充，所以 expected 用 default_map()。
        let full_default = HotkeyMap::default_map();
        persist_hotkeys(&db, &full_default)
            .await
            .expect("persist must succeed");
        let loaded = load_persist_hotkeys(&db).await.expect("load must succeed");
        assert_eq!(loaded, full_default, "persist → load 往返不丢数据");
    }

    /// 3. 手动写入损坏 JSON → load 返回 default()，不重写库
    #[tokio::test]
    async fn corrupted_json_returns_default_without_rewrite() {
        let db = open_test_db();

        // 手动写入损坏的 JSON
        db.set_setting(HOTKEYS_KEY, "not valid json at all")
            .await
            .expect("set_setting must succeed");

        let map = load_persist_hotkeys(&db).await.expect("load must succeed");
        assert_eq!(map.actions.len(), 13, "损坏 JSON 应回落到默认映射");

        // 确认库中仍然是损坏的 JSON（没有重写）
        let raw = db
            .get_setting(HOTKEYS_KEY)
            .await
            .expect("get_setting must succeed");
        assert_eq!(raw.as_deref(), Some("not valid json at all"));
    }
}
