//! 菜单栏托盘图标 + 菜单 + 状态变体（U4 / R2）。
//!
//! 托盘图标使用应用自带图标并置为 **template image**（`icon_as_template(true)`），
//! 从而随 macOS 菜单栏明暗自动反色。四种状态变体（监听中 / 访客模式 / 已暂停 / 异常）
//! 通过 tooltip 文案 + macOS 托盘标题字形（title glyph）区分，无需重建图标——
//! [`set_state`] 只更新现有托盘对象。像素级 SF Symbol 徽标（红点 / ⚠️ 叠加）
//! 作为后续视觉打磨项，当前以字形 + tooltip 表达（足以驱动 U10 的访客/暂停切换）。
//!
//! 托盘菜单项：打开主面板 / 本次不记录 / 访客模式(toggle) / 加密区 / 关于 / 退出。
//! 访客模式与「本次不记录」直接写入与 watcher 共享的原子开关（U10 完成持久化）。

use std::sync::atomic::Ordering;

use tauri::AppHandle;
use tauri::Manager;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::commands::AppState;

/// 托盘的稳定 id，供 [`set_state`] 通过 `app.tray_by_id` 找回。
const TRAY_ID: &str = "clipvault-tray";

/// 被 Tauri `manage` 的访客模式 CheckMenuItem 句柄。
///
/// 让前端触发的 `set_guest_mode` 命令也能把菜单栏勾选态同步过来
/// （托盘自身点击由 muda 原生翻转勾选态，命令路径则需显式设置），U10。
pub struct GuestMenuItem(pub CheckMenuItem<tauri::Wry>);

/// 托盘状态变体（对应 plan 中的四态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    /// 监听中：标准图标，无附加标记。
    Monitoring,
    /// 访客模式：仅查看，不记录。
    Guest,
    /// 已暂停：监听停止。
    Paused,
    /// 异常：Keychain 等不可用。
    Error,
}

impl TrayState {
    /// tooltip 文案。
    fn tooltip(self) -> String {
        match self {
            TrayState::Monitoring => "ClipVault — 监听中".to_string(),
            TrayState::Guest => "ClipVault — 访客模式（仅查看，不记录）".to_string(),
            TrayState::Paused => "ClipVault — 已暂停".to_string(),
            TrayState::Error => "ClipVault — 异常（Keychain 不可用）".to_string(),
        }
    }

    /// macOS 托盘标题字形：监听中无字形，其余状态用一个紧凑标记。
    #[cfg(target_os = "macos")]
    fn title_glyph(self) -> String {
        match self {
            TrayState::Monitoring => String::new(),
            TrayState::Guest => "•".to_string(),
            TrayState::Paused => "⏸".to_string(),
            TrayState::Error => "⚠".to_string(),
        }
    }
}

/// 构建并注册托盘图标 + 菜单。应在 Tauri `setup` 中调用一次。
///
/// `guest_mode` 为启动时从 `settings` 表加载到的持久化状态（U10）：据此设置
/// 访客模式 CheckMenuItem 的初始勾选态与托盘图标初始变体，使重启后 UI 与库一致。
pub fn build_tray(app: &AppHandle, guest_mode: bool) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开主面板", true, None::<&str>)?;
    let skip = MenuItem::with_id(app, "skip_once", "本次不记录", true, None::<&str>)?;
    let guest = CheckMenuItem::with_id(app, "guest", "访客模式", true, guest_mode, None::<&str>)?;
    let vault = MenuItem::with_id(app, "vault", "加密区", true, None::<&str>)?;
    let about = MenuItem::with_id(app, "about", "关于 ClipVault", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    // 存下 CheckMenuItem 句柄，供命令路径（前端开关）同步勾选态。
    app.manage(GuestMenuItem(guest.clone()));

    let menu = Menu::with_items(
        app,
        &[&open, &skip, &guest, &vault, &sep, &about, &sep, &quit],
    )?;

    let initial = if guest_mode {
        TrayState::Guest
    } else {
        TrayState::Monitoring
    };

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(initial.tooltip())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_icon_event);

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone()).icon_as_template(true);
    }

    builder.build(app)?;
    // 图标构建完成后再落初始状态字形（set_title 需已存在的托盘对象）。
    set_state(app, initial);
    Ok(())
}

/// 更新托盘状态变体（tooltip + 字形），不重建图标。
pub fn set_state(app: &AppHandle, state: TrayState) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(state.tooltip()));
        #[cfg(target_os = "macos")]
        {
            let glyph = state.title_glyph();
            let _ = tray.set_title(if glyph.is_empty() { None } else { Some(glyph) });
        }
    }
}

/// 显示并聚焦主面板。
fn show_panel(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 托盘菜单事件分发。
fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        "open" => show_panel(app),
        "skip_once" => {
            if let Some(state) = app.try_state::<AppState>() {
                crate::privacy::set_skip_next(&state.skip_next);
            }
        }
        "guest" => {
            if let Some(state) = app.try_state::<AppState>() {
                // 运行时即时生效：以共享原子为准取反。
                let new = !state.guest_mode.load(Ordering::SeqCst);
                crate::privacy::apply_guest_mode(&state.guest_mode, new);
                set_state(
                    app,
                    if new {
                        TrayState::Guest
                    } else {
                        TrayState::Monitoring
                    },
                );
                // CheckMenuItem 原生已翻转勾选态；显式对齐一次，避免与命令路径漂移。
                if let Some(item) = app.try_state::<GuestMenuItem>() {
                    let _ = item.0.set_checked(new);
                }
                // 持久化到 settings 表（运行时已生效，落库为 best-effort）。
                let storage = state.storage.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::privacy::persist_guest_mode(&storage, new).await {
                        eprintln!("clipvault: 访客模式持久化失败：{e}");
                    }
                });
            }
        }
        // 加密区（U9）/ 关于面板（后续）暂先打开主面板占位。
        "vault" | "about" => show_panel(app),
        "quit" => {
            crate::hotkey::unregister();
            app.exit(0);
        }
        _ => {}
    }
}

/// 托盘图标事件：左键单击呼出主面板。
fn on_tray_icon_event(tray: &tauri::tray::TrayIcon, event: TrayIconEvent) {
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        show_panel(tray.app_handle());
    }
}
