//! HotkeyMap 数据层：平台无关的快捷键抽象。
//!
//! 数据结构：
//! - [`HotkeyAction`] — 13 个语义化动作标识符（与前端 dispatch 入口 1:1 对应）。
//! - [`CharKind`] — 键值类别（字母/数字/符号、Enter、Escape、方向键、逗号）。
//! - [`KeyCombo`] — 一个具体的按键组合 `{ mods, key }`。
//! - [`HotkeyMap`] — Action → KeyCombo 的全量映射表（启动时从 settings 表加载，
//!   运行时存于 `Arc<RwLock<HotkeyMap>>`）。
//!
//! JSON schema（version = 1）：
//! ```json
//! {
//!   "version": 1,
//!   "actions": {
//!     "open_panel": {"mods": ["meta","alt"], "key": "v"},
//!     ...
//!   }
//! }
//! ```
//!
//! Modifier 名称（JSON 层）：`"meta"` / `"ctrl"` / `"alt"` / `"shift"`（小写）。
//!
//! ## Bit flag 注
//!
//! `Mods` 的位布局为：`Meta=0x01 / Shift=0x02 / Alt=0x04 / Ctrl=0x08`。
//! 这与 macOS Carbon 的 `cmdKey=0x0100 / shiftKey=0x0200 / optionKey=0x0800 / controlKey=0x1000`
//! **不是同一个布局**。平台翻译层（U2 `hotkey.rs`）必须在 translate 时显式映射，
//! 不得混用两侧的常量。

/// 修饰键位标志。
///
/// 位布局：Meta=0x01 / Shift=0x02 / Alt=0x04 / Ctrl=0x08。
/// 与 Carbon 位常量（cmdKey=0x0100 等）不对齐；平台翻译在 U2 `hotkey.rs` 中显式完成。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Mods(u8);

impl Mods {
    pub const META: Mods = Mods(0b0000_0001);
    pub const SHIFT: Mods = Mods(0b0000_0010);
    pub const ALT: Mods = Mods(0b0000_0100);
    pub const CTRL: Mods = Mods(0b0000_1000);

    #[inline]
    pub fn empty() -> Mods {
        Mods(0)
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn contains(self, flag: Mods) -> bool {
        (self.0 & flag.0) == flag.0
    }

    /// self 是否为 superset 的子集（即 self 的所有位 superset 也都有）
    #[inline]
    pub fn is_subset(self, superset: Mods) -> bool {
        (self.0 & !superset.0) == 0
    }

    /// 两个 Mods 是否有交集
    #[inline]
    pub fn is_disjoint(self, other: Mods) -> bool {
        (self.0 & other.0) == 0
    }

    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    #[inline]
    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for Mods {
    type Output = Mods;
    #[inline]
    fn bitand(self, rhs: Mods) -> Mods {
        Mods(self.0 & rhs.0)
    }
}

impl std::ops::BitXor for Mods {
    type Output = Mods;
    #[inline]
    fn bitxor(self, rhs: Mods) -> Mods {
        Mods(self.0 ^ rhs.0)
    }
}

impl std::ops::Not for Mods {
    type Output = Mods;
    #[inline]
    fn not(self) -> Mods {
        Mods(!self.0 & 0b0000_1111)
    }
}

impl std::ops::BitOrAssign for Mods {
    #[inline]
    fn bitor_assign(&mut self, rhs: Mods) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAndAssign for Mods {
    #[inline]
    fn bitand_assign(&mut self, rhs: Mods) {
        self.0 &= rhs.0;
    }
}

/// 键值类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CharKind {
    /// 单字符键（字母、数字、常用符号）。
    Char(char),
    Enter,
    Escape,
    ArrowUp,
    ArrowDown,
    Comma,
}

impl Default for CharKind {
    fn default() -> Self {
        CharKind::Char(' ')
    }
}

impl CharKind {
    /// 将 JSON key string 转换为 CharKind。
    pub fn from_json_str(s: &str) -> Option<CharKind> {
        match s {
            "enter" => Some(CharKind::Enter),
            "escape" => Some(CharKind::Escape),
            "arrow_up" => Some(CharKind::ArrowUp),
            "arrow_down" => Some(CharKind::ArrowDown),
            "comma" => Some(CharKind::Comma),
            s if s.len() == 1 => Some(CharKind::Char(
                s.chars().next().unwrap().to_ascii_lowercase(),
            )),
            _ => None,
        }
    }
}

/// 一个具体的按键组合。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct KeyCombo {
    pub mods: Mods,
    pub key: CharKind,
}

/// 13 个语义化动作标识符。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HotkeyAction {
    OpenPanel,
    Paste,
    EditNote,
    TogglePin,
    MoveToVault,
    OpenUrl,
    OpenVault,
    ToggleGuestMode,
    SkipOnce,
    OpenSettings,
    Close,
    MoveUp,
    MoveDown,
}

impl HotkeyAction {
    /// 将 JSON key string 转换为 enum（前向兼容：未知 key 返回 None）。
    pub fn from_json_str(s: &str) -> Option<HotkeyAction> {
        match s {
            "open_panel" => Some(HotkeyAction::OpenPanel),
            "paste" => Some(HotkeyAction::Paste),
            "edit_note" => Some(HotkeyAction::EditNote),
            "toggle_pin" => Some(HotkeyAction::TogglePin),
            "move_to_vault" => Some(HotkeyAction::MoveToVault),
            "open_url" => Some(HotkeyAction::OpenUrl),
            "open_vault" => Some(HotkeyAction::OpenVault),
            "toggle_guest_mode" => Some(HotkeyAction::ToggleGuestMode),
            "skip_once" => Some(HotkeyAction::SkipOnce),
            "open_settings" => Some(HotkeyAction::OpenSettings),
            "close" => Some(HotkeyAction::Close),
            "move_up" => Some(HotkeyAction::MoveUp),
            "move_down" => Some(HotkeyAction::MoveDown),
            _ => None,
        }
    }

    /// action → 线格式键名（[`HotkeyAction::from_json_str`] 的逆）。
    ///
    /// 公开是为了让写入口的错误信息能指名道姓（`app/src/keymap.rs` 的
    /// `combo_conflict_error`）—— core 里没有中文标签表，也不该为一条兜底提示复制一份。
    pub fn json_key(&self) -> &'static str {
        match self {
            HotkeyAction::OpenPanel => "open_panel",
            HotkeyAction::Paste => "paste",
            HotkeyAction::EditNote => "edit_note",
            HotkeyAction::TogglePin => "toggle_pin",
            HotkeyAction::MoveToVault => "move_to_vault",
            HotkeyAction::OpenUrl => "open_url",
            HotkeyAction::OpenVault => "open_vault",
            HotkeyAction::ToggleGuestMode => "toggle_guest_mode",
            HotkeyAction::SkipOnce => "skip_once",
            HotkeyAction::OpenSettings => "open_settings",
            HotkeyAction::Close => "close",
            HotkeyAction::MoveUp => "move_up",
            HotkeyAction::MoveDown => "move_down",
        }
    }
}

/// Action → KeyCombo 全量映射表。
///
/// **不** 派生 `Default` —— `HashMap` 的派生 Default 产生空表，而本类型的"默认"
/// 含义是 [`default_map`] 提供的 13 条硬编码条目。`parse` 路径从 default_map 出发、
/// 缺失字段保留默认值；如果本类型有 `Default::default()`，调用方很容易拿到空表。
/// 调用方请使用 [`HotkeyMap::default_map`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotkeyMap {
    pub actions: std::collections::HashMap<HotkeyAction, KeyCombo>,
}

impl HotkeyMap {
    /// 构建包含 13 个默认条目的 HotkeyMap（与 main.ts:649-778 硬编码 1:1 对应）。
    pub fn default_map() -> HotkeyMap {
        let mut actions = std::collections::HashMap::new();
        actions.insert(
            HotkeyAction::OpenPanel,
            KeyCombo {
                mods: Mods::META | Mods::ALT,
                key: CharKind::Char('v'),
            },
        );
        actions.insert(
            HotkeyAction::Paste,
            KeyCombo {
                mods: Mods::empty(),
                key: CharKind::Enter,
            },
        );
        actions.insert(
            HotkeyAction::EditNote,
            KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('e'),
            },
        );
        actions.insert(
            HotkeyAction::TogglePin,
            KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('p'),
            },
        );
        actions.insert(
            HotkeyAction::MoveToVault,
            KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('l'),
            },
        );
        actions.insert(
            HotkeyAction::OpenUrl,
            KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('o'),
            },
        );
        actions.insert(
            HotkeyAction::OpenVault,
            KeyCombo {
                mods: Mods::META | Mods::SHIFT,
                key: CharKind::Char('v'),
            },
        );
        actions.insert(
            HotkeyAction::ToggleGuestMode,
            KeyCombo {
                mods: Mods::META | Mods::SHIFT,
                key: CharKind::Char('g'),
            },
        );
        actions.insert(
            HotkeyAction::SkipOnce,
            KeyCombo {
                mods: Mods::META | Mods::SHIFT,
                key: CharKind::Char('n'),
            },
        );
        actions.insert(
            HotkeyAction::OpenSettings,
            KeyCombo {
                mods: Mods::META,
                key: CharKind::Comma,
            },
        );
        actions.insert(
            HotkeyAction::Close,
            KeyCombo {
                mods: Mods::empty(),
                key: CharKind::Escape,
            },
        );
        actions.insert(
            HotkeyAction::MoveUp,
            KeyCombo {
                mods: Mods::empty(),
                key: CharKind::ArrowUp,
            },
        );
        actions.insert(
            HotkeyAction::MoveDown,
            KeyCombo {
                mods: Mods::empty(),
                key: CharKind::ArrowDown,
            },
        );
        HotkeyMap { actions }
    }

    /// 除 `action` 之外，表里有没有别的 action 已经占着 `combo`。
    ///
    /// 组合键在表里必须唯一：两条 action 共用同一个组合时，前端 `matches()` 只会派发给
    /// [`serialize`] 排序（`BTreeMap`，按 action 名）靠前的那条，另一条从此永远不触发
    /// —— 而设置里那一行仍然显示着这个组合。写入口（`set_hotkey`）据此拒绝重绑。
    ///
    /// 返回**排序靠前**的那一条，也就是前端真正会拿到派发的那条；多条同时占用时结果
    /// 因此是确定的（`HashMap` 的迭代顺序不承诺稳定，不能直接 `find`）。
    ///
    /// 注意：只排除 `action` 自己，不做「全表查重」—— 库里可能已经存在历史重复（这个
    /// 校验出现之前写进去的），全表查重会让用户连改别的快捷键都做不了。
    pub fn conflict_for(&self, action: HotkeyAction, combo: KeyCombo) -> Option<HotkeyAction> {
        let mut owners: Vec<HotkeyAction> = self
            .actions
            .iter()
            .filter(|(a, c)| **a != action && **c == combo)
            .map(|(a, _)| *a)
            .collect();
        owners.sort_by_key(|a| a.json_key());
        owners.first().copied()
    }
}

// ---------------------------------------------------------------------------
// JSON 序列化 / 反序列化
// ---------------------------------------------------------------------------

/// Serialize a HotkeyMap to a JSON string (version 1 schema).
pub fn serialize(map: &HotkeyMap) -> String {
    let mut entries = std::collections::BTreeMap::new(); // sorted for deterministic output
    for (action, combo) in &map.actions {
        let mods_list: Vec<&str> = {
            let mut m = Vec::new();
            if combo.mods.contains(Mods::META) {
                m.push("meta");
            }
            if combo.mods.contains(Mods::SHIFT) {
                m.push("shift");
            }
            if combo.mods.contains(Mods::ALT) {
                m.push("alt");
            }
            if combo.mods.contains(Mods::CTRL) {
                m.push("ctrl");
            }
            m
        };
        let key_value = match &combo.key {
            CharKind::Char(c) => serde_json::Value::String(c.to_ascii_lowercase().to_string()),
            CharKind::Enter => serde_json::Value::String("enter".to_string()),
            CharKind::Escape => serde_json::Value::String("escape".to_string()),
            CharKind::ArrowUp => serde_json::Value::String("arrow_up".to_string()),
            CharKind::ArrowDown => serde_json::Value::String("arrow_down".to_string()),
            CharKind::Comma => serde_json::Value::String("comma".to_string()),
        };
        entries.insert(
            action.json_key().to_string(),
            serde_json::json!({ "mods": mods_list, "key": key_value }),
        );
    }
    serde_json::json!({ "version": 1, "actions": entries }).to_string()
}

/// Parse a JSON string into a HotkeyMap.
/// Returns Err with message on invalid JSON, missing/wrong-type fields, unknown version,
/// modifier-only combos (key is null), or unknown actions (unknown actions are dropped silently).
pub fn parse(json: &str) -> Result<HotkeyMap, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("JSON parse error: {e}"))?;

    let version = value
        .get("version")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| "missing or non-integer 'version'".to_string())?;

    if version != 1 {
        return Err(format!("unsupported schema version {version}"));
    }

    let actions_obj = value
        .get("actions")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "missing or non-object 'actions'".to_string())?;

    let mut map = HotkeyMap::default_map(); // start with defaults

    for (key_str, entry) in actions_obj {
        let Some(action) = HotkeyAction::from_json_str(key_str) else {
            // Unknown action — drop silently (forward compatibility)
            continue;
        };

        let entry_obj = entry
            .as_object()
            .ok_or_else(|| format!("action '{key_str}': expected object, got {}", entry))?;

        let mods_array = entry_obj
            .get("mods")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("action '{key_str}': missing or non-array 'mods'"))?;

        let mut mods = Mods::empty();
        for m in mods_array {
            let s = m
                .as_str()
                .ok_or_else(|| format!("action '{key_str}': 'mods' elements must be strings"))?;
            match s {
                "meta" => mods |= Mods::META,
                "shift" => mods |= Mods::SHIFT,
                "alt" => mods |= Mods::ALT,
                "ctrl" => mods |= Mods::CTRL,
                _ => return Err(format!("action '{key_str}': unknown modifier '{s}'")),
            }
        }

        let key_value = entry_obj
            .get("key")
            .ok_or_else(|| format!("action '{key_str}': missing 'key' field"))?;

        // null key → modifier-only combo → invalid
        if key_value.is_null() {
            return Err(format!(
                "action '{key_str}': key must not be null (modifier-only combos are not allowed)"
            ));
        }

        let key_str2 = key_value
            .as_str()
            .ok_or_else(|| format!("action '{key_str}': 'key' must be a string"))?;

        let char_kind = CharKind::from_json_str(key_str2)
            .ok_or_else(|| format!("action '{key_str}': unrecognised key '{key_str2}'"))?;

        map.actions.insert(
            action,
            KeyCombo {
                mods,
                key: char_kind,
            },
        );
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charkind_from_str_valid() {
        assert_eq!(CharKind::from_json_str("enter"), Some(CharKind::Enter));
        assert_eq!(CharKind::from_json_str("escape"), Some(CharKind::Escape));
        assert_eq!(CharKind::from_json_str("arrow_up"), Some(CharKind::ArrowUp));
        assert_eq!(
            CharKind::from_json_str("arrow_down"),
            Some(CharKind::ArrowDown)
        );
        assert_eq!(CharKind::from_json_str("comma"), Some(CharKind::Comma));
        assert_eq!(CharKind::from_json_str("v"), Some(CharKind::Char('v')));
        assert_eq!(CharKind::from_json_str("V"), Some(CharKind::Char('v'))); // upper normalised
    }

    #[test]
    fn charkind_from_str_invalid() {
        assert_eq!(CharKind::from_json_str("enterx"), None);
        assert_eq!(CharKind::from_json_str(""), None);
        assert_eq!(CharKind::from_json_str("vv"), None);
    }

    #[test]
    fn hotkeyaction_roundtrip() {
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
            let key = action.json_key();
            assert_eq!(
                HotkeyAction::from_json_str(key),
                Some(action),
                "{key:?} roundtrip failed"
            );
        }
    }

    #[test]
    fn mods_json_roundtrip() {
        let json = serialize(&HotkeyMap::default_map());
        let parsed = parse(&json).expect("serialize then parse should succeed");
        assert_eq!(parsed, HotkeyMap::default_map());
    }

    #[test]
    fn mods_bit_operations() {
        let m = Mods::META | Mods::ALT;
        assert!(m.contains(Mods::META));
        assert!(m.contains(Mods::ALT));
        assert!(!m.contains(Mods::CTRL));

        let superset = Mods::META | Mods::ALT | Mods::SHIFT;
        let subset = Mods::META | Mods::ALT;
        assert!(subset.is_subset(superset));
        assert!(!superset.is_subset(subset));

        let a = Mods::META | Mods::ALT;
        let b = Mods::CTRL | Mods::SHIFT;
        assert!(a.is_disjoint(b));
        assert!(!a.is_disjoint(Mods::META));

        assert!(Mods::empty().is_empty());
        assert!(!Mods::META.is_empty());
    }

    // ----------------------------------------------------------------
    // conflict_for：表内组合键唯一性（写入口的冲突校验）
    // ----------------------------------------------------------------

    fn combo(mods: Mods, ch: char) -> KeyCombo {
        KeyCombo {
            mods,
            key: CharKind::Char(ch),
        }
    }

    /// 表里已经有人占着这个组合 → 返回占用者；占用的不是自己那份时也一样。
    #[test]
    fn conflict_for_finds_owner() {
        let map = HotkeyMap::default_map();
        // ⌘E 是 EditNote 的，Paste 想拿 → 报 EditNote。
        assert_eq!(
            map.conflict_for(HotkeyAction::Paste, combo(Mods::META, 'e')),
            Some(HotkeyAction::EditNote)
        );
        // 全局热键也在同一张表里：面板动作想拿 ⌥⌘V 一样会被查出。
        assert_eq!(
            map.conflict_for(HotkeyAction::Paste, combo(Mods::META | Mods::ALT, 'v')),
            Some(HotkeyAction::OpenPanel)
        );
    }

    /// 正在改绑的那条自己不算冲突，空闲组合也不算。
    #[test]
    fn conflict_for_excludes_self_and_free_combos() {
        let map = HotkeyMap::default_map();
        // EditNote 保留自己当前的 ⌘E → 放行。
        assert_eq!(
            map.conflict_for(HotkeyAction::EditNote, combo(Mods::META, 'e')),
            None
        );
        // 没人占的组合 → 放行（mods 或键只差一项都不算撞）。
        assert_eq!(
            map.conflict_for(HotkeyAction::Paste, combo(Mods::META, 'z')),
            None
        );
        assert_eq!(
            map.conflict_for(HotkeyAction::Paste, combo(Mods::SHIFT, 'e')),
            None
        );
    }

    /// 默认表本身必须无冲突：13 条默认绑定里没有任何两条共用同一个组合。
    #[test]
    fn default_map_has_no_duplicate_combo() {
        let map = HotkeyMap::default_map();
        for (action, bound) in &map.actions {
            assert_eq!(
                map.conflict_for(*action, *bound),
                None,
                "{action:?} 的默认组合被别的 action 占了"
            );
        }
    }

    /// 多条同时占用时返回**排序靠前**的那条 —— 前端 `matches()` 派发的正是它
    /// （`HashMap` 迭代顺序不承诺稳定，直接 `find` 会让错误信息飘）。
    #[test]
    fn conflict_for_is_deterministic_among_multiple_owners() {
        let mut map = HotkeyMap::default_map();
        let shared = combo(Mods::META, 'x');
        map.actions.insert(HotkeyAction::MoveDown, shared);
        map.actions.insert(HotkeyAction::EditNote, shared);
        // "edit_note" < "move_down" → 前者才是真会拿到派发的那条。
        assert_eq!(
            map.conflict_for(HotkeyAction::Paste, shared),
            Some(HotkeyAction::EditNote)
        );
    }
}
