//! 面板窗口生命周期（点击面板外自动收起）。
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

use tauri::Window;

/// 主面板窗口 label（与 `tauri.conf.json` 的 `windows[0].label` 一致）。
pub const PANEL_LABEL: &str = "main";

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
    if let Err(e) = window.hide() {
        eprintln!("clipvault: 面板失焦自动收起失败：{e}");
    }
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
