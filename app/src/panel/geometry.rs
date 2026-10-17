//! 摆放决策的纯函数层（KTD2）。
//!
//! 判据里凡是「决策」（不是取 I/O）的部分都收在这里，用本地轻量几何类型表达、
//! 不依赖任何平台类型，因此两端适配层（[`super::macos`] / [`super::windows`]）共享
//! 同一套算术，且判据能在任一平台上被单测覆盖（R14 / R15）。

use super::{PanelMode, panel_size_for};

/// 几何比较的容差：落位坐标来自系统（整点或半像素），浮点相等的直接比较会偶发失败。
const EPS: f64 = 0.5;

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

/// 展开 / 收回时窗口实际用的尺寸：工作区装不下 expanded 时把宽度收到工作区宽度，
/// 但**不小于** normal 宽度（列表是主界面，再窄就没法用了）。
///
/// 这一档是 plan KTD7 的降级路径：正常 1440pt 以上的工作区走不到它，窄屏 / 分屏
/// 拖到很窄时才生效 —— 那时预览栏自己按 flex 比例被挤窄，而不是窗口溢出屏外。
pub(crate) fn fitted_size(work: Rect, mode: PanelMode) -> (f64, f64) {
    let (w, h) = panel_size_for(mode);
    let (normal_w, _) = panel_size_for(PanelMode::Normal);
    (w.min(work.w.max(normal_w)), h)
}

/// 展开时窗口左上角的落点（左上原点、y 向下；KTD7）。
///
/// **以列表的左上角为锚点**：展开只是右侧多长出一栏，列表不该跟着挪 —— 用户正在
/// 盯着的那一行动一下，方向键的连续操作就断了。
///
/// - 右侧装得下 → 左上角不动，栏向右长。
/// - 右侧不够、左侧够 → 窗口整体左移「长出来的那一截」，列表位置不变、栏向左伸。
/// - 两侧都不够（超窄工作区，宽度已被 [`fitted_size`] 收过）→ 工作区内居中。
pub(crate) fn expanded_origin(
    cur: Point,
    work: Rect,
    size: (f64, f64),
    normal: (f64, f64),
) -> Point {
    let (w, _) = size;
    let y = cur.y; // 高度两档相同，纵向一律不动。
    if cur.x + w <= work.x + work.w + EPS {
        return Point { x: cur.x, y };
    }
    let flipped = cur.x - (w - normal.0);
    if flipped >= work.x - EPS {
        return Point { x: flipped, y };
    }
    Point {
        x: work.x + centered_offset(work, size).0,
        y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn falls_back_to_nearest_when_cursor_sits_in_the_gap() {
        // 与 selects_nearest_display_when_focus_in_gap 同型两块屏（主屏 (0,0,1000,1000)、
        // 副屏 (2000,0,1000,1000)，中间 (1000,2000) 整段空隙），但把焦点空出来、只给一个
        // 落在空隙中的鼠标点，断言 select_display 走「最近屏」档、命中副屏。
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
        // 鼠标在空隙偏副屏一侧 (1700, 500) —— 第 1/2 档都不命中，落到 min_by 取最近（副屏比主屏近 200）。
        let cursor = Point {
            x: 1700.0,
            y: 500.0,
        };
        assert_eq!(select_display(None, Some(cursor), &displays), Some(1));
    }

    #[test]
    fn nearest_probe_prefers_focused_center_over_cursor() {
        // 第 3 档的 `probe` 必须以 focused 中心优先、cursor 兜底；颠倒顺序会让 Cursor 档
        // （前台是自己 / 取不到 pid）的最近屏判定走错屏。
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
        let focused = rect(1900.0, 400.0, 200.0, 200.0); // 中心 (2000,500) 落在副屏
        let cursor = Point { x: 500.0, y: 500.0 }; // 中心在主屏
        assert_eq!(
            select_display(Some(focused), Some(cursor), &displays),
            Some(1)
        );
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

    // ----------------------------------------------------------------
    // 尺寸档位 + 展开落位（列表旁预览栏 plan KTD6 / KTD7 / R1 / R4）
    // ----------------------------------------------------------------

    #[test]
    fn panel_size_normal_matches_tauri_conf() {
        // normal 必须与 `tauri.conf.json` 的初始尺寸一致，否则启动首帧会跳一下。
        assert_eq!(panel_size_for(PanelMode::Normal), (560.0, 420.0));
    }

    #[test]
    fn panel_size_expanded_widens_only() {
        // 高度两档必须相同：预览栏是「右侧多一栏」，列表不该上下跳。
        assert_eq!(panel_size_for(PanelMode::Expanded), (960.0, 420.0));
        assert_eq!(
            panel_size_for(PanelMode::Expanded).1,
            panel_size_for(PanelMode::Normal).1
        );
    }

    #[test]
    fn panel_mode_parses_wire_strings() {
        assert_eq!(PanelMode::parse("normal"), Some(PanelMode::Normal));
        assert_eq!(PanelMode::parse("expanded"), Some(PanelMode::Expanded));
        // 非法值不静默当 normal（前端会收到 Err）。
        assert_eq!(PanelMode::parse("Expanded"), None);
        assert_eq!(PanelMode::parse(""), None);
    }

    #[test]
    fn fitted_size_keeps_expanded_when_work_is_wide_enough() {
        let work = rect(0.0, 0.0, 1440.0, 900.0);
        assert_eq!(fitted_size(work, PanelMode::Expanded), (960.0, 420.0));
        assert_eq!(fitted_size(work, PanelMode::Normal), (560.0, 420.0));
    }

    #[test]
    fn fitted_size_shrinks_to_work_but_never_below_normal() {
        // 工作区 800pt：expanded 收到 800（预览栏被挤窄），列表宽度保住。
        assert_eq!(
            fitted_size(rect(0.0, 0.0, 800.0, 900.0), PanelMode::Expanded),
            (800.0, 420.0)
        );
        // 工作区比 normal 还窄：仍是 normal 宽度（宁可窗口溢出，也不把列表压扁）。
        assert_eq!(
            fitted_size(rect(0.0, 0.0, 400.0, 900.0), PanelMode::Expanded),
            (560.0, 420.0)
        );
    }

    #[test]
    fn expanded_origin_keeps_list_in_place_when_right_side_fits() {
        // 工作区 1440 宽、面板居中在 x=440：右侧还剩 1440-440=1000 ≥ 960 → 不动。
        let work = rect(0.0, 0.0, 1440.0, 900.0);
        let cur = Point { x: 440.0, y: 330.0 };
        let origin = expanded_origin(cur, work, (960.0, 420.0), (560.0, 420.0));
        assert_eq!(origin, cur);
    }

    #[test]
    fn expanded_origin_flips_to_the_left_when_right_side_is_short() {
        // 面板靠右侧（x=800，工作区 1440）：右侧只剩 640 < 960，左侧够 → 整体左移 400。
        let work = rect(0.0, 0.0, 1440.0, 900.0);
        let cur = Point { x: 800.0, y: 300.0 };
        let origin = expanded_origin(cur, work, (960.0, 420.0), (560.0, 420.0));
        assert_eq!(origin, Point { x: 400.0, y: 300.0 });
        // 列表（窗口右侧那 560）仍在原处：800 = 400 + (960-560)。
        assert_eq!(origin.x + (960.0 - 560.0), cur.x);
    }

    #[test]
    fn expanded_origin_centers_when_neither_side_fits() {
        // 工作区 900 宽、面板在 x=300：右侧 600、左侧 300 都不够 960 → 居中。
        let work = rect(0.0, 0.0, 900.0, 900.0);
        let cur = Point { x: 300.0, y: 300.0 };
        let origin = expanded_origin(cur, work, (900.0, 420.0), (560.0, 420.0));
        assert_eq!(origin, Point { x: 0.0, y: 300.0 });
    }

    #[test]
    fn expanded_origin_works_on_negative_origin_display() {
        // 副屏在主屏左侧（工作区原点 -1920）：判据只比相对位移，负原点同样成立。
        let work = rect(-1920.0, 0.0, 1920.0, 1080.0);
        let cur = Point {
            x: -560.0,
            y: 330.0,
        }; // 贴工作区右缘（-560 + 960 = 400 > 0）
        let origin = expanded_origin(cur, work, (960.0, 420.0), (560.0, 420.0));
        assert_eq!(
            origin,
            Point {
                x: -960.0,
                y: 330.0
            }
        );
    }
}
