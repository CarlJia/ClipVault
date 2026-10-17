//! Windows 定位适配层（U3）：全程**物理像素**，落位用 `set_position(PhysicalPosition)`。
//!
//! 与 macOS 同源：先用 [`focused_source`] 判自指、按 [`select_display`] 选屏、再用
//! [`centered_offset`] 算居中偏移；差异只在两处取数（Win32 vs AppKit）与坐标系
//! （物理像素 vs AppKit 点、y 向下 vs y 向上）。取数走
//! `GetForegroundWindow` / `GetWindowThreadProcessId` / `GetWindowRect` / `GetCursorPos`，
//! 四个 API 都在已启用的 `Win32_UI_WindowsAndMessaging` feature 下，**不新增 feature**
//! （`MonitorFromWindow` / `GetMonitorInfoW` 处于未启用的 `Win32_Graphics_Gdi` 下，与本适配层无关：选屏在 `available_monitors()` 的同一份列表上直接按下标取用，不调用 `monitor_from_point`，详见 `place()` 内联注释）。
//!
//! # 线程模型
//!
//! 本适配层由 `app/src/hotkey.rs` 自建的消息泵线程调用，**不是主线程**。因此只用线程
//! 安全的 Tauri API（`available_monitors` / `primary_monitor` / `set_position`，内部向事件循环投递消息并等应答），**不引入任何主线程假设**，也不碰
//! AppKit 那套（`MainThreadMarker` / `NSScreen`）。

use tauri::PhysicalPosition;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId,
};

use super::geometry::{
    Display, FocusSource, Point, Rect, centered_offset, expanded_origin, fitted_size,
    focused_source, select_display,
};
use super::{
    PanelMode, log_skip_once, panel_size_for, remember_restore_origin, set_size,
    take_restore_origin,
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
fn hwnd_rect(hwnd: HWND) -> Option<Rect> {
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

/// 显示器列表 → `(monitors, displays)`：两者同序同长，下标可互查。
///
/// 取不到显示器列表与列表为空走同一条降级路径，故一并判定。
fn monitors_and_displays(
    window: &tauri::WebviewWindow,
) -> Option<(Vec<tauri::Monitor>, Vec<Display>)> {
    let monitors = window.available_monitors().ok().filter(|m| !m.is_empty())?;
    let displays: Vec<Display> = monitors.iter().map(display_of).collect();
    Some((monitors, displays))
}

/// 窗口自身的外框（物理像素、虚拟屏全局坐标）；取不到时为 `None`。
fn window_rect(window: &tauri::WebviewWindow) -> Option<Rect> {
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    Some(Rect {
        x: f64::from(pos.x),
        y: f64::from(pos.y),
        w: f64::from(size.width),
        h: f64::from(size.height),
    })
}

/// 目标显示器：`(scale_factor, 工作区)`，全程物理像素。`None` = 取不到，调用方
/// 各自走降级。
///
/// 前台窗口 → 进程号 → 自指判定（R2 / R2a）；前台是别的应用时用前台窗口矩形，
/// 是 ClipVault 自己（托盘菜单弹出 / 面板已持焦点）或取不到进程号时退到鼠标屏。
///
/// 判据选中的下标与 `monitors` 一一对应（两者同序同长），直接取该块：不用
/// `monitor_from_point` 再问一次系统——那条路要投递到事件循环并阻塞等应答，
/// 而调用方跑在热键消息泵线程上，多一次往返只会拖慢热键响应，结果却与直接索引
/// 完全相同。两个输入都取不到（`select_display` 为 `None`）时回退主屏（R3）。
fn target_display(window: &tauri::WebviewWindow) -> Option<(f64, Rect)> {
    let (monitors, displays) = monitors_and_displays(window)?;

    let self_pid = std::process::id() as i32;
    let front = foreground_window();
    let front_pid = front.and_then(window_process_id);
    let cursor = cursor_point();
    let focused = match focused_source(front_pid, self_pid) {
        FocusSource::Frontmost => front.and_then(hwnd_rect),
        FocusSource::Cursor => None,
    };

    match select_display(focused, cursor, &displays) {
        Some(i) => monitors
            .get(i)
            .map(|m| (m.scale_factor(), displays[i].work)),
        None => window
            .primary_monitor()
            .ok()
            .flatten()
            .map(|m| (m.scale_factor(), display_of(&m).work)),
    }
}

/// 尺寸切换的基准显示器：**窗口自己所在的那块**，不是鼠标 / 前台那块。
///
/// 尺寸切换发生时面板已经在屏幕上了，而鼠标常常停在另一块屏（刚在那块屏点了一下
/// 就按 Space 预览）。照唤出时的判据选屏，会把面板整个拽到鼠标那块去 —— 用户看到
/// 的是「按一下预览，窗口跳屏了」。
fn window_display(window: &tauri::WebviewWindow, win: Option<Rect>) -> Option<(f64, Rect)> {
    let (monitors, displays) = monitors_and_displays(window)?;
    match select_display(win, None, &displays) {
        Some(i) => monitors
            .get(i)
            .map(|m| (m.scale_factor(), displays[i].work)),
        None => window
            .primary_monitor()
            .ok()
            .flatten()
            .map(|m| (m.scale_factor(), display_of(&m).work)),
    }
}

/// 定位面板并在目标工作区内居中。
pub(super) fn place(window: &tauri::WebviewWindow) {
    let Some((scale, work)) = target_display(window) else {
        log_skip_once(
            "no-display",
            "取不到显示器列表 / 解析不出目标显示器，保留面板原位置",
        );
        return;
    };

    // 面板逻辑尺寸按目标屏 `scale_factor` 换算成物理像素（Windows 全程物理像素）。
    let normal = panel_size_for(PanelMode::Normal);
    let window_size = (normal.0 * scale, normal.1 * scale);
    let (dx, dy) = centered_offset(work, window_size);
    // Windows 用左上原点、y 向下，`centered_offset` 的偏移可直接相加，无需 AppKit
    // 式的 y 翻转（`appkit_frame_origin` 只对 macOS 有意义，故不在此处出现）。
    let x = (work.x + dx).round() as i32;
    let y = (work.y + dy).round() as i32;

    if window.set_position(PhysicalPosition::new(x, y)).is_err() {
        log_skip_once("set-position-failed", "set_position 失败，保留面板原位置");
    }
}

/// 切换尺寸档位（U2 / R1 / R4）。本适配层跑在热键消息泵线程上，只用线程安全的
/// Tauri API；坐标全程**物理像素**（与 [`place`] 同一套）。
pub(super) fn apply_mode(window: &tauri::WebviewWindow, mode: PanelMode) {
    // 位置只问系统一次：`cur` 与「展开前位置」必须来自同一次读数。
    let win = window_rect(window);
    let Some((scale, work)) = window_display(window, win) else {
        log_skip_once(
            "mode-no-display",
            "取不到显示器列表 / 解析不出目标显示器，只改尺寸不改位置",
        );
        set_size_only(window, mode);
        return;
    };

    // `fitted_size` / `expanded_origin` 吃逻辑点：把工作区换算回逻辑尺寸再算，
    // 最后统一乘回 `scale` 落到物理像素。
    let work_logical = Rect {
        x: work.x / scale,
        y: work.y / scale,
        w: work.w / scale,
        h: work.h / scale,
    };
    let size = fitted_size(work_logical, mode);

    match mode {
        PanelMode::Normal => {
            set_size(window, size);
            // 回到展开前的位置（物理像素原样写回，不经过逻辑换算）。
            if let Some((x, y)) = take_restore_origin() {
                let _ =
                    window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
            }
        }
        PanelMode::Expanded => {
            let cur = win
                .map(|r| Point {
                    x: r.x / scale,
                    y: r.y / scale,
                })
                .unwrap_or(Point {
                    x: work_logical.x,
                    y: work_logical.y,
                });
            if let Some(r) = win {
                remember_restore_origin((r.x, r.y));
            }
            let origin =
                expanded_origin(cur, work_logical, size, panel_size_for(PanelMode::Normal));
            set_size(window, size);
            let x = (origin.x * scale).round() as i32;
            let y = (origin.y * scale).round() as i32;
            if window.set_position(PhysicalPosition::new(x, y)).is_err() {
                log_skip_once(
                    "mode-set-position-failed",
                    "set_position 失败，保留面板原位置",
                );
            }
        }
    }
}

/// 落位前置条件不满足时的降级：尺寸按物理像素算，位置留在原处。
fn set_size_only(window: &tauri::WebviewWindow, mode: PanelMode) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let (w, h) = panel_size_for(mode);
    if let Err(e) = window.set_size(tauri::PhysicalSize::new(
        (w * scale).round() as u32,
        (h * scale).round() as u32,
    )) {
        ::log::warn!(target: "panel", "set_panel_mode 设置尺寸失败：{e}");
    }
}
