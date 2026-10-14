//! U1 确定性测试：HotkeyMap 序列化 / 反序列化 / 往返 / 边界情况。
//!
//! 纯函数测试，不触碰 Tauri / 真实存储 / 前端。

use clipvault_core::keymap::{HotkeyAction, HotkeyMap, KeyCombo, Mods, CharKind};

// ---------------------------------------------------------------------------
// Helper：把 KeyCombo 构造得简单一点
// ---------------------------------------------------------------------------
fn combo(mods: Mods, key: char) -> KeyCombo {
    KeyCombo { mods, key: CharKind::Char(key.to_ascii_lowercase()) }
}

fn empty_combo(key: CharKind) -> KeyCombo {
    KeyCombo { mods: Mods::empty(), key }
}

// ---------------------------------------------------------------------------
// HotkeyMap::default() 与 13 个硬编码条目 1:1 对应
// ---------------------------------------------------------------------------

fn default_map() -> HotkeyMap {
    HotkeyMap::default_map()
}

#[test]
fn default_has_13_entries() {
    let map = default_map();
    assert_eq!(map.actions.len(), 13, "HotkeyMap::default() 必须有 13 个条目");
}

#[test]
fn default_open_panel() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::OpenPanel),
        Some(&combo(Mods::META | Mods::ALT, 'v')),
        "OpenPanel 必须是 Meta+Alt+V"
    );
}

#[test]
fn default_paste() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::Paste),
        Some(&empty_combo(CharKind::Enter)),
        "Paste 必须是 Enter"
    );
}

#[test]
fn default_edit_note() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::EditNote),
        Some(&combo(Mods::META, 'e')),
        "EditNote 必须是 Meta+E"
    );
}

#[test]
fn default_toggle_pin() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::TogglePin),
        Some(&combo(Mods::META, 'p')),
        "TogglePin 必须是 Meta+P"
    );
}

#[test]
fn default_move_to_vault() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::MoveToVault),
        Some(&combo(Mods::META, 'l')),
        "MoveToVault 必须是 Meta+L"
    );
}

#[test]
fn default_open_url() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::OpenUrl),
        Some(&combo(Mods::META, 'o')),
        "OpenUrl 必须是 Meta+O"
    );
}

#[test]
fn default_open_vault() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::OpenVault),
        Some(&combo(Mods::META | Mods::SHIFT, 'v')),
        "OpenVault 必须是 Meta+Shift+V"
    );
}

#[test]
fn default_toggle_guest_mode() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::ToggleGuestMode),
        Some(&combo(Mods::META | Mods::SHIFT, 'g')),
        "ToggleGuestMode 必须是 Meta+Shift+G"
    );
}

#[test]
fn default_skip_once() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::SkipOnce),
        Some(&combo(Mods::META | Mods::SHIFT, 'n')),
        "SkipOnce 必须是 Meta+Shift+N"
    );
}

#[test]
fn default_open_settings() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::OpenSettings),
        Some(&combo(Mods::META, ',')),
        "OpenSettings 必须是 Meta+,"
    );
}

#[test]
fn default_close() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::Close),
        Some(&empty_combo(CharKind::Escape)),
        "Close 必须是 Escape"
    );
}

#[test]
fn default_move_up() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::MoveUp),
        Some(&empty_combo(CharKind::ArrowUp)),
        "MoveUp 必须是 ArrowUp"
    );
}

#[test]
fn default_move_down() {
    let map = default_map();
    assert_eq!(
        map.actions.get(&HotkeyAction::MoveDown),
        Some(&empty_combo(CharKind::ArrowDown)),
        "MoveDown 必须是 ArrowDown"
    );
}

// ---------------------------------------------------------------------------
// serialize(default()) 输出符合预期 schema（含 version:1 与 13 个 action）
// ---------------------------------------------------------------------------

#[test]
fn serialize_default_has_version_1() {
    let json = clipvault_core::keymap::serialize(&HotkeyMap::default());
    let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
    assert_eq!(value.get("version").and_then(|v| v.as_i64()), Some(1), "version 必须为 1");
}

#[test]
fn serialize_default_has_13_actions() {
    let json = clipvault_core::keymap::serialize(&HotkeyMap::default());
    let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
    let actions = value.get("actions").and_then(|v| v.as_object()).expect("actions 必须是 object");
    assert_eq!(actions.len(), 13, "actions 必须有 13 个条目");
}

#[test]
fn serialize_default_open_panel_matches_schema() {
    let json = clipvault_core::keymap::serialize(&HotkeyMap::default());
    let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
    let open_panel = &value["actions"]["open_panel"];
    assert_eq!(open_panel["mods"].as_array().expect("mods must be array"), &["meta", "alt"]);
    assert_eq!(open_panel["key"].as_str().expect("key must be string"), "v");
}

// ---------------------------------------------------------------------------
// parse(serialize(default())) == Ok(default())（往返不丢字段）
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_idempotent() {
    let original = HotkeyMap::default();
    let json = clipvault_core::keymap::serialize(&original);
    let parsed = clipvault_core::keymap::parse(&json).expect("往返 parse 不应失败");
    assert_eq!(parsed.actions.len(), original.actions.len());
    for action in [
        HotkeyAction::OpenPanel,
        HotkeyAction::Paste,
        HotkeyAction::EditNote,
        HotkeyAction::TogglePin,
        HotkeyAction::MoveToVault,
        HotkeyAction::OpenUrl,
        HotkeyAction::OpenVault,
        HotkeyAction::ToggleGuestMode,
        HotkeyAction::SkipOnce,
        HotkeyAction::OpenSettings,
        HotkeyAction::Close,
        HotkeyAction::MoveUp,
        HotkeyAction::MoveDown,
    ] {
        assert_eq!(
            parsed.actions.get(&action),
            original.actions.get(&action),
            "往返后 {action:?} 的 KeyCombo 必须一致"
        );
    }
}

// ---------------------------------------------------------------------------
// parse(invalid_json) → Err
// ---------------------------------------------------------------------------

#[test]
fn parse_invalid_json_err() {
    let result = clipvault_core::keymap::parse(r#"not json at all"#);
    assert!(result.is_err(), "非法 JSON 必须返回 Err");
}

#[test]
fn parse_truncated_json_err() {
    let result = clipvault_core::keymap::parse(r#"{"version":1,"actions":{"open_panel":{"#);
    assert!(result.is_err(), "截断 JSON 必须返回 Err");
}

// ---------------------------------------------------------------------------
// parse(missing_actions_field) → Err
// ---------------------------------------------------------------------------

#[test]
fn parse_missing_actions_field_err() {
    let result = clipvault_core::keymap::parse(r#"{"version":1}"#);
    assert!(result.is_err(), "缺少 actions 字段必须返回 Err");
}

// ---------------------------------------------------------------------------
// parse(unknown_action) 不报 Err，未识别 action 丢弃（前向兼容）
// ---------------------------------------------------------------------------

#[test]
fn parse_unknown_action_dropped() {
    let json = r#"{
        "version": 1,
        "actions": {
            "open_panel": {"mods": ["meta", "alt"], "key": "v"},
            "future_action": {"mods": ["ctrl"], "key": "x"}
        }
    }"#;
    let result = clipvault_core::keymap::parse(json).expect("未知 action 不应导致 parse 失败");
    assert!(result.actions.get(&HotkeyAction::OpenPanel).is_some());
    assert!(result.actions.get(&HotkeyAction::Paste).is_none()); // 默认不补
    // future_action 不在 enum 里，已丢弃
    assert_eq!(result.actions.len(), 1, "只有已知 action 被保留");
}

// ---------------------------------------------------------------------------
// parse(version=999) → Err
// ---------------------------------------------------------------------------

#[test]
fn parse_unknown_version_err() {
    let json = r#"{"version":999,"actions":{"open_panel":{"mods":["meta","alt"],"key":"v"}}}"#;
    let result = clipvault_core::keymap::parse(json);
    assert!(result.is_err(), "未知 version 必须返回 Err");
}

// ---------------------------------------------------------------------------
// parse(modifier_only) (mods 非空，但 key 为 null) → Err
// ---------------------------------------------------------------------------

#[test]
fn parse_modifier_only_err() {
    let json = r#"{
        "version": 1,
        "actions": {
            "open_panel": {"mods": ["meta", "alt"], "key": null}
        }
    }"#;
    let result = clipvault_core::keymap::parse(json);
    assert!(result.is_err(), "modifier-only 组合（key=null）必须返回 Err");
}

// ---------------------------------------------------------------------------
// parse(partial_missing_field)：已知 action 保留值，未知 action 丢弃
// ---------------------------------------------------------------------------

#[test]
fn parse_partial_unknown_action_dropped() {
    // open_panel 是已知，future_action 未知
    let json = r#"{
        "version": 1,
        "actions": {
            "open_panel": {"mods": ["meta"], "key": "x"},
            "future_action": {"mods": ["ctrl"], "key": "y"}
        }
    }"#;
    let result = clipvault_core::keymap::parse(json).expect("parse 不应失败");
    assert_eq!(
        result.actions.get(&HotkeyAction::OpenPanel),
        Some(&combo(Mods::META, 'x')),
        "已知 action 保留"
    );
}

// ---------------------------------------------------------------------------
// Mods::contains / is_subset / is_disjoint / is_empty
// ---------------------------------------------------------------------------

#[test]
fn mods_contains() {
    let mods = Mods::META | Mods::ALT;
    assert!(mods.contains(Mods::META));
    assert!(mods.contains(Mods::ALT));
    assert!(!mods.contains(Mods::CTRL));
}

#[test]
fn mods_is_subset() {
    let superset = Mods::META | Mods::ALT | Mods::SHIFT;
    let subset = Mods::META | Mods::ALT;
    assert!(subset.is_subset(superset));
    assert!(!superset.is_subset(subset));
}

#[test]
fn mods_is_disjoint() {
    let a = Mods::META | Mods::ALT;
    let b = Mods::CTRL | Mods::SHIFT;
    let c = Mods::META | Mods::CTRL;
    assert!(a.is_disjoint(b));
    assert!(!a.is_disjoint(c));
}

#[test]
fn mods_is_empty() {
    assert!(Mods::empty().is_empty());
    assert!(!Mods::META.is_empty());
    assert!(!(Mods::META | Mods::ALT).is_empty());
}
