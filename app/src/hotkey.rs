//! 全局快捷键（⌥⌘V）+ Accessibility 检测（U4 / KTD2 / R2 / R14 基础）。
//!
//! **实现选择：** 直接声明 Carbon `RegisterEventHotKey` 家族的 `extern "C"` 绑定，
//! 并 `#[link(name = "Carbon", kind = "framework")]`。`RegisterEventHotKey` /
//! `InstallEventHandler` 是纯 C 的 Carbon API，不是 Objective-C 消息，因此不需要
//! 走 `objc2` 的 `msg_send`——原生 FFI 是最直接、依赖最少的路径，且完全满足
//! KTD2「用 Carbon 而非 tauri-plugin-global-shortcut」的约束（不引入任何 shortcut 插件）。
//! `objc2` 仍在 watcher 里用于 NSPasteboard。
//!
//! Carbon 事件处理器安装在 application event target 上，回调在主线程 run loop 中被调用，
//! 因此可以安全地直接调用 Tauri 的 `window.show()` / `set_focus()`。
//!
//! 非 macOS 平台整体降级为 no-op（`register` 什么都不做，`check_accessibility` 返回 `true`）。

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tauri::{AppHandle, Manager};

    // ---- Carbon 不透明类型 ----
    type OsStatus = i32;
    type EventTargetRef = *mut c_void;
    type EventHandlerRef = *mut c_void;
    type EventHandlerCallRef = *mut c_void;
    type EventRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EventHotKeyID {
        signature: u32,
        id: u32,
    }

    type EventHandlerProc = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> EventTargetRef;
        fn InstallEventHandler(
            target: EventTargetRef,
            handler: EventHandlerProc,
            num_types: u32,
            list: *const EventTypeSpec,
            user_data: *mut c_void,
            out_ref: *mut EventHandlerRef,
        ) -> OsStatus;
        fn RegisterEventHotKey(
            key_code: u32,
            modifiers: u32,
            hot_key_id: EventHotKeyID,
            target: EventTargetRef,
            options: u32,
            out_ref: *mut EventHotKeyRef,
        ) -> OsStatus;
        fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
    }

    // ---- Carbon 常量 ----
    /// `'keyb'` —— kEventClassKeyboard。
    const K_EVENT_CLASS_KEYBOARD: u32 = 0x6b65_7962;
    /// kEventHotKeyPressed。
    const K_EVENT_HOT_KEY_PRESSED: u32 = 6;
    /// Carbon `cmdKey` 修饰位。
    const CMD_KEY: u32 = 0x0100;
    /// Carbon `optionKey` 修饰位。
    const OPTION_KEY: u32 = 0x0800;
    /// `kVK_ANSI_V` 虚拟键码。
    const KVK_ANSI_V: u32 = 0x09;
    /// 热键签名 `'CLPV'`。
    const HOTKEY_SIGNATURE: u32 = 0x434c_5056;

    /// 主线程 run loop 中被回调使用的 AppHandle。
    static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();
    /// 已注册热键的指针（存为 usize 以便跨线程持有 / 卸载）。
    static HOTKEY_REF: AtomicUsize = AtomicUsize::new(0);

    /// Carbon 热键回调：显示并聚焦主面板（F2）。
    extern "C" fn hotkey_handler(
        _call: EventHandlerCallRef,
        _event: EventRef,
        _user: *mut c_void,
    ) -> OsStatus {
        if let Some(app) = APP_HANDLE.get()
            && let Some(window) = app.get_webview_window("main")
        {
            let _ = window.show();
            let _ = window.set_focus();
        }
        0
    }

    /// 注册 ⌥⌘V 全局热键。重复调用是幂等的（已注册则直接返回）。
    pub fn register(app: &AppHandle) {
        // 保存 AppHandle 供回调使用。
        let _ = APP_HANDLE.set(app.clone());

        if HOTKEY_REF.load(Ordering::SeqCst) != 0 {
            return; // 已注册。
        }

        unsafe {
            let target = GetApplicationEventTarget();

            // 安装事件处理器（只装一次）。
            let spec = EventTypeSpec {
                event_class: K_EVENT_CLASS_KEYBOARD,
                event_kind: K_EVENT_HOT_KEY_PRESSED,
            };
            let mut handler_ref: EventHandlerRef = std::ptr::null_mut();
            let status = InstallEventHandler(
                target,
                hotkey_handler,
                1,
                &spec,
                std::ptr::null_mut(),
                &mut handler_ref,
            );
            if status != 0 {
                eprintln!("clipvault: InstallEventHandler 失败, status={status}");
                return;
            }

            let hot_key_id = EventHotKeyID {
                signature: HOTKEY_SIGNATURE,
                id: 1,
            };
            let mut hotkey_ref: EventHotKeyRef = std::ptr::null_mut();
            let status = RegisterEventHotKey(
                KVK_ANSI_V,
                CMD_KEY | OPTION_KEY,
                hot_key_id,
                target,
                0,
                &mut hotkey_ref,
            );
            if status != 0 {
                eprintln!("clipvault: RegisterEventHotKey 失败, status={status}");
                return;
            }
            HOTKEY_REF.store(hotkey_ref as usize, Ordering::SeqCst);
        }
    }

    /// 卸载热键（应用退出时可选调用）。
    pub fn unregister() {
        let ptr = HOTKEY_REF.swap(0, Ordering::SeqCst);
        if ptr != 0 {
            unsafe {
                UnregisterEventHotKey(ptr as EventHotKeyRef);
            }
        }
    }

    // ---- Accessibility ----
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    /// 返回进程是否已获得 Accessibility 授权（AE7 检测接缝）。
    pub fn check_accessibility() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }
}

#[cfg(target_os = "macos")]
pub use imp::{check_accessibility, register, unregister};

// ---- 非 macOS：no-op 降级，保证跨平台可编译 ----

/// 非 macOS 平台：不注册任何全局热键。
#[cfg(not(target_os = "macos"))]
pub fn register(_app: &tauri::AppHandle) {}

/// 非 macOS 平台：无热键可卸载。
#[cfg(not(target_os = "macos"))]
pub fn unregister() {}

/// 非 macOS 平台：无 Accessibility 概念，视为已授权。
#[cfg(not(target_os = "macos"))]
pub fn check_accessibility() -> bool {
    true
}
