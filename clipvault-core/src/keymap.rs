//! HotkeyMap 数据层：平台无关的快捷键抽象。
//!
//! 数据结构：
//! - [`HotkeyAction`] — 15 个语义化动作标识符（与前端 dispatch 入口 1:1 对应）。
//! - [`CharKind`] — 键值类别（字母/数字/符号、Enter、Escape、方向键、逗号、退格）。
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
    /// 退格键（`⌫`）。删除动作的默认主修饰键组合用它。
    Backspace,
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
            "backspace" => Some(CharKind::Backspace),
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

/// 15 个语义化动作标识符。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HotkeyAction {
    OpenPanel,
    Paste,
    EditNote,
    TogglePin,
    MoveToVault,
    DeleteEntry,
    OpenUrl,
    OpenVault,
    ToggleGuestMode,
    SkipOnce,
    OpenSettings,
    Close,
    MoveUp,
    MoveDown,
    /// 「切换预览栏」（列表旁预览栏 plan U5 / R8 / KTD8）。
    Preview,
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
            "delete_entry" => Some(HotkeyAction::DeleteEntry),
            "open_url" => Some(HotkeyAction::OpenUrl),
            "open_vault" => Some(HotkeyAction::OpenVault),
            "toggle_guest_mode" => Some(HotkeyAction::ToggleGuestMode),
            "skip_once" => Some(HotkeyAction::SkipOnce),
            "open_settings" => Some(HotkeyAction::OpenSettings),
            "close" => Some(HotkeyAction::Close),
            "move_up" => Some(HotkeyAction::MoveUp),
            "move_down" => Some(HotkeyAction::MoveDown),
            "preview" => Some(HotkeyAction::Preview),
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
            HotkeyAction::DeleteEntry => "delete_entry",
            HotkeyAction::OpenUrl => "open_url",
            HotkeyAction::OpenVault => "open_vault",
            HotkeyAction::ToggleGuestMode => "toggle_guest_mode",
            HotkeyAction::SkipOnce => "skip_once",
            HotkeyAction::OpenSettings => "open_settings",
            HotkeyAction::Close => "close",
            HotkeyAction::MoveUp => "move_up",
            HotkeyAction::MoveDown => "move_down",
            HotkeyAction::Preview => "preview",
        }
    }
}

/// Action → KeyCombo 全量映射表。
///
/// 每个 action 的值是 `Option<KeyCombo>` —— `None` 表示**未绑定**（这条 action 没有
/// 快捷键）。用户明确选「强行换绑」（[`HotkeyMap::claim`]）时，被抢走组合的那条会
/// 变成 `None`，而不是让两条 action 共用同一个组合。
///
/// **不** 派生 `Default` —— `HashMap` 的派生 Default 产生空表，而本类型的"默认"
/// 含义是 [`default_map`] 提供的 15 条硬编码条目（15 条都有键，没有 `None`）。
/// `parse` 路径从 default_map 出发、缺失字段保留默认值；如果本类型有
/// `Default::default()`，调用方很容易拿到空表。调用方请使用 [`HotkeyMap::default_map`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotkeyMap {
    pub actions: std::collections::HashMap<HotkeyAction, Option<KeyCombo>>,
}

impl HotkeyMap {
    /// 构建内置的默认热键映射（每个面板动作各一条）。
    pub fn default_map() -> HotkeyMap {
        let mut actions = std::collections::HashMap::new();
        actions.insert(
            HotkeyAction::OpenPanel,
            Some(KeyCombo {
                mods: Mods::META | Mods::ALT,
                key: CharKind::Char('v'),
            }),
        );
        actions.insert(
            HotkeyAction::Paste,
            Some(KeyCombo {
                mods: Mods::empty(),
                key: CharKind::Enter,
            }),
        );
        actions.insert(
            HotkeyAction::EditNote,
            Some(KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('e'),
            }),
        );
        actions.insert(
            HotkeyAction::TogglePin,
            Some(KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('p'),
            }),
        );
        actions.insert(
            HotkeyAction::MoveToVault,
            Some(KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('l'),
            }),
        );
        actions.insert(
            HotkeyAction::DeleteEntry,
            Some(KeyCombo {
                mods: Mods::META,
                key: CharKind::Backspace,
            }),
        );
        actions.insert(
            HotkeyAction::OpenUrl,
            Some(KeyCombo {
                mods: Mods::META,
                key: CharKind::Char('o'),
            }),
        );
        actions.insert(
            HotkeyAction::OpenVault,
            Some(KeyCombo {
                mods: Mods::META | Mods::SHIFT,
                key: CharKind::Char('v'),
            }),
        );
        actions.insert(
            HotkeyAction::ToggleGuestMode,
            Some(KeyCombo {
                mods: Mods::META | Mods::SHIFT,
                key: CharKind::Char('g'),
            }),
        );
        actions.insert(
            HotkeyAction::SkipOnce,
            Some(KeyCombo {
                mods: Mods::META | Mods::SHIFT,
                key: CharKind::Char('n'),
            }),
        );
        actions.insert(
            HotkeyAction::OpenSettings,
            Some(KeyCombo {
                mods: Mods::META,
                key: CharKind::Comma,
            }),
        );
        actions.insert(
            HotkeyAction::Close,
            Some(KeyCombo {
                mods: Mods::empty(),
                key: CharKind::Escape,
            }),
        );
        actions.insert(
            HotkeyAction::MoveUp,
            Some(KeyCombo {
                mods: Mods::empty(),
                key: CharKind::ArrowUp,
            }),
        );
        actions.insert(
            HotkeyAction::MoveDown,
            Some(KeyCombo {
                mods: Mods::empty(),
                key: CharKind::ArrowDown,
            }),
        );
        // 单按 Space（列表旁预览栏 plan KTD2）：MV 期承诺的「Space 预览完整内容」。
        // 与搜索框「打空格」的冲突由前端的焦点门解决（查询非空时让位给原生输入），
        // 不靠换一个带修饰键的组合来回避。
        actions.insert(
            HotkeyAction::Preview,
            Some(KeyCombo {
                mods: Mods::empty(),
                key: CharKind::Char(' '),
            }),
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
    /// 「未绑定」（`None`）的条目不算占用 —— 没有键就抢不走任何键。
    ///
    /// 注意：只排除 `action` 自己，不做「全表查重」—— 库里可能已经存在历史重复（这个
    /// 校验出现之前写进去的），全表查重会让用户连改别的快捷键都做不了。
    pub fn conflict_for(&self, action: HotkeyAction, combo: KeyCombo) -> Option<HotkeyAction> {
        let mut owners: Vec<HotkeyAction> = self
            .actions
            .iter()
            .filter(|(a, c)| **a != action && **c == Some(combo))
            .map(|(a, _)| *a)
            .collect();
        owners.sort_by_key(|a| a.json_key());
        owners.first().copied()
    }

    /// 强行换绑：把 `combo` 归给 `action`，并把表里所有正占着它的 action 解绑
    /// （写成 `None`，即「未绑定」）。
    ///
    /// [`conflict_for`] 只能拒绝；用户明确选「强行换绑」时走这里。组合键在表里仍然唯一，
    /// 只是原来占着它的那条没有键了（与「两条共用、靠前的那条生效」相反 —— 那种状态在
    /// 界面上看不出来，正是本次要关掉的那类静默失效）。
    ///
    /// 解绑**所有**占用者，而不是只解绑 [`conflict_for`] 报出来的那一条：库里可能留着
    /// 历史重复（这个校验之前写进去的），只解一条会把这个 1:1 不变量再留个尾巴。
    pub fn claim(&mut self, action: HotkeyAction, combo: KeyCombo) {
        for (other, bound) in self.actions.iter_mut() {
            if *other != action && *bound == Some(combo) {
                *bound = None;
            }
        }
        self.actions.insert(action, Some(combo));
    }
}

// ---------------------------------------------------------------------------
// JSON 序列化 / 反序列化
// ---------------------------------------------------------------------------

/// Serialize a HotkeyMap to a JSON string (version 1 schema).
///
/// 未绑定（`None`）的 action 写成 JSON `null`：`{"actions": {"paste": null, ...}}`。
/// 与 `{"mods": [], "key": null}` 是两种形状 —— 后者一直是非法 entry（modifier-only），
/// `parse` 仍然拒绝；前者是「这条没有快捷键」，`parse` 读成 `None`。
pub fn serialize(map: &HotkeyMap) -> String {
    let mut entries = std::collections::BTreeMap::new(); // sorted for deterministic output
    for (action, combo) in &map.actions {
        let entry = match combo {
            None => serde_json::Value::Null,
            Some(combo) => {
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
                    CharKind::Char(c) => {
                        serde_json::Value::String(c.to_ascii_lowercase().to_string())
                    }
                    CharKind::Enter => serde_json::Value::String("enter".to_string()),
                    CharKind::Escape => serde_json::Value::String("escape".to_string()),
                    CharKind::ArrowUp => serde_json::Value::String("arrow_up".to_string()),
                    CharKind::ArrowDown => serde_json::Value::String("arrow_down".to_string()),
                    CharKind::Comma => serde_json::Value::String("comma".to_string()),
                    CharKind::Backspace => serde_json::Value::String("backspace".to_string()),
                };
                serde_json::json!({ "mods": mods_list, "key": key_value })
            }
        };
        entries.insert(action.json_key().to_string(), entry);
    }
    serde_json::json!({ "version": 1, "actions": entries }).to_string()
}

/// Parse a JSON string into a HotkeyMap.
/// Returns Err with message on invalid JSON, missing/wrong-type fields, unknown version,
/// modifier-only combos (key is null), or unknown actions (unknown actions are dropped silently).
/// 某条 action 的值是 `null` → 该 action 未绑定（`None`），不是错误。
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

        // null → 未绑定（这条 action 没有快捷键）。注意与下面「entry 里的 key 为 null」
        // 区分：那种形状是非法 entry，仍然报错。
        if entry.is_null() {
            map.actions.insert(action, None);
            continue;
        }

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
            Some(KeyCombo {
                mods,
                key: char_kind,
            }),
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
        assert_eq!(
            CharKind::from_json_str("backspace"),
            Some(CharKind::Backspace)
        );
        assert_eq!(CharKind::from_json_str("v"), Some(CharKind::Char('v')));
        assert_eq!(CharKind::from_json_str("V"), Some(CharKind::Char('v'))); // upper normalised
    }

    #[test]
    fn charkind_from_str_invalid() {
        assert_eq!(CharKind::from_json_str("enterx"), None);
        assert_eq!(CharKind::from_json_str(""), None);
        assert_eq!(CharKind::from_json_str("vv"), None);
    }

    /// 全变体清单的唯一副本：`hotkeyaction_roundtrip` 用它做线格式往返，
    /// `default_map_covers_every_action` 用它当默认映射的基数锚点。
    /// 返回定长数组，所以新增一个变体时这里会编译失败 —— 这正是要的提醒。
    fn all_actions() -> [HotkeyAction; 15] {
        [
            HotkeyAction::OpenPanel,
            HotkeyAction::Paste,
            HotkeyAction::EditNote,
            HotkeyAction::TogglePin,
            HotkeyAction::MoveToVault,
            HotkeyAction::DeleteEntry,
            HotkeyAction::OpenUrl,
            HotkeyAction::OpenVault,
            HotkeyAction::ToggleGuestMode,
            HotkeyAction::SkipOnce,
            HotkeyAction::OpenSettings,
            HotkeyAction::Close,
            HotkeyAction::MoveUp,
            HotkeyAction::MoveDown,
            HotkeyAction::Preview,
        ]
    }

    #[test]
    fn hotkeyaction_roundtrip() {
        for action in all_actions() {
            let key = action.json_key();
            assert_eq!(
                HotkeyAction::from_json_str(key),
                Some(action),
                "{key:?} roundtrip failed"
            );
        }
    }

    /// 默认映射必须给每个动作都留一条。缺一条的后果是设置面板那一行显示 `—`、
    /// 键位静默失效，而 `default_map()` 与 load 回落路径的比较是同源的，抓不到。
    #[test]
    fn default_map_covers_every_action() {
        let map = HotkeyMap::default_map();
        for action in all_actions() {
            assert!(map.actions.contains_key(&action), "{action:?} 缺默认绑定");
        }
        assert_eq!(
            map.actions.len(),
            all_actions().len(),
            "默认映射出现了变体清单之外的条目"
        );
        assert_eq!(all_actions().len(), 15, "动作数必须是 15");
    }

    #[test]
    fn default_preview_is_space() {
        let map = HotkeyMap::default_map();
        assert_eq!(
            map.actions.get(&HotkeyAction::Preview),
            Some(&Some(KeyCombo {
                mods: Mods::empty(),
                key: CharKind::Char(' '),
            })),
            "Preview 必须是单按 Space"
        );
    }

    /// `delete_entry` 的默认绑定钉在 ⌘⌫（`Meta + Backspace`）—— 它是「删除选中条目」
    /// 的唯一默认入口，改坏前端就再没有办法删条目。
    #[test]
    fn default_delete_entry_is_meta_backspace() {
        let map = HotkeyMap::default_map();
        assert_eq!(
            map.actions.get(&HotkeyAction::DeleteEntry),
            Some(&Some(KeyCombo {
                mods: Mods::META,
                key: CharKind::Backspace,
            })),
            "DeleteEntry 必须是 Meta+Backspace"
        );
    }

    #[test]
    fn mods_json_roundtrip() {
        let json = serialize(&HotkeyMap::default_map());
        let parsed = parse(&json).expect("serialize then parse should succeed");
        assert_eq!(parsed, HotkeyMap::default_map());
    }

    /// `serialize` 的 schema 形状：`version: 1` + 一条不缺 + 命名键的写法。
    /// 前端的 `DEFAULT_WIRE` / `keyMapFromWire` 直接吃这个格式 —— 形状漂了就是
    /// 「面板里所有快捷键静默失效」（见 `frontend/src/keymap.test.ts` 顶部）。
    #[test]
    fn serialize_default_schema_shape() {
        let json = serialize(&HotkeyMap::default_map());
        let value: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");

        assert_eq!(value["version"], serde_json::json!(1));
        let actions = value["actions"].as_object().expect("actions 必须是 object");
        assert_eq!(actions.len(), all_actions().len(), "每个动作都必须被序列化");
        assert_eq!(
            value["actions"]["open_panel"]["mods"],
            serde_json::json!(["meta", "alt"])
        );
        assert_eq!(
            value["actions"]["open_panel"]["key"],
            serde_json::json!("v")
        );
        // 命名键在线格式里是 token（不是字符），前端靠这张 token 表还原 kind。
        assert_eq!(
            value["actions"]["delete_entry"]["key"],
            serde_json::json!("backspace")
        );
    }

    /// `parse` 的拒绝路径：坏 JSON / 截断 / 缺 actions / 未知 version / 只剩修饰键
    /// （`key` 为 null）/ 不认识的键 token / 不认识的修饰键。每一条都必须 `Err` ——
    /// 调用方拿到 `Err` 才敢回落产品默认（R3：不得写库），悄悄接受就会把一个
    /// 永远匹配不上的绑定当成功存下来。
    #[test]
    fn parse_rejects_malformed_input() {
        let cases: &[(&str, &str)] = &[
            ("非法 JSON", "not json at all"),
            ("截断的 JSON", r#"{"version":1,"actions":{"open_panel":{"#),
            ("缺 actions 字段", r#"{"version":1}"#),
            ("未知 version", r#"{"version":999,"actions":{}}"#),
            (
                "key 为 null（只剩修饰键）",
                r#"{"version":1,"actions":{"open_panel":{"mods":["meta","alt"],"key":null}}}"#,
            ),
            (
                "不认识的键 token",
                r#"{"version":1,"actions":{"open_panel":{"mods":["meta"],"key":"nope"}}}"#,
            ),
            (
                "不认识的修饰键",
                r#"{"version":1,"actions":{"open_panel":{"mods":["hyper"],"key":"v"}}}"#,
            ),
        ];
        for (name, json) in cases {
            assert!(parse(json).is_err(), "{name} 必须被拒绝");
        }
    }

    /// 未知 action 静默丢弃（前向兼容），且丢弃不影响其余动作 —— `parse` 从
    /// `default_map()` 出发，未知项只是被跳过。
    #[test]
    fn parse_unknown_action_is_dropped() {
        let json = r#"{
            "version": 1,
            "actions": {
                "open_panel": {"mods": ["meta", "alt"], "key": "v"},
                "future_action": {"mods": ["ctrl"], "key": "x"}
            }
        }"#;
        let parsed = parse(json).expect("未知 action 不应导致 parse 失败");
        assert_eq!(
            parsed.actions.len(),
            all_actions().len(),
            "未知动作被丢弃，其余动作保留默认"
        );
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

    /// 默认表本身必须无冲突：默认绑定里没有任何两条共用同一个组合，且条条都有键。
    #[test]
    fn default_map_has_no_duplicate_combo() {
        let map = HotkeyMap::default_map();
        assert_eq!(map.actions.len(), all_actions().len());
        for (action, bound) in &map.actions {
            let bound = bound.expect("默认映射里没有「未绑定」条目");
            assert_eq!(
                map.conflict_for(*action, bound),
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
        map.actions.insert(HotkeyAction::MoveDown, Some(shared));
        map.actions.insert(HotkeyAction::EditNote, Some(shared));
        // "edit_note" < "move_down" → 前者才是真会拿到派发的那条。
        assert_eq!(
            map.conflict_for(HotkeyAction::Paste, shared),
            Some(HotkeyAction::EditNote)
        );
    }

    // ----------------------------------------------------------------
    // 未绑定（None）：线格式 + claim（强行换绑）
    // ----------------------------------------------------------------

    /// `null` 条目 = 未绑定，且能原样往返（serialize → parse 不丢状态）。
    #[test]
    fn unbound_action_roundtrips_as_json_null() {
        let mut map = HotkeyMap::default_map();
        map.actions.insert(HotkeyAction::Paste, None);

        let json = serialize(&map);
        // 形状：整条写成 null，而不是 {"mods":[],"key":null}。
        assert!(
            json.contains("\"paste\":null"),
            "未绑定必须序列化成 null，实际：{json}"
        );

        let parsed = parse(&json).expect("serialize → parse 必须成功");
        assert_eq!(parsed.actions.get(&HotkeyAction::Paste), Some(&None));
        assert_eq!(parsed, map);
    }

    /// 未绑定的条目不算占用：别人可以自由拿走那个组合。
    #[test]
    fn unbound_action_does_not_own_its_old_combo() {
        let mut map = HotkeyMap::default_map();
        map.actions.insert(HotkeyAction::EditNote, None);
        assert_eq!(
            map.conflict_for(HotkeyAction::Paste, combo(Mods::META, 'e')),
            None,
            "未绑定的 action 不该再占着 ⌘E"
        );
    }

    /// `{"key": null}` 仍是非法 entry（modifier-only），与整条 `null`（未绑定）不同。
    #[test]
    fn entry_with_null_key_is_still_rejected() {
        let json = r#"{"version":1,"actions":{"paste":{"mods":[],"key":null}}}"#;
        let err = parse(json).expect_err("key: null 必须仍然报错");
        assert!(err.contains("modifier-only"), "实际错误：{err}");
    }

    /// 强行换绑：抢到组合的那条拿到键，原来占着它的那条变成未绑定。
    #[test]
    fn claim_takes_combo_and_unbinds_owner() {
        let mut map = HotkeyMap::default_map();
        let stolen = combo(Mods::META, 'e'); // EditNote 的默认键
        map.claim(HotkeyAction::Paste, stolen);

        assert_eq!(map.actions.get(&HotkeyAction::Paste), Some(&Some(stolen)));
        assert_eq!(map.actions.get(&HotkeyAction::EditNote), Some(&None));
        // 表内仍然唯一：抢完不该再有冲突。
        assert_eq!(map.conflict_for(HotkeyAction::Paste, stolen), None);
    }

    /// 解绑**所有**占用者（库里可能留着历史重复），其余条目不受影响。
    #[test]
    fn claim_unbinds_every_other_holder_only() {
        let mut map = HotkeyMap::default_map();
        let shared = combo(Mods::META, 'x');
        map.actions.insert(HotkeyAction::MoveDown, Some(shared));
        map.actions.insert(HotkeyAction::EditNote, Some(shared));
        let untouched = map.actions.get(&HotkeyAction::TogglePin).copied();

        map.claim(HotkeyAction::Paste, shared);

        assert_eq!(map.actions.get(&HotkeyAction::Paste), Some(&Some(shared)));
        assert_eq!(map.actions.get(&HotkeyAction::MoveDown), Some(&None));
        assert_eq!(map.actions.get(&HotkeyAction::EditNote), Some(&None));
        assert_eq!(
            map.actions.get(&HotkeyAction::TogglePin),
            untouched.as_ref()
        );
    }

    /// 抢自己当前的键：不算换绑，自己的键不变。
    #[test]
    fn claim_on_own_combo_is_noop() {
        let mut map = HotkeyMap::default_map();
        let own = combo(Mods::META, 'p'); // TogglePin 的默认键
        map.claim(HotkeyAction::TogglePin, own);
        assert_eq!(map.actions.get(&HotkeyAction::TogglePin), Some(&Some(own)));
        assert_eq!(map, HotkeyMap::default_map());
    }
}
