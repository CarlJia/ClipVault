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
            ::log::warn!(
                target: "keymap",
                "load_persist_hotkeys: parse 失败（{}），使用默认映射",
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

/// 把 `HotkeyMap` 中的全局热键注册到操作系统。
///
/// **替换语义**（U2 code review P0 修复）：不再「先 `unregister_all` 再注册」。
/// `crate::hotkey::register` 本身就是按 action 替换 —— 先注册新 combo，
/// 成功后才注销同一 action 的旧 combo。因此：
///
/// - 注册成功：新 combo 生效，旧 combo 已注销，中间不存在「无热键」窗口。
/// - 注册失败（冲突 / 不支持的键）：旧 combo 原样保留，用户不会掉进
///   「一次冲突后永远没有全局热键」的状态，且再次启动时仍从库里的旧值恢复。
///
/// 平台翻译在 `crate::hotkey::register` 内部完成；本函数只串调用链。
pub fn apply_global_hotkey(
    app: &tauri::AppHandle,
    map: &HotkeyMap,
) -> Result<(), crate::hotkey::ConflictReason> {
    use clipvault_core::keymap::HotkeyAction;

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

/// 解析 `set_hotkey` IPC 的入参：action 字符串 + JSON combo → `(action, combo)`。
///
/// 抽成纯函数是为了可单测 —— 命令体本身需要 `State<'_, AppState>`，
/// 无法在单测里构造（U2 code review：5 条错误返回路径此前无覆盖）。
///
/// 错误字符串与前端 toast 文案对齐（英文，命令行式）。
pub fn parse_set_hotkey_args(
    action: &str,
    combo: &serde_json::Value,
) -> Result<
    (
        clipvault_core::keymap::HotkeyAction,
        clipvault_core::keymap::KeyCombo,
    ),
    String,
> {
    use clipvault_core::keymap::{CharKind, HotkeyAction, KeyCombo, Mods};

    // 1. action string → enum
    let action =
        HotkeyAction::from_json_str(action).ok_or_else(|| format!("unknown action '{action}'"))?;

    // 2. combo 必须是对象
    let combo_obj = combo.as_object().ok_or("combo must be a JSON object")?;

    // 3. mods：字符串数组，逐项映射到 Mods 位
    let mods_array = combo_obj
        .get("mods")
        .and_then(|v| v.as_array())
        .ok_or("combo.mods must be an array")?;
    let mut mods = Mods::empty();
    for m in mods_array {
        let s = m.as_str().ok_or("mods elements must be strings")?;
        match s {
            "meta" => mods |= Mods::META,
            "shift" => mods |= Mods::SHIFT,
            "alt" => mods |= Mods::ALT,
            "ctrl" => mods |= Mods::CTRL,
            _ => return Err(format!("unknown modifier '{s}'")),
        }
    }

    // 4. key：必填、非 null（modifier-only 拒绝）、可识别
    let key_value = combo_obj.get("key").ok_or("combo.key is required")?;
    if key_value.is_null() {
        return Err("modifier-only combos are not allowed".to_string());
    }
    let key_str = key_value.as_str().ok_or("combo.key must be a string")?;
    let key =
        CharKind::from_json_str(key_str).ok_or_else(|| format!("unrecognised key '{key_str}'"))?;

    Ok((action, KeyCombo { mods, key }))
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
        assert_eq!(map, HotkeyMap::default_map(), "无 row 时应返回完整默认映射");
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
        // round-trip 后 parse 以 default_map()（14 条）填充，所以 expected 用 default_map()。
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
        assert_eq!(
            map,
            HotkeyMap::default_map(),
            "损坏 JSON 应回落到完整默认映射"
        );

        // 确认库中仍然是损坏的 JSON（没有重写）
        let raw = db
            .get_setting(HOTKEYS_KEY)
            .await
            .expect("get_setting must succeed");
        assert_eq!(raw.as_deref(), Some("not valid json at all"));
    }

    // ----------------------------------------------------------------
    // parse_set_hotkey_args：set_hotkey IPC 的入参解析（5 条错误路径）
    // ----------------------------------------------------------------

    fn combo_json(mods: &[&str], key: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "mods": mods, "key": key })
    }

    /// happy path：合法 action + 合法 combo → Ok。
    #[test]
    fn parse_args_accepts_valid_input() {
        use clipvault_core::keymap::{CharKind, HotkeyAction, Mods};
        let (action, combo) =
            parse_set_hotkey_args("open_panel", &combo_json(&["meta", "alt"], "v".into()))
                .expect("valid input must parse");
        assert_eq!(action, HotkeyAction::OpenPanel);
        assert_eq!(combo.key, CharKind::Char('v'));
        assert!(combo.mods.contains(Mods::META) && combo.mods.contains(Mods::ALT));
    }

    /// 绑定方向键（无修饰键）也应可解析。
    #[test]
    fn parse_args_accepts_named_keys() {
        use clipvault_core::keymap::{CharKind, HotkeyAction};
        for (json_key, expected) in [
            ("enter", CharKind::Enter),
            ("escape", CharKind::Escape),
            ("arrow_up", CharKind::ArrowUp),
            ("arrow_down", CharKind::ArrowDown),
            ("comma", CharKind::Comma),
            ("backspace", CharKind::Backspace),
        ] {
            let (action, combo) =
                parse_set_hotkey_args("move_up", &combo_json(&[], json_key.into()))
                    .unwrap_or_else(|e| panic!("'{json_key}' must parse, got: {e}"));
            assert_eq!(action, HotkeyAction::MoveUp);
            assert_eq!(combo.key, expected, "'{json_key}' 应翻译为 {expected:?}");
        }
    }

    /// 错误路径 1：未知 action。
    #[test]
    fn parse_args_rejects_unknown_action() {
        let err = parse_set_hotkey_args("not_an_action", &combo_json(&[], "v".into()))
            .expect_err("unknown action must fail");
        assert!(err.contains("unknown action"), "got: {err}");
    }

    /// 错误路径 2：combo 不是对象。
    #[test]
    fn parse_args_rejects_non_object_combo() {
        let err = parse_set_hotkey_args("open_panel", &serde_json::json!("v"))
            .expect_err("non-object combo must fail");
        assert!(err.contains("must be a JSON object"), "got: {err}");
    }

    /// 错误路径 3：mods 不是数组。
    #[test]
    fn parse_args_rejects_mods_not_array() {
        let err = parse_set_hotkey_args(
            "open_panel",
            &serde_json::json!({ "mods": "meta", "key": "v" }),
        )
        .expect_err("non-array mods must fail");
        assert!(err.contains("must be an array"), "got: {err}");
    }

    /// 错误路径 4：未知修饰键字符串。
    #[test]
    fn parse_args_rejects_unknown_modifier() {
        let err = parse_set_hotkey_args("open_panel", &combo_json(&["hyper"], "v".into()))
            .expect_err("unknown modifier must fail");
        assert!(err.contains("unknown modifier"), "got: {err}");
    }

    /// 错误路径 5a：key 缺失。
    #[test]
    fn parse_args_rejects_missing_key() {
        let err = parse_set_hotkey_args("open_panel", &serde_json::json!({ "mods": [] }))
            .expect_err("missing key must fail");
        assert!(err.contains("combo.key is required"), "got: {err}");
    }

    /// 错误路径 5b：key 为 null（modifier-only）。
    #[test]
    fn parse_args_rejects_modifier_only_combo() {
        use serde_json::Value;
        let err = parse_set_hotkey_args("open_panel", &combo_json(&["meta"], Value::Null))
            .expect_err("modifier-only must fail");
        assert!(err.contains("modifier-only"), "got: {err}");
    }

    /// 错误路径 5c：key 不可识别。
    #[test]
    fn parse_args_rejects_unrecognised_key() {
        let err = parse_set_hotkey_args("open_panel", &combo_json(&[], "f13".into()))
            .expect_err("unrecognised key must fail");
        assert!(err.contains("unrecognised key"), "got: {err}");
    }

    /// 错误路径 5d：key 不是字符串。
    #[test]
    fn parse_args_rejects_non_string_key() {
        let err = parse_set_hotkey_args("open_panel", &combo_json(&[], 42.into()))
            .expect_err("non-string key must fail");
        assert!(err.contains("must be a string"), "got: {err}");
    }
}
