//! 面板窗口生命周期：唤出入口、多屏定位、点击面板外自动收起。
//!
//! 本模块持有**全应用唯一的「唤出面板」入口** [`show_panel`]：热键回调与托盘（菜单 /
//! 左键点击）都只调用它，因此「先记录回贴目标、再定位到焦点屏、然后显示」这条顺序由
//! 代码结构而不是注释保证（KTD9 / R4）。定位的判据算术是平台无关的纯函数（U1），
//! macOS 适配层见 [`imp_macos`]，Windows 适配层见 [`imp_win`]（两端共享同一套判据）。
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

/// 主面板窗口 label（与 `tauri.conf.json` 的 `windows[0].label` 一致）。
pub const PANEL_LABEL: &str = "main";

/// 面板逻辑尺寸（与 `tauri.conf.json` 的 `width` × `height` 一致）。
///
/// macOS 的 AppKit 点即逻辑像素，不需要缩放换算；Windows 侧（U3）再按目标屏
/// `scale_factor` 换成物理像素。
const PANEL_SIZE: (f64, f64) = (560.0, 420.0);

// ================================================================
// U1：摆放决策纯函数层（KTD2）
//
// 判据里凡是「决策」（不是取 I/O）的部分都收在这里，用本地轻量几何类型表达、
// 不依赖任何平台类型，因此两端适配层（macOS 见本模块的 `imp_macos`）共享同一套
// 算术，且判据能在 macOS 上被单测覆盖（R14 / R15）。
// ================================================================

/// 平面上的一个点（`f64`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Point {
    pub x: f64,
    pub y: f64,
}

/// 轴对齐矩形（`f64`，`(x, y)` 为原点、`w`/`h` 为尺寸）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// 点是否落在矩形内（左闭右开，与显示器矩形判包含的习惯一致）。
    fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.x + self.w && p.y >= self.y && p.y < self.y + self.h
    }

    /// 矩形中心点（选屏判包含时用中心点代表整块「焦点矩形」）。
    fn center(&self) -> Point {
        Point {
            x: self.x + self.w / 2.0,
            y: self.y + self.h / 2.0,
        }
    }
}

/// 一块显示器：**同时**带全屏矩形与工作区矩形。
///
/// - `bounds`（全屏矩形）：选屏时判包含用；macOS `NSScreen.frame`，Windows 显示器矩形。
/// - `work`（工作区）：落位居中用；macOS `NSScreen.visibleFrame`（已扣菜单栏 / Dock），
///   Windows `MONITORINFO.rcWork`（已扣任务栏）。居中基准是工作区而非整屏（R6）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Display {
    pub bounds: Rect,
    pub work: Rect,
}

/// 焦点输入取哪一档（R2 / R2a）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusSource {
    /// 前台是别的应用 → 取前台 / 键窗口所在屏。
    Frontmost,
    /// 前台是 ClipVault 自己，或取不到前台进程号 → 取鼠标所在屏。
    Cursor,
}

/// 判定焦点输入取哪一档（R2 / R2a）。
///
/// - 前台是**别的**应用（`pid != self_pid`）→ `Frontmost`。
/// - 前台是**我们自己**（托盘菜单一弹出 / 面板已持有 key 窗口，前台就变成
///   ClipVault 的菜单窗口）→ `Cursor`。否则会把面板判回它自己所在的那块屏，
///   与「面板跟着用户去他正在工作的屏」的意图相反。
/// - 取不到前台进程号（`None`）→ 也取 `Cursor`：鼠标位置通常仍在，这是与 R3
///   一致的一档，**不得**因此直接跳到主显示器。
///
/// 谓词与 [`crate::pasteback::should_replace_target`] 同形（`Some(pid) if pid != self_pid`），
/// 但两者表达的是不同的东西：那边的 `false` 混着「是自己是」与「取不到进程号」两种情形
/// （都要保留旧目标），这里要把它们分成「前台是别的应用」与「取不到」两档、由调用方各自
/// 处置。摆放判据要能独立演进，故不复用那个谓词。
pub(crate) fn focused_source(front_pid: Option<i32>, self_pid: i32) -> FocusSource {
    match front_pid {
        Some(pid) if pid != self_pid => FocusSource::Frontmost,
        _ => FocusSource::Cursor,
    }
}

/// 点到矩形的轴对齐距离平方（点在矩形内时为 `0`）。用于包含都落空时的「取最近」。
fn rect_distance2(r: Rect, p: Point) -> f64 {
    let dx = (r.x - p.x).max(0.0).max(p.x - (r.x + r.w));
    let dy = (r.y - p.y).max(0.0).max(p.y - (r.y + r.h));
    dx * dx + dy * dy
}

/// 选一块目标显示器，返回它在 `displays` 中的下标。
///
/// 顺序（R2a）：
/// 1. `focused` 矩形的中心落在某块屏内 → 取它；
/// 2. 否则鼠标点落在某块屏内 → 取它；
/// 3. 两者都没命中（例如焦点矩形落在两块屏之间的空隙）→ 取轴对齐距离最近的一块；
/// 4. `displays` 为空，或 `focused` 与 `cursor` 都为空 → `None`（调用方据此回退主屏）。
pub(crate) fn select_display(
    focused: Option<Rect>,
    cursor: Option<Point>,
    displays: &[Display],
) -> Option<usize> {
    if displays.is_empty() {
        return None;
    }
    // 1. 包含判定：focused 优先。
    if let Some(rect) = focused
        && let Some(i) = displays
            .iter()
            .position(|d| d.bounds.contains(rect.center()))
    {
        return Some(i);
    }
    // 2. 包含判定：cursor 其次。
    if let Some(p) = cursor
        && let Some(i) = displays.iter().position(|d| d.bounds.contains(p))
    {
        return Some(i);
    }
    // 3. 包含都落空 → 取最近的显示器（用现有输入里可用的一档作为探针）。
    let probe = focused.map(|r| r.center()).or(cursor)?;
    displays
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            rect_distance2(a.bounds, probe)
                .partial_cmp(&rect_distance2(b.bounds, probe))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
}

/// 窗口在工作区里居中时，其左上角相对工作区左上角的偏移（两个方向各取松弛量的一半）。
///
/// 窗口在某方向上大于工作区时，该方向钳到 `0`——贴工作区左上 / 上沿，而不是把面板
/// 推到工作区外的负坐标（上游 `calculate_window_center_position` 不做这个钳位，
/// 这是有意的改进）。偏移**相对工作区原点**计算，不掺入屏幕的全局原点，因此负原点
/// 的副屏同样成立。
pub(crate) fn centered_offset(work: Rect, window: (f64, f64)) -> (f64, f64) {
    let (w, h) = window;
    (((work.w - w) / 2.0).max(0.0), ((work.h - h) / 2.0).max(0.0))
}

/// 把「左上原点、y 向下」的居中偏移换算成 AppKit 的左上角点（左下原点、y 向上）。
///
/// 局部坐标里窗口左上角 = 工作区左上角 + `offset`；工作区左上角在 AppKit 里是
/// `(work.x, work.y + work.h)`（`work.y` 为工作区**底边**的 y），从顶边**往下**走
/// `dy`，在 AppKit 里就是 y 减去 `dy`。
///
/// 工作区比窗口矮时（`centered_offset` 已把该方向钳到 0），取工作区顶边作为顶——
/// 与纯函数的钳位保持一致（贴工作区左上角）。
///
/// 只在 macOS 有意义，故用 `#[cfg(target_os = "macos")]` 门控：否则 Windows 目标上
/// 会出现一个没有生产调用方的死函数，`clippy -D warnings` 会因此报错。
#[cfg(target_os = "macos")]
pub(crate) fn appkit_frame_origin(
    offset: (f64, f64),
    work: Rect,
    window: (f64, f64),
) -> (f64, f64) {
    let (dx, dy) = offset;
    let (_, win_h) = window;
    let top_x = work.x + dx;
    let top_y = if win_h <= work.h {
        work.y + work.h - dy
    } else {
        work.y + work.h
    };
    (top_x, top_y)
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
    if let Err(e) = window.hide() {
        eprintln!("clipvault: 面板失焦自动收起失败：{e}");
    }
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

/// 按焦点分档解析目标屏并把面板居中过去。macOS 实现见 [`imp_macos`]，Windows 见 [`imp_win`]。
///
/// 定位失败一律内部消化（各自的失败路径留一条 [`log_skip_once`]），调用方 [`show_panel`]
/// 不关心成败——定位失败不影响唤出（R3）。
#[cfg(target_os = "macos")]
fn place_panel(window: &tauri::WebviewWindow) {
    imp_macos::place(window);
}

/// Windows：基于 `GetForegroundWindow` / `GetCursorPos` 的适配，判据算术与 macOS 同源。
#[cfg(target_os = "windows")]
fn place_panel(window: &tauri::WebviewWindow) {
    imp_win::place(window);
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
    eprintln!("clipvault: 面板定位跳过（{path}）：{detail}");
}

/// macOS 定位适配层（KTD3）：全程 AppKit 点坐标，直接用 `NSWindow.setFrameTopLeftPoint`。
#[cfg(target_os = "macos")]
mod imp_macos {
    use objc2_app_kit::{NSEvent, NSRunningApplication, NSScreen, NSWindow, NSWorkspace};
    use objc2_foundation::{MainThreadMarker, NSPoint, NSRect};

    use super::{
        Display, FocusSource, PANEL_SIZE, Point, Rect, appkit_frame_origin, centered_offset,
        focused_source, log_skip_once, select_display,
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
            FocusSource::Frontmost => {
                NSScreen::mainScreen(mtm).map(|screen| rect_of(screen.frame()))
            }
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
        let offset = centered_offset(display.work, PANEL_SIZE);
        let (x, y) = appkit_frame_origin(offset, display.work, PANEL_SIZE);

        let Some(ns) = ns_window(window) else {
            log_skip_once("no-ns-window", "取不到 NSWindow，保留面板原位置");
            return;
        };
        // `show()` 在主线程上是同步的；直接在 `show()` 之前同步落位，避免 tao 的
        // `set_outer_position` 走 `dispatch_async` 造成「先画旧位置一帧」（KTD3）。
        ns.setFrameTopLeftPoint(NSPoint::new(x, y));
    }
}

/// Windows 定位适配层（U3）：全程**物理像素**，落位用 `set_position(PhysicalPosition)`。
///
/// 与 macOS 同源：先用 [`focused_source`] 判自指、按 [`select_display`] 选屏、再用
/// [`centered_offset`] 算居中偏移；差异只在两处取数（Win32 vs AppKit）与坐标系
/// （物理像素 vs AppKit 点、y 向下 vs y 向上）。取数走
/// `GetForegroundWindow` / `GetWindowThreadProcessId` / `GetWindowRect` / `GetCursorPos`，
/// 四个 API 都在已启用的 `Win32_UI_WindowsAndMessaging` feature 下，**不新增 feature**
/// （`MonitorFromWindow` / `GetMonitorInfoW` 在未启用的 `Win32_Graphics_Gdi` 下，故选屏
/// 交给 Tauri 的 `monitor_from_point`，见 `app/Cargo.toml`）。
///
/// # 线程模型
///
/// 本适配层由 `app/src/hotkey.rs` 自建的消息泵线程调用，**不是主线程**。因此只用线程
/// 安全的 Tauri API（`available_monitors` / `monitor_from_point` / `primary_monitor` /
/// `set_position`，内部向事件循环投递消息并等应答），**不引入任何主线程假设**，也不碰
/// AppKit 那套（`MainThreadMarker` / `NSScreen`）。
#[cfg(target_os = "windows")]
mod imp_win {
    use tauri::PhysicalPosition;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
    };

    use super::{
        Display, FocusSource, PANEL_SIZE, Point, Rect, centered_offset, focused_source,
        log_skip_once, select_display,
    };

    /// 当前前台窗口；无前台窗口（例如刚失去激活）时为 `None`。
    fn foreground_window() -> Option<HWND> {
        // SAFETY: `GetForegroundWindow` 无参数、无副作用；返回裸 `HWND`，用
        // `is_invalid()` 判空（无前台窗口时返回空句柄）。
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.is_invalid()).then_some(hwnd)
    }

    /// 窗口所属进程号，换算成与 `focused_source` 的入参同型的 `i32`。
    ///
    /// `GetWindowThreadProcessId` 的出参是 `Option<*mut u32>`：传 `Some(&mut pid)`，
    /// 它写入 pid 并返回线程号（这里不用）。`pid == 0` 视为取不到。
    fn window_process_id(hwnd: HWND) -> Option<i32> {
        let mut pid = 0u32;
        // SAFETY: 出参指向本栈上有效的 `u32`；该 API 只写这一个值。
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        (pid != 0).then_some(pid as i32)
    }

    /// 前台窗口矩形（物理像素，虚拟屏全局坐标）；取不到时为 `None`。
    ///
    /// 这是 R1「Windows 取前台窗口所在屏」的唯一取数路径：把整块矩形交给
    /// [`select_display`]，由它取中心点判包含——只判自指而不取矩形，就会退化成
    /// 「面板跟着鼠标跑」。
    fn window_rect(hwnd: HWND) -> Option<Rect> {
        let mut raw = RECT::default();
        // SAFETY: 出参指向本栈上有效的 `RECT`。
        if unsafe { GetWindowRect(hwnd, &mut raw) }.is_err() {
            return None;
        }
        // `RECT` 是「左上 / 右下」两角，换算成 `(x, y, w, h)`。
        Some(Rect {
            x: raw.left as f64,
            y: raw.top as f64,
            w: (raw.right - raw.left) as f64,
            h: (raw.bottom - raw.top) as f64,
        })
    }

    /// 鼠标位置（物理像素）；取不到时为 `None`。
    fn cursor_point() -> Option<Point> {
        let mut raw = POINT::default();
        // SAFETY: 出参指向本栈上有效的 `POINT`。
        if unsafe { GetCursorPos(&mut raw) }.is_err() {
            return None;
        }
        Some(Point {
            x: raw.x as f64,
            y: raw.y as f64,
        })
    }

    /// 由 Tauri 的 `Monitor` 构造 U1 的 `Display`（全程物理像素）。
    ///
    /// `bounds`（`position` × `size`）供选屏判包含；`work`（`work_area`，已扣任务栏）
    /// 供落位居中（R6）。负原点的副屏同样成立——这些值本就是虚拟屏全局坐标。
    fn display_of(monitor: &tauri::Monitor) -> Display {
        let pos = monitor.position();
        let size = monitor.size();
        let work = monitor.work_area();
        Display {
            bounds: Rect {
                x: pos.x as f64,
                y: pos.y as f64,
                w: size.width as f64,
                h: size.height as f64,
            },
            work: Rect {
                x: work.position.x as f64,
                y: work.position.y as f64,
                w: work.size.width as f64,
                h: work.size.height as f64,
            },
        }
    }

    /// 定位面板并在目标工作区内居中。
    pub(super) fn place(window: &tauri::WebviewWindow) {
        // 取不到显示器列表与列表为空走同一条降级路径，故一并判定。
        let monitors = window.available_monitors().unwrap_or_default();
        if monitors.is_empty() {
            log_skip_once("no-monitors", "取不到显示器列表，保留面板原位置");
            return;
        }
        let displays: Vec<Display> = monitors.iter().map(display_of).collect();

        // 前台窗口 → 进程号 → 自指判定（R2 / R2a）。前台是别的应用时用前台窗口矩形，
        // 是 ClipVault 自己（托盘菜单弹出 / 面板已持焦点）或取不到进程号时退到鼠标屏。
        let self_pid = std::process::id() as i32;
        let front = foreground_window();
        let front_pid = front.and_then(window_process_id);
        let cursor = cursor_point();
        let focused = match focused_source(front_pid, self_pid) {
            FocusSource::Frontmost => front.and_then(window_rect),
            FocusSource::Cursor => None,
        };

        let selected = select_display(focused, cursor, &displays);

        // 目标显示器：判据选中的下标与 `monitors` 一一对应（两者同序同长），直接取该块。
        // 不用 `monitor_from_point` 再问一次系统：那条路要投递到事件循环并阻塞等应答，
        // 而本函数跑在热键消息泵线程上，多一次往返只会拖慢热键响应，结果却与直接索引
        // 完全相同。两个输入都取不到（`selected` 为 `None`）时回退主屏（R3）。
        let target = match selected {
            Some(i) => monitors
                .get(i)
                .map(|m| (m.scale_factor(), displays[i].work)),
            None => window
                .primary_monitor()
                .ok()
                .flatten()
                .map(|m| (m.scale_factor(), display_of(&m).work)),
        };
        let Some((scale, work)) = target else {
            log_skip_once("no-display", "解析不出目标显示器，保留面板原位置");
            return;
        };

        // 面板逻辑尺寸按目标屏 `scale_factor` 换算成物理像素（Windows 全程物理像素）。
        let window_size = (PANEL_SIZE.0 * scale, PANEL_SIZE.1 * scale);
        let (dx, dy) = centered_offset(work, window_size);
        // Windows 用左上原点、y 向下，`centered_offset` 的偏移可直接相加，无需 AppKit
        // 式的 y 翻转（`appkit_frame_origin` 只对 macOS 有意义，故不在此处出现）。
        let x = (work.x + dx).round() as i32;
        let y = (work.y + dy).round() as i32;

        if window.set_position(PhysicalPosition::new(x, y)).is_err() {
            log_skip_once("set-position-failed", "set_position 失败，保留面板原位置");
        }
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

    // ----------------------------------------------------------------
    // U1：摆放决策纯函数（R2 / R2a / R3 / R6 / R14 / R15）
    // ----------------------------------------------------------------

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    /// 两块屏的常见布局：`displays[0]` 是主屏（原点 0），`[1]` 在左侧（负原点）。
    fn two_displays() -> Vec<Display> {
        vec![
            Display {
                bounds: rect(0.0, 0.0, 1920.0, 1080.0),
                work: rect(0.0, 0.0, 1920.0, 1055.0),
            },
            Display {
                bounds: rect(-1920.0, 0.0, 1920.0, 1080.0),
                work: rect(-1920.0, 0.0, 1920.0, 1080.0),
            },
        ]
    }

    #[test]
    fn focused_source_is_frontmost_for_another_app() {
        assert_eq!(focused_source(Some(4242), 1234), FocusSource::Frontmost);
    }

    #[test]
    fn focused_source_is_cursor_when_front_is_self() {
        // 托盘菜单弹出 / 面板已持有 key 窗口：前台是 ClipVault 自己 → 看鼠标屏（R2）。
        assert_eq!(focused_source(Some(1234), 1234), FocusSource::Cursor);
    }

    #[test]
    fn focused_source_is_cursor_when_front_pid_unknown() {
        // 取不到前台进程号 → 落到下一档（鼠标屏），不得直接跳主屏（R2a）。
        assert_eq!(focused_source(None, 1234), FocusSource::Cursor);
    }

    #[test]
    fn selects_second_display_for_negative_origin_focus() {
        let displays = two_displays();
        // 焦点矩形完全落在负原点的第二块屏内（x = -229）。
        let focused = rect(-229.0, 100.0, 100.0, 100.0);
        assert_eq!(select_display(Some(focused), None, &displays), Some(1));
    }

    #[test]
    fn selects_nearest_display_when_focus_in_gap() {
        let displays = vec![
            Display {
                bounds: rect(0.0, 0.0, 1000.0, 1000.0),
                work: rect(0.0, 0.0, 1000.0, 1000.0),
            },
            Display {
                bounds: rect(2000.0, 0.0, 1000.0, 1000.0),
                work: rect(2000.0, 0.0, 1000.0, 1000.0),
            },
        ];
        // 焦点矩形落在两块屏之间的空隙；中心 (1600, 600) 离第二块屏更近。
        let focused = rect(1400.0, 400.0, 400.0, 400.0);
        assert_eq!(select_display(Some(focused), None, &displays), Some(1));
    }

    #[test]
    fn selects_display_under_cursor_when_no_focus() {
        let displays = two_displays();
        let cursor = Point {
            x: -500.0,
            y: 500.0,
        };
        assert_eq!(select_display(None, Some(cursor), &displays), Some(1));
    }

    #[test]
    fn returns_none_without_focus_or_cursor() {
        // 调用方据此回退主屏（R3）。
        assert_eq!(select_display(None, None, &two_displays()), None);
    }

    #[test]
    fn returns_none_when_no_displays() {
        let cursor = Point { x: 10.0, y: 10.0 };
        assert_eq!(
            select_display(Some(rect(0.0, 0.0, 10.0, 10.0)), Some(cursor), &[]),
            None
        );
    }

    #[test]
    fn centered_offset_splits_slack() {
        let work = rect(0.0, 0.0, 1000.0, 800.0);
        assert_eq!(centered_offset(work, (560.0, 420.0)), (220.0, 190.0));
    }

    #[test]
    fn centered_offset_clamps_when_window_larger_than_work() {
        let work = rect(0.0, 0.0, 400.0, 300.0);
        let (dx, dy) = centered_offset(work, (560.0, 420.0));
        assert_eq!((dx, dy), (0.0, 0.0));
        assert!(dx >= 0.0 && dy >= 0.0);
    }

    #[test]
    fn centered_offset_is_relative_to_work_origin() {
        // 副屏在主屏左侧（负原点）：偏移只由工作区尺寸决定，不掺入全局原点。
        let at_zero = centered_offset(rect(0.0, 0.0, 1920.0, 1080.0), (560.0, 420.0));
        let negative = centered_offset(rect(-1920.0, 0.0, 1920.0, 1080.0), (560.0, 420.0));
        assert_eq!(at_zero, negative);
        assert_eq!(negative, (680.0, 330.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn appkit_frame_origin_flips_y_on_negative_origin_work() {
        let work = rect(-1920.0, 0.0, 1920.0, 1080.0);
        let window = (560.0, 420.0);
        let offset = centered_offset(work, window);
        // AppKit 左上角点 = (工作区左 + dx, 工作区顶 - dy)。
        assert_eq!(appkit_frame_origin(offset, work, window), (-1240.0, 750.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn appkit_frame_origin_keeps_window_top_when_work_is_short() {
        // 工作区比窗口矮：贴工作区左上角（与 centered_offset 的钳位一致）。
        let work = rect(100.0, 100.0, 400.0, 300.0);
        let window = (560.0, 420.0);
        let offset = centered_offset(work, window);
        assert_eq!(appkit_frame_origin(offset, work, window), (100.0, 400.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn appkit_frame_origin_flush_with_work_bottom() {
        // 窗口恰好占满工作区高度：底边贴在屏幕下沿（AppKit y = work.y）。
        let work = rect(0.0, 0.0, 1920.0, 1080.0);
        let window = (560.0, 1080.0);
        let offset = centered_offset(work, window);
        let (x, top) = appkit_frame_origin(offset, work, window);
        assert_eq!((x, top), (680.0, 1080.0));
        assert_eq!(top - window.1, work.y);
    }
}
