//! 面板窗口生命周期：唤出入口、多屏定位、点击面板外自动收起。
//!
//! 本模块持有**全应用唯一的「唤出面板」入口** [`show_panel`]：热键回调与托盘（菜单 /
//! 左键点击）都只调用它，因此「先记录回贴目标、再定位到焦点屏、然后显示」这条顺序由
//! 代码结构而不是注释保证（KTD9 / R4）。定位的判据算术是平台无关的纯函数
//! （[`geometry`]），macOS 适配层见 [`macos`]，Windows 适配层见 [`windows`]（两端共享
//! 同一套判据）。
//!
//! macOS 上「点击面板之外」的表现是**面板窗口失去 key 状态**（`NSWindow`
//! `resignKey`）：点别的 App 的窗口、点桌面、拉开托盘菜单都会触发。Tauri 把它
//! 转成 [`tauri::WindowEvent::Focused`]`(false)`，这里据此把面板 `hide()`。
//!
//! **不**复用 [`crate::pasteback::hide_panel_and_restore_focus`] 的「隐藏 + 把激活
//! 权交还上一个 App」三步：那次面板之外的点击本身就已经把激活权交给了被点的对象
//! （另一个 App 的窗口、或用户刚点开的菜单），再把焦点抢回唤出面板前的 App，等于
//! 和用户刚做的选择对着干。Esc 关闭走的是那条路径（用户没有点任何东西，激活权该
//! 还给它原来的主人），两条路径的语义不同，故不合并。

use tauri::{AppHandle, Manager, Window};

mod geometry;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// 主面板窗口 label（与 `tauri.conf.json` 的 `windows[0].label` 一致）。
pub const PANEL_LABEL: &str = "main";

/// 面板尺寸档位（列表旁预览栏 plan KTD6）。
///
/// 两档而不是「随便设尺寸」：`tauri.conf.json` 里的初始尺寸必须与 [`PanelMode::Normal`]
/// 一致，否则启动首帧会跳一下；`Expanded` 只是「normal + 右侧预览栏」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelMode {
    /// 560×420：列表独占面板（`tauri.conf.json` 的初始尺寸）。
    Normal,
    /// 960×420：右侧长出 400pt 的预览栏，高度不变（列表不上下抖）。
    Expanded,
}

impl PanelMode {
    /// 前端 IPC 的字符串 → 档位。非法值返回 `None`（命令层据此报错，
    /// 而不是悄悄当 normal —— 那样面板会莫名缩一下，用户只看到「预览打不开」）。
    pub fn parse(raw: &str) -> Option<PanelMode> {
        match raw {
            "normal" => Some(PanelMode::Normal),
            "expanded" => Some(PanelMode::Expanded),
            _ => None,
        }
    }
}

/// 两档的逻辑尺寸（macOS 的 AppKit 点 / Windows 的逻辑像素，落位时再按平台换算）。
pub fn panel_size_for(mode: PanelMode) -> (f64, f64) {
    match mode {
        PanelMode::Normal => (560.0, 420.0),
        PanelMode::Expanded => (960.0, 420.0),
    }
}

/// 展开前的窗口左上角，供收回时把列表放回原处（R4：对齐到开启前的那条工作区边界）。
///
/// 存的是**本平台的落位坐标**（macOS：AppKit 左上角点；Windows：物理像素左上角）——
/// 两侧各写各读，不跨平台解释，也就没有换算误差。只记最新一次展开的前一档：
/// 连续展开 / 收回本来就有序，成对调用（`Remember` → `Take`）之后自然清空。
static RESTORE_ORIGIN: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);

/// 记住「展开前」的位置。只在切到 expanded 时调用。
fn remember_restore_origin(origin: (f64, f64)) {
    if let Ok(mut slot) = RESTORE_ORIGIN.lock() {
        *slot = Some(origin);
    }
}

/// 取走「展开前」的位置（取一次即清空）。收回时调用。
fn take_restore_origin() -> Option<(f64, f64)> {
    RESTORE_ORIGIN.lock().ok().and_then(|mut slot| slot.take())
}

/// 是否还留着「上一次展开」的恢复点（不取走）。
fn has_restore_origin() -> bool {
    RESTORE_ORIGIN
        .lock()
        .map(|slot| slot.is_some())
        .unwrap_or(false)
}

/// 逻辑尺寸 → `set_size`（Tauri 内部按窗口 `scale_factor` 换算成物理像素）。
///
/// 两个平台适配层共用同一份：它不含平台语义，失败也只在日志里留痕。其余平台走
/// [`set_mode`] 里的内联分支（那里连日志文案都不同），故在此门控掉。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn set_size(window: &tauri::WebviewWindow, size: (f64, f64)) {
    if let Err(e) = window.set_size(tauri::LogicalSize::new(size.0, size.1)) {
        ::log::warn!(target: "panel", "set_panel_mode 设置尺寸失败：{e}");
    }
}

/// 切换面板尺寸档位：先设尺寸，再按平台落位（macOS 见 [`macos`]，Windows 见
/// [`windows`]）。
///
/// 调用方是 `commands::set_panel_mode`（前端预览栏开 / 关时发的那条 IPC）。失败一律
/// 内部消化并留一条日志：尺寸没变不会让面板不可用，而把错误抛给前端只会多一条
/// 没人看得懂的 toast。
pub fn set_mode(window: &tauri::WebviewWindow, mode: PanelMode) {
    #[cfg(target_os = "macos")]
    macos::apply_mode(window, mode);
    #[cfg(target_os = "windows")]
    windows::apply_mode(window, mode);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // 其余平台没有预览栏（Deferred），但仍跟进尺寸，免得窗口停在一个
        // 前端已经不再布局的宽度上。
        let (w, h) = panel_size_for(mode);
        if let Err(e) = window.set_size(tauri::LogicalSize::new(w, h)) {
            ::log::warn!(target: "panel", "set_panel_mode({mode:?}) 失败：{e}");
        }
    }
}

/// 关面板之前把窗口收回 normal（R12）。
///
/// 只有「上一次展开留下了恢复点」时才动窗口 —— 常规隐藏（面板本来就是 normal）多做
/// 一次 `set_size` 只是让每次收起都慢一拍。Esc / 失焦自动收起都先走这里；回贴路径
/// 由前端 `await closePreview()` 提前收过，走到这里时已经没有恢复点。
pub fn restore_normal(window: &tauri::WebviewWindow) {
    if !has_restore_origin() {
        return;
    }
    set_mode(window, PanelMode::Normal);
}

/// 失焦时是否应当自动收起面板。抽成纯判定便于单测。
///
/// 两个条件缺一不可：
/// - `visible`：已经隐藏的窗口不必再隐藏。回贴链路会**先**隐藏面板
///   （`pasteback` 的让位三步），隐藏动作本身也会带来一次失焦事件；不设这道闸，
///   降级分支（`window_switched` / `clipboard_only` / `no_accessibility`）紧跟着
///   `show()` 出来的提示面板会被那次陈旧失焦立刻藏掉，用户既看不到 toast、又得
///   重新唤起面板。
/// - `!focused`：事件是排队投递的。`show()` + `set_focus()` 之后仍可能收到一次
///   早于这次聚焦的 `Focused(false)`；以窗口**当下**的 key 状态为准，才是
///   「现在真的不在前台」，而不是「曾经不在前台」。
pub fn should_auto_hide(visible: bool, focused: bool) -> bool {
    visible && !focused
}

/// 窗口事件入口：`Focused(false)` 且窗口可见 → 收起面板。其余事件不处理。
///
/// 由 `main.rs` 的 `Builder::on_window_event` 接线；跑在主线程上，`hide()` 就地生效。
/// `is_visible()` / `is_focused()` 取不到值时**什么都不做**：状态不明时让窗口留在
/// 屏幕上，比让它莫名消失安全。
pub fn handle_window_event(window: &Window, event: &tauri::WindowEvent) {
    if !matches!(event, tauri::WindowEvent::Focused(false)) {
        return;
    }
    if window.label() != PANEL_LABEL {
        return;
    }
    let (Ok(visible), Ok(focused)) = (window.is_visible(), window.is_focused()) else {
        return;
    };
    if !should_auto_hide(visible, focused) {
        return;
    }
    // 预览栏开着时先把窗口收回 560×420，再隐藏（R12）：否则下一次唤出会带着 960 的
    // 尺寸出现，而 `place()` 是按 normal 算的位置。
    if let Some(panel) = window.app_handle().get_webview_window(PANEL_LABEL) {
        restore_normal(&panel);
    }
    if let Err(e) = window.hide() {
        ::log::warn!(target: "panel", "面板失焦自动收起失败：{e}");
        return;
    }
    // 收起同样意味着录制态结束（快捷键 plan KTD9）：用户点去别处时，设置里那一行不该还攥着
    // 「请按新键…」，⌥⌘V 也不该继续缺席 —— 让全局热键就地归位。
    crate::keymap::restore_global_hotkey_after_leaving_panel(window.app_handle());
}

// ================================================================
// U2：唤出入口 + 定位适配层（KTD9）
// ================================================================

/// 显示并聚焦主面板 —— **全应用唯一的「唤出面板」入口**。
///
/// 热键回调（`hotkey.rs`）与托盘的菜单 / 左键点击都走这里，从而保证
/// 「**先**记录回贴目标、**再**显示面板」的顺序在两处一致 —— 顺序反了就会把目标记成
/// ClipVault 自己（见 [`crate::pasteback::capture_target_app`]），此后的回贴都会误判为
/// 「目标窗口已切换」。
///
/// 内部顺序固定为：取目标 App → 定位 → `show()` → `set_focus()`（R4）。把入口收进面板
/// 模块、而不是留在托盘模块，是为了让「先定位、再显示」由模块边界保证，而不是只靠注释
/// ——任何新唤出路径直接 `window.show()` 都会静默跳过定位。
///
/// 定位是「尽力而为」：解析不出目标屏时回退主屏，定位调用本身失败时保留原位置，
/// 两者都不得阻止唤出（R3）。
pub(crate) fn show_panel(app: &AppHandle) {
    let Some(window) = app.get_webview_window(PANEL_LABEL) else {
        return;
    };
    crate::pasteback::capture_target_app();
    place_panel(&window);
    let _ = window.show();
    let _ = window.set_focus();
}

/// 按焦点分档解析目标屏并把面板居中过去。macOS 实现见 [`macos`]，Windows 见
/// [`windows`]。
///
/// 定位失败一律内部消化（各自的失败路径留一条 [`log_skip_once`]），调用方 [`show_panel`]
/// 不关心成败——定位失败不影响唤出（R3）。
#[cfg(target_os = "macos")]
fn place_panel(window: &tauri::WebviewWindow) {
    macos::place(window);
}

/// Windows：基于 `GetForegroundWindow` / `GetCursorPos` 的适配，判据算术与 macOS 同源。
#[cfg(target_os = "windows")]
fn place_panel(window: &tauri::WebviewWindow) {
    windows::place(window);
}

/// 其余平台：不定位，面板留在原位（plan 的 Deferred：Linux 等平台沿用 `#[cfg]` 降级）。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn place_panel(_window: &tauri::WebviewWindow) {}

/// 按路径去重的调试日志：同一失败路径只在首次打一条，避免每次唤出刷屏。
///
/// 「静默降级」是本链路最大的风险（Risks）：拿不到 `MainThreadMarker`、取不到
/// `NSWindow` 等都会保留原位置而没有任何测试会红，这条日志是可观测信号。
/// macOS 与 Windows 两个适配层共用（两侧的失败路径都靠它留痕）。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn log_skip_once(path: &'static str, detail: &str) {
    static LOGGED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    {
        let mut seen = LOGGED.lock().unwrap();
        if seen.contains(&path) {
            return;
        }
        seen.push(path);
    }
    // 先放锁再写 stderr：日志路径不值得让别的线程等 I/O。
    ::log::warn!(target: "panel::display", "面板定位跳过（{path}）：{detail}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_when_visible_and_unfocused() {
        assert!(should_auto_hide(true, false));
    }

    #[test]
    fn keeps_hidden_window_alone() {
        // 回贴链路先 hide 再让位，那次失焦事件不能反过来影响后面的 show()。
        assert!(!should_auto_hide(false, false));
    }

    #[test]
    fn keeps_focused_window() {
        assert!(!should_auto_hide(true, true));
    }

    #[test]
    fn keeps_hidden_and_focused_window() {
        assert!(!should_auto_hide(false, true));
    }
}
