//! macOS 定位适配层（KTD3）：全程 AppKit 点坐标，直接用 `NSWindow.setFrameTopLeftPoint`。

use objc2_app_kit::{NSEvent, NSRunningApplication, NSScreen, NSWindow, NSWorkspace};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect};

use super::geometry::{
    Display, FocusSource, Point, Rect, appkit_frame_origin, centered_offset, expanded_origin,
    fitted_size, focused_source, select_display,
};
use super::{
    PanelMode, log_skip_once, panel_size_for, remember_restore_origin, set_size,
    take_restore_origin,
};

/// 面板的 NSWindow（`tauri::WebviewWindow::ns_window` 的稳定逃生口）。
///
/// `ns_window()` 取不到时返回 `None`（走「定位调用失败」分支）。
fn ns_window(window: &tauri::WebviewWindow) -> Option<&'static NSWindow> {
    let ptr = window.ns_window().ok()?;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `ns_window()` 返回的是本窗口活着的 `NSWindow` 指针；Tauri 自身文档
    // 与运行时同样把它当 `&NSWindow` 用（与 `pasteback::imp` 直调 AppKit 的边界
    // 同类）。引用只在本函数返回的调用作用域内使用，且调用发生在主线程。
    Some(unsafe { &*(ptr as *const NSWindow) })
}

fn rect_of(r: NSRect) -> Rect {
    Rect {
        x: r.origin.x,
        y: r.origin.y,
        w: r.size.width,
        h: r.size.height,
    }
}

fn point_of(p: NSPoint) -> Point {
    Point { x: p.x, y: p.y }
}

/// 由一块 `NSScreen` 构造 [`Display`]：`frame` 供选屏、`visibleFrame` 供落位。
fn display_of(screen: &NSScreen) -> Display {
    Display {
        bounds: rect_of(screen.frame()),
        work: rect_of(screen.visibleFrame()),
    }
}

/// 由 `NSScreen.screens` 构造显示器列表。
fn displays(mtm: MainThreadMarker) -> Vec<Display> {
    NSScreen::screens(mtm)
        .iter()
        .map(|screen| display_of(&screen))
        .collect()
}

/// 按 KTD1 / R2 / R2a 解析目标显示器。
///
/// `Frontmost` 档取 `NSScreen.mainScreen`（键窗口所在屏）；`Cursor` 档取
/// `NSEvent::mouseLocation()`。选屏交给平台无关的 [`select_display`]，它也承担 R2a 的
/// 输入优先级：前台窗口所在屏 → 鼠标所在屏 → 主显示器（`select_display` 先取
/// `focused`、再取 `cursor`）。两档都取不到屏（或列表为空）时回退主屏（R3）——
/// `NSScreen.mainScreen` 不可得时取列表首项（`screens[0]` 即菜单栏所在的主显示器）。
fn pick_display(mtm: MainThreadMarker, displays: &[Display]) -> Option<Display> {
    let front_pid = NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .map(|app| app.processIdentifier());
    let self_pid = NSRunningApplication::currentApplication().processIdentifier();

    // 鼠标位置始终作为「下一档」输入：`focused` 先被判包含，落空后才轮到它（R2a）。
    let cursor = Some(point_of(NSEvent::mouseLocation()));
    let focused = match focused_source(front_pid, self_pid) {
        FocusSource::Frontmost => NSScreen::mainScreen(mtm).map(|screen| rect_of(screen.frame())),
        FocusSource::Cursor => None,
    };

    match select_display(focused, cursor, displays) {
        Some(i) => displays.get(i).copied(),
        None => NSScreen::mainScreen(mtm)
            .map(|screen| display_of(&screen))
            .or_else(|| displays.first().copied()),
    }
}

/// 定位并在主线程上同步生效。
pub(super) fn place(window: &tauri::WebviewWindow) {
    // `NSScreen.mainScreen` / `screens` 需要 `MainThreadMarker`（KTD3）；三条唤出路径
    // （Carbon 回调、托盘菜单、托盘左键）本来就在主线程，这里只是显式校验。
    let Some(mtm) = MainThreadMarker::new() else {
        log_skip_once("not-main-thread", "不在主线程，保留面板原位置");
        return;
    };
    let screens = displays(mtm);
    let Some(display) = pick_display(mtm, &screens) else {
        log_skip_once("no-screen", "取不到任何显示器，保留面板原位置");
        return;
    };
    let offset = centered_offset(display.work, panel_size_for(PanelMode::Normal));
    let (x, y) = appkit_frame_origin(offset, display.work, panel_size_for(PanelMode::Normal));

    let Some(ns) = ns_window(window) else {
        log_skip_once("no-ns-window", "取不到 NSWindow，保留面板原位置");
        return;
    };
    // `show()` 在主线程上是同步的；直接在 `show()` 之前同步落位，避免 tao 的
    // `set_outer_position` 走 `dispatch_async` 造成「先画旧位置一帧」（KTD3）。
    ns.setFrameTopLeftPoint(NSPoint::new(x, y));
}

/// 切换尺寸档位（U2 / R1 / R4）：`Expanded` 把列表左上角锚在原地向右长，
/// `Normal` 回到展开前的位置。
///
/// 全程 AppKit 点：`frame` 的 y 是**底边**，而 [`expanded_origin`] 与
/// [`appkit_frame_origin`] 吃的是「左上原点、y 向下」，故进出各翻转一次
/// （`work.y + work.h` 是工作区顶边）。
pub(super) fn apply_mode(window: &tauri::WebviewWindow, mode: PanelMode) {
    // 尺寸先落：`setFrameTopLeftPoint` 只动左上角，与尺寸无关，顺序不会互相覆盖。
    let Some(mtm) = MainThreadMarker::new() else {
        log_skip_once("mode-not-main-thread", "不在主线程，只改尺寸不改位置");
        set_size_only(window, mode);
        return;
    };
    let Some(ns) = ns_window(window) else {
        log_skip_once("mode-no-ns-window", "取不到 NSWindow，只改尺寸不改位置");
        set_size_only(window, mode);
        return;
    };
    // 位置只读一次：落位判据与「展开前的位置」必须来自同一帧。
    let frame = ns.frame();
    let screens = displays(mtm);
    // 以**窗口自己**所在屏为基准：尺寸切换发生时面板已经在屏幕上了，而鼠标可能停在
    // 另一块屏（刚在那块屏点了一下就按 Space）。按前台 / 鼠标选屏会让面板横跳到
    // 鼠标那块去，列表位置也跟着丢。取不到窗口矩形时才回退到唤出时的选屏判据。
    let display = select_display(Some(rect_of(frame)), None, &screens)
        .and_then(|i| screens.get(i).copied())
        .or_else(|| pick_display(mtm, &screens));
    let Some(display) = display else {
        log_skip_once("mode-no-screen", "取不到显示器，只改尺寸不改位置");
        set_size_only(window, mode);
        return;
    };
    let work = display.work;
    let size = fitted_size(work, mode);
    set_size(window, size);

    match mode {
        PanelMode::Normal => {
            // 收回：回到展开前的位置。没有记录（不是从 expanded 回来的，例如
            // 启动后前端首次对齐）就什么都别做 —— 当前位置本来就是对的。
            if let Some((x, y)) = take_restore_origin() {
                ns.setFrameTopLeftPoint(NSPoint::new(x, y));
            }
        }
        PanelMode::Expanded => {
            // 存**左上角点**：`setFrameTopLeftPoint` 收回时吃的就是这个点，而
            // `frame.origin` 是左下角，原样存回会让面板每次开合下沉一个窗口高。
            remember_restore_origin((frame.origin.x, frame.origin.y + frame.size.height));
            let cur = Point {
                x: frame.origin.x,
                y: work.y + work.h - (frame.origin.y + frame.size.height),
            };
            let origin = expanded_origin(cur, work, size, panel_size_for(PanelMode::Normal));
            // `expanded_origin` 的 y 已经是「距工作区顶边」的偏移（`cur.y` 就是这么算的），
            // 与 `centered_offset` 同一种量，不能再减一次 `work.y`。
            let (x, y) = appkit_frame_origin((origin.x - work.x, origin.y), work, size);
            ns.setFrameTopLeftPoint(NSPoint::new(x, y));
        }
    }
}

/// 只改尺寸（落位前置条件不满足时的降级）：窗口大小对了，位置留在原处。
fn set_size_only(window: &tauri::WebviewWindow, mode: PanelMode) {
    set_size(window, panel_size_for(mode));
}
