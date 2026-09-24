//! ClipVault 主入口（U4）。
//!
//! `setup` 钩子里完成：
//! 1. 打开 SQLite 存储（U2）。
//! 2. 构造与 watcher / IPC 共享的 `guest_mode` / `skip_next` 原子开关。
//! 3. 把 [`AppState`] 注册进 Tauri，供 IPC 命令读取。
//! 4. 在 tokio 运行时上 spawn U3 的 [`Watcher`]。
//! 5. 构建菜单栏托盘（U4）。
//! 6. 注册 Carbon 全局热键 ⌥⌘V（U4 / KTD2）。
//!
//! 主窗口在 `tauri.conf.json` 中初始 `visible: false`，靠热键 / 托盘唤出，
//! 符合 LSUIElement 菜单栏应用形态。

// 生产构建（release + 非 test）下隐藏 Windows 控制台窗口；对 macOS 无影响，
// 但保持与 Tauri 惯例一致。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clipvault::commands::{self, AppState};
use clipvault::hotkey;
use clipvault::storage::{Storage, default_db_path};
use clipvault::tray;
use clipvault::watcher::Watcher;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // 1. 打开存储。
            let storage = Arc::new(Storage::open(&default_db_path())?);

            // 2. 共享开关（U10 完成持久化）。
            let guest_mode = Arc::new(AtomicBool::new(false));
            let skip_next = Arc::new(AtomicBool::new(false));

            // 3. 注册 IPC 共享状态。
            app.manage(AppState {
                storage: storage.clone(),
                guest_mode: guest_mode.clone(),
                skip_next: skip_next.clone(),
            });

            // 4. 后台剪贴板 watcher（U3），与上面共享同一批开关。
            let watcher = Watcher::new(storage, guest_mode, skip_next);
            tauri::async_runtime::spawn(watcher.run());

            // 5. 菜单栏托盘。
            tray::build_tray(app.handle())?;

            // 6. Carbon 全局热键 ⌥⌘V。
            hotkey::register(app.handle());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_results,
            commands::list_recent,
            commands::get_all_tags,
            commands::get_categories,
            commands::check_accessibility,
            commands::hide_panel,
            commands::open_accessibility_settings,
            commands::paste_entry,
            commands::toggle_pin,
            commands::update_entry,
            commands::set_guest_mode,
            commands::get_guest_mode,
            commands::set_skip_next,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClipVault");
}
