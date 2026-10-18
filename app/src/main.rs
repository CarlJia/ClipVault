//! ClipVault 主入口（U4）。
//!
//! `setup` 钩子里完成：
//! 1. 初始化诊断日志 sink（U2 早期）—— 失败不阻塞启动。
//! 2. 打开 SQLite 存储（U2）。
//! 3. 构造与 watcher / IPC 共享的 `guest_mode` / `skip_next` 原子开关。
//! 4. 把 [`AppState`] 注册进 Tauri，供 IPC 命令读取。
//! 5. 在 tokio 运行时上 spawn U3 的 [`Watcher`]。
//! 6. 构建菜单栏托盘（U4）。
//! 7. 注册 Carbon 全局热键 ⌥⌘V（U4 / KTD2）。
//! 8. 注册窗口事件钩子：面板失焦（点击面板之外）时自动收起，见 [`panel`]。
//!
//! 另有一处 **Windows 专属**步骤：`log::init` 之后、打开存储之前先抢单实例锁；
//! 重复启动的进程（又点了一次图标）会请主实例唤出面板，然后自己退出 ——
//! 见 [`clipvault::single_instance`]。
//!
//! 主窗口在 `tauri.conf.json` 中初始 `visible: false`，靠热键 / 托盘唤出，
//! 符合 LSUIElement 菜单栏应用形态。

// 生产构建（release + 非 test）下隐藏 Windows 控制台窗口；对 macOS 无影响，
// 但保持与 Tauri 惯例一致。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32};

use clipvault::commands::{self, AppState};
use clipvault::log::{self, Level};
use clipvault::panel;
use clipvault::privacy;
use clipvault::storage::{Storage, default_db_path};
use clipvault::tray;
use clipvault::watcher::Watcher;
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        // 8. 点击面板之外 → 面板失焦 → 自动收起。
        .on_window_event(panel::handle_window_event)
        .setup(|app| {
            // 开发模式 (`cargo tauri dev`) 下 binary 直接从 target/debug 运行,
            // 不在 .app bundle 里,系统读不到 Info.plist 的 LSUIElement=true,
            // 会按普通 foreground app 处理、Dock 出现图标。打包后的 .app 由
            // Info.plist 自然隐藏,但为保持开发/打包行为一致,运行时显式设
            // 激活策略为 Accessory(菜单栏 app,无 Dock)。`macos-private-api`
            // feature 已启用。
            #[cfg(target_os = "macos")]
            {
                use tauri::ActivationPolicy;
                if let Err(e) = app
                    .handle()
                    .set_activation_policy(ActivationPolicy::Accessory)
                {
                    eprintln!("clipvault: 设置 ActivationPolicy::Accessory 失败：{e}");
                }
            }
            // 1. U2：初始化诊断日志 sink。最先于 `Storage::open` 之前,这样
            //    后续即便 `Storage::open` 失败也能在文件里留痕(R1)。
            //    失败用兜底:由 `log` crate 的默认行为(no-op)或后续 log::*! 走
            //    stderr(U2 注释),不阻塞启动。
            let log_level = Arc::new(AtomicU8::new(Level::Info as u8));
            if let Err(e) = log::init(&log::default_log_path(), log_level.clone()) {
                eprintln!("[clipvault] 初始化日志文件失败：{e}；日志将不会落到文件");
            }
            ::log::info!(target: "main", "ClipVault 启动");

            // 1.5 单实例守卫（Windows）：必须早于 `Storage::open` / watcher / 托盘 /
            //     全局热键 —— 次实例要去做的每一件事都会和主实例抢同一批资源。
            //     理由（含为什么放在 `log::init` 之后）见 [`clipvault::single_instance`]。
            #[cfg(target_os = "windows")]
            if matches!(
                clipvault::single_instance::acquire(&app.config().identifier),
                clipvault::single_instance::Startup::Secondary
            ) {
                ::log::info!(target: "main", "已有实例在运行，已请求它唤出面板；本次启动退出");
                app.handle().exit(0);
                return Ok(());
            }

            // 1.6 主实例接管「又点了一次图标」的信号：收到就唤出面板。
            #[cfg(target_os = "windows")]
            clipvault::single_instance::spawn_activation_listener(app.handle().clone());

            // 2. 打开存储。
            let storage = Arc::new(Storage::open(&default_db_path())?);

            // 3. 共享开关。guest_mode 持久化于 settings 表；skip_next 纯运行时。
            //    panel_opacity（U12）持久化于 settings 表，但 watcher 不消费。
            let guest_mode = Arc::new(AtomicBool::new(false));
            let skip_next = Arc::new(AtomicBool::new(false));
            let panel_opacity = Arc::new(AtomicU32::new(privacy::DEFAULT_PANEL_OPACITY));
            let keymap = Arc::new(RwLock::new(clipvault_core::keymap::HotkeyMap::default_map()));
            // W7 / R30：自写抑制槽。`Watcher` 与 `AppState` 共享同一份 ——
            // 「复制日志路径」写剪贴板前登记指纹，watcher 下一轮认出并跳过。
            let self_writes: Arc<tokio::sync::Mutex<Option<String>>> =
                Arc::new(tokio::sync::Mutex::new(None));

            // 3.5 W3 / R11:一次 `SELECT key, value FROM settings WHERE key IN (…)` 把
            //      guest_mode / panel_opacity / hotkeys / log_level 四个键全取回来,
            //      替代原先四次独立 `block_on`(每次都要重新拿 `Storage::conn` 的
            //      tokio mutex)。读回后在内存里分发给各模块的 parse + apply。
            let boot_settings = tauri::async_runtime::block_on(storage.get_settings_batch(&[
                clipvault::privacy::GUEST_MODE_KEY,
                clipvault::privacy::PANEL_OPACITY_KEY,
                clipvault::keymap::HOTKEYS_KEY,
                log::LOG_LEVEL_KEY,
            ]))
            .unwrap_or_default();

            // 3.5a 访客模式 → 原子(托盘图标 / 前端 / watcher 共享)。
            let initial_guest = clipvault::privacy::parse_guest_value(
                boot_settings
                    .get(clipvault::privacy::GUEST_MODE_KEY)
                    .map(String::as_str),
            );
            clipvault::privacy::apply_guest_mode(&guest_mode, initial_guest);

            // 3.5b 面板透明度 → 原子(前端启动时一次性对齐 `--panel-alpha`)。
            let _initial_opacity = clipvault::privacy::parse_panel_opacity(
                boot_settings
                    .get(clipvault::privacy::PANEL_OPACITY_KEY)
                    .map(String::as_str),
            );
            clipvault::privacy::apply_panel_opacity(&panel_opacity, _initial_opacity);

            // 3.5c 快捷键映射 → slot(全局热键与 panel 内热键)。
            let initial_keymap = match clipvault::keymap::parse_hotkeys(
                boot_settings
                    .get(clipvault::keymap::HOTKEYS_KEY)
                    .map(String::as_str),
            ) {
                Ok(map) => map,
                Err(e) => {
                    ::log::warn!(target: "main", "boot_load_hotkeys 失败：{e}，使用默认映射");
                    clipvault_core::keymap::HotkeyMap::default_map()
                }
            };
            clipvault::keymap::apply_hotkeys(&keymap, &initial_keymap);

            // 3.5d 日志级别 → 原子。注意:与旧路径等价 —— 旧 `boot_load_log_level`
            //      也只 `atomic.store`,不在这里调 `log::set_max_level`(那由
            //      `log::init` 与运行时 `set_log_level` 负责)。
            let _initial_log_level =
                log::parse_level(boot_settings.get(log::LOG_LEVEL_KEY).map(String::as_str));
            log_level.store(
                _initial_log_level as u8,
                std::sync::atomic::Ordering::SeqCst,
            );

            // 注册 IPC 共享状态（在 apply_global_hotkey 之前，因为 apply 需要 AppState.app）。
            app.manage(AppState {
                storage: storage.clone(),
                guest_mode: guest_mode.clone(),
                skip_next: skip_next.clone(),
                panel_opacity: panel_opacity.clone(),
                keymap: keymap.clone(),
                log_level: log_level.clone(),
                self_writes: self_writes.clone(),
                app: app.handle().clone(),
            });

            // 4.4 U9：启动时校验加密区条目的可解密性
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                if clipvault::crypto::system_key_available() {
                    let storage_for_verify = storage.clone();
                    tauri::async_runtime::spawn(async move {
                        let cipher = clipvault::crypto::default_vault_cipher();
                        if let Err(e) = storage_for_verify
                            .verify_decryptable_on_startup(&cipher)
                            .await
                        {
                            ::log::warn!(target: "main", "加密区可解密性校验失败：{e}");
                        }
                    });
                }
            }

            // 5. 后台剪贴板 watcher（U3），与上面共享同一批开关。
            let watcher = Watcher::new(storage, guest_mode, skip_next, self_writes);
            tauri::async_runtime::spawn(watcher.run());

            // 6. 菜单栏托盘（反映启动时加载到的访客模式状态）。
            tray::build_tray(app.handle(), initial_guest)?;

            // 7. U2：全局热键 re-register（boot 时从 settings 表加载的 initial_keymap）。
            if let Err(e) = clipvault::keymap::apply_global_hotkey(app.handle(), &initial_keymap) {
                ::log::warn!(target: "main", "apply_global_hotkey 失败：{e}（热键可能不生效）");
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_results,
            commands::list_recent,
            commands::get_all_tags,
            commands::get_categories,
            commands::check_accessibility,
            commands::platform_info,
            commands::hide_panel,
            commands::open_accessibility_settings,
            commands::paste_entry,
            commands::toggle_pin,
            commands::update_entry,
            commands::delete_entry,
            commands::get_entry_preview,
            commands::set_panel_mode,
            commands::set_guest_mode,
            commands::get_guest_mode,
            commands::set_skip_next,
            commands::get_panel_opacity,
            commands::set_panel_opacity,
            commands::get_hotkeys,
            commands::set_hotkey,
            commands::set_hotkey_recording,
            commands::keychain_available,
            commands::move_to_vault,
            commands::list_vault,
            commands::clear_undecryptable,
            commands::open_url,
            commands::open_external_url,
            commands::app_info,
            commands::get_log_level,
            commands::set_log_level,
            commands::read_log,
            commands::clear_log,
            commands::copy_log_path,
            commands::reset_hotkeys,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClipVault");
}
