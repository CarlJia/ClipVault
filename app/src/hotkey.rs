//! 全局快捷键（⌥⌘V）+ Accessibility 检测（U2 / U4 / KTD2 / KTD3 / R2 / R14 基础）。
//!
//! ## 热键注册 API（U2）
//!
//! `register(app, action: HotkeyAction, combo: &KeyCombo) -> Result<(), ConflictReason>`
//! 接受显式 `KeyCombo`，失败时返回 `ConflictReason`。
//!
//! macOS：`Mutex<Vec<(EventHotKeyRef, EventHotKeyID, HotkeyAction)>>` 支持多 hotkey，
//! 通过 `EventHotKeyID.id` 路由。`unregister_all()` 遍历注销。
//!
//! Windows：通道式消息泵线程，`PumpMsg::Register / Unregister / Quit` 通过 channel 发送，
//! pump 内部维护 `HashMap<i32, HotkeyAction>` 路由。
//!
//! ## Mods 位标志 → Carbon / Win32 修饰掩码（U2）
//!
//! `Mods` 位布局：Meta=0x01 / Shift=0x02 / Alt=0x04 / Ctrl=0x08。
//! Carbon 位常量：cmdKey=0x0100 / shiftKey=0x0200 / optionKey=0x0800 / controlKey=0x1000。
//! **两者位布局不同，翻译必须显式 match，不得混用常量。**
//!
//! ## 实现选择（延续 U4）
//!
//! 直接声明 Carbon `RegisterEventHotKey` 家族的 `extern "C"` 绑定，
//! 并 `#[link(name = "Carbon", kind = "framework")]`。`RegisterEventHotKey` /
//! `InstallEventHandler` 是纯 C 的 Carbon API，不需要 `objc2`。
//!
//! Windows 侧用 `RegisterHotKey` 注册 `Ctrl+Alt+V`，并为本热键单开一条消息泵线程。
//!
//! 其余平台整体降级为 no-op。

// ---------------------------------------------------------------------------
// ConflictReason（U2）
// ---------------------------------------------------------------------------

/// 热键注册失败原因。
#[derive(Debug, Clone)]
pub enum ConflictReason {
    /// 该热键被现有程序占用；`description` 含 platform 错误码 + 原始 Carbon / Win32 Errno 字面量。
    Taken(String),
}

impl std::fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConflictReason::Taken(d) => write!(f, "已被占用：{d}"),
        }
    }
}

impl std::error::Error for ConflictReason {}

// ---------------------------------------------------------------------------
// KeyCombo → Carbon 平台翻译（U2）
// ---------------------------------------------------------------------------

/// 将 `clipvault_core::keymap::Mods`（Meta=0x01 / Shift=0x02 / Alt=0x04 / Ctrl=0x08）
/// 翻译为 Carbon 修饰掩码（cmdKey=0x0100 / shiftKey=0x0200 / optionKey=0x0800 / controlKey=0x1000）。
///
/// **注意**：Carbon 的位常量与 `Mods` 位标志**不是同一个布局**；
/// 本函数做显式 match 翻译，调用方不得混用两侧常量。
///
/// SAFETY/notes:
/// - Carbon 的位布局是 `cmdKey=0x0100 / shiftKey=0x0200 / optionKey=0x0800 / controlKey=0x1000`，
///   与 `Mods`（Meta=0x01 / Shift=0x02 / Alt=0x04 / Ctrl=0x08）完全不同。
/// - `Mods::META`（0x01）映射到 Carbon `cmdKey`（0x0100）—— 不是 1:1 位平移。
#[cfg(target_os = "macos")]
fn mods_to_carbon(mods: clipvault_core::keymap::Mods) -> u32 {
    use clipvault_core::keymap::Mods;
    let mut mask = 0u32;
    if mods.contains(Mods::META) {
        mask |= 0x0100; // cmdKey
    }
    if mods.contains(Mods::SHIFT) {
        mask |= 0x0200; // shiftKey
    }
    if mods.contains(Mods::ALT) {
        mask |= 0x0800; // optionKey
    }
    if mods.contains(Mods::CTRL) {
        mask |= 0x1000; // controlKey
    }
    mask
}

/// 将 `clipvault_core::keymap::CharKind` 翻译为 Carbon 虚拟键码（kVK_*）。
///
/// macOS 虚拟键码不是 ASCII：kVK_ANSI_A=0x00, B=0x0B, C=0x08, Q=0x0C,
/// S=0x01, V=0x09, W=0x0D, X=0x07, Y=0x10, Z=0x06。
/// 使用静态查找表而非公式推算。
///
/// SAFETY/notes:
/// - 返回值是 macOS 虚拟键码表中对应键的 kVK_ANSI_* / kVK_* 常量。
/// - 字母键用大写形式查找。
#[cfg(target_os = "macos")]
fn charkind_to_vkey(kind: clipvault_core::keymap::CharKind) -> Option<u32> {
    use clipvault_core::keymap::CharKind;

    // macOS 虚拟键码表（kVK_ANSI_* 值，非 ASCII）。
    // 格式：char -> vk code。
    const VK_TABLE: &[(char, u32)] = &[
        // 字母（按字母序排列，但虚拟键码不连续）。
        ('A', 0x00),
        ('S', 0x01),
        ('D', 0x02),
        ('F', 0x03),
        ('H', 0x04),
        ('G', 0x05),
        ('Z', 0x06),
        ('X', 0x07),
        ('C', 0x08),
        ('V', 0x09),
        ('B', 0x0B),
        ('Q', 0x0C),
        ('W', 0x0D),
        ('E', 0x0E),
        ('R', 0x0F),
        ('Y', 0x10),
        ('T', 0x11),
        ('1', 0x12),
        ('2', 0x13),
        ('3', 0x14),
        ('4', 0x17),
        ('6', 0x15),
        ('5', 0x16),
        ('9', 0x19),
        ('7', 0x1A),
        ('8', 0x1C),
        ('0', 0x1D),
        ('O', 0x1F),
        ('U', 0x21),
        ('I', 0x22),
        ('P', 0x23),
        ('L', 0x25),
        ('J', 0x26),
        ('K', 0x28),
        (',', 0x2B),
        ('N', 0x2D),
        ('M', 0x2E),
    ];

    match kind {
        CharKind::Char(c) => {
            let upper = c.to_ascii_uppercase();
            VK_TABLE
                .iter()
                .find(|&&(ch, _)| ch == upper)
                .map(|&(_, vk)| vk)
        }
        CharKind::Enter => Some(0x24),     // kVK_Return
        CharKind::Escape => Some(0x35),    // kVK_Escape
        CharKind::ArrowUp => Some(0x7E),   // kVK_UpArrow
        CharKind::ArrowDown => Some(0x7D), // kVK_DownArrow
        CharKind::Comma => Some(0x2B),     // kVK_ANSI_Comma
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::ConflictReason;
    use super::{charkind_to_vkey, mods_to_carbon};
    use std::ffi::c_void;
    use std::sync::{Mutex, OnceLock};

    use clipvault_core::keymap::{HotkeyAction, KeyCombo};
    use tauri::AppHandle;

    // ---- Carbon 不透明类型 ----
    type OsStatus = i32;
    type EventTargetRef = *mut c_void;
    type EventHandlerRef = *mut c_void;
    type EventHandlerCallRef = *mut c_void;
    type EventRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;

    /// `EventHandlerRef` 和 `EventHotKeyRef` 都是 Carbon opaque pointer。
    /// 裸 `*mut c_void` 不是 `Sync`，但这些静态变量只在主线程访问，
    /// 仍需满足 Rust 的 `Sync` 检查。使用 newtype 手动实现 `Sync`（合理）。
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct HandlerRef(EventHandlerRef);
    unsafe impl std::marker::Sync for HandlerRef {}
    unsafe impl std::marker::Send for HandlerRef {}

    /// EventHotKeyRef 包装（用于 HOTKEYS mutex）。
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct HotkeyRef(EventHotKeyRef);
    unsafe impl std::marker::Sync for HotkeyRef {}
    unsafe impl std::marker::Send for HotkeyRef {}

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
        /// 从 EventRef 中取出 EventHotKeyID 参数。
        fn GetEventParameter(
            event: EventRef,
            name: u32, // typeEventHotKeyID
            expected: u32,
            actual: *mut u32,
            size: u32,
            used: *mut u32,
            data: *mut EventHotKeyID,
        ) -> OsStatus;
    }

    // GetEventParameter name 第四参数 type
    const TYPE_EVENTHOTKEYID: u32 = 0x484B_4944; // 'HKID'

    // ---- Carbon 常量 ----
    /// `'keyb'` —— kEventClassKeyboard。
    const K_EVENT_CLASS_KEYBOARD: u32 = 0x6b65_7962;
    /// kEventHotKeyPressed。
    const K_EVENT_HOT_KEY_PRESSED: u32 = 6;
    /// 热键签名 `'CLPV'`。
    const HOTKEY_SIGNATURE: u32 = 0x434c_5056;

    /// 主线程 run loop 中被回调使用的 AppHandle。
    static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

    /// 已注册热键表：`(Carbon ref, 热键 id, 语义 action, 当前 combo)`。
    ///
    /// **存 combo 是为了「按 action 替换」**：`register` 先注册新 combo、
    /// 成功后再注销同 action 的旧 combo，从而不存在「先清空、再注册、失败后
    /// 一个热键都不剩」的窗口（U2 code review P0：conflict 后永久失去全局热键）。
    /// 存 combo 也让「改回同一个 combo」短路成 no-op（否则 Carbon 会返回
    /// 「已注册」错误，被误报为冲突）。
    static HOTKEYS: Mutex<Vec<(HotkeyRef, EventHotKeyID, HotkeyAction, KeyCombo)>> =
        Mutex::new(Vec::new());

    /// 串行化 `register` 的整段「查重 → 注册 → 替换旧条目」。
    ///
    /// `set_hotkey` 命令在 await 之后调用 `apply_global_hotkey`，两个并发命令
    /// 可能在不同 runtime 线程同时进入，导致同一 action 注册出两条热键。
    static REGISTER_LOCK: Mutex<()> = Mutex::new(());

    /// 进程内自增 hotkey id（保证 EventHotKeyID.id 在同一进程内不重复）。
    static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

    /// 全局热键回调：路由到具体 action。
    ///
    /// Carbon 通过 `GetEventParameter(event, kEventParamEventHotKeyID, ...)` 提供 EventHotKeyID，
    /// 我们用它查找对应的 HotkeyAction。
    extern "C" fn hotkey_handler(
        _call: EventHandlerCallRef,
        _event: EventRef,
        _user: *mut c_void,
    ) -> OsStatus {
        let Some(app) = APP_HANDLE.get() else {
            return 0;
        };

        // 从 EventRef 中取出 EventHotKeyID。
        let mut hotkey_id = EventHotKeyID {
            signature: 0,
            id: 0,
        };
        let mut actual = 0u32;
        let status = unsafe {
            GetEventParameter(
                _event,
                TYPE_EVENTHOTKEYID,
                TYPE_EVENTHOTKEYID,
                &mut actual,
                std::mem::size_of::<EventHotKeyID>() as u32,
                std::ptr::null_mut(),
                &mut hotkey_id,
            )
        };
        if status != 0 {
            return status;
        }

        // 用 signature + id 查找 action。
        let hotkeys = HOTKEYS.lock().unwrap();
        for (_, id, action, _) in hotkeys.iter() {
            if id.signature == hotkey_id.signature && id.id == hotkey_id.id {
                if *action == HotkeyAction::OpenPanel {
                    crate::tray::show_panel(app);
                } else {
                    // 未来新增全局 action 时在这里处理（现在只支持 OpenPanel）。
                    eprintln!("[hotkey] 未实现的全局 action: {:?}", action);
                }
                break;
            }
        }
        0
    }

    /// 注册（或替换）某个 action 的全局热键。
    ///
    /// **替换语义**（U2 code review P0 修复）：
    /// 1. 同一 `(action, combo)` 已在注册中 → no-op 返回 `Ok`，不碰 Carbon。
    /// 2. 否则先注册新 combo；**成功之后**才注销同 action 的旧 combo。
    ///    注册失败时旧条目原样保留 —— 用户不会因一次冲突失去这个 action 的全局热键。
    ///
    /// 整段由 [`REGISTER_LOCK`] 串行化，避免并发命令为同一 action 注册出两条热键。
    pub fn register(
        app: &AppHandle,
        action: HotkeyAction,
        combo: &KeyCombo,
    ) -> Result<(), ConflictReason> {
        use super::ConflictReason;
        let _guard = REGISTER_LOCK.lock().unwrap();

        // 1. 已用同一 combo 注册过？no-op。
        {
            let hotkeys = HOTKEYS.lock().unwrap();
            if hotkeys
                .iter()
                .any(|(_, _, a, c)| *a == action && c == combo)
            {
                return Ok(());
            }
        }

        // 保存 AppHandle 供回调使用。
        let _ = APP_HANDLE.set(app.clone());

        // 翻译 KeyCombo → Carbon 虚拟键码。
        let Some(keycode) = charkind_to_vkey(combo.key) else {
            return Err(ConflictReason::Taken(format!(
                "unsupported key {:?}",
                combo.key
            )));
        };

        // 翻译 Mods → Carbon 修饰掩码（显式 match，不混用位常量）。
        let carbon_mask = mods_to_carbon(combo.mods);

        // 分配唯一 EventHotKeyID。
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let hot_key_id = EventHotKeyID {
            signature: HOTKEY_SIGNATURE,
            id,
        };

        let mut raw_ref: EventHotKeyRef = std::ptr::null_mut();
        unsafe {
            let target = GetApplicationEventTarget();

            // 只装一次处理器（首次注册时）。
            static HANDLER_REF: Mutex<Option<HandlerRef>> = Mutex::new(None);
            let needs_install = HANDLER_REF.lock().unwrap().is_none();
            if needs_install {
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
                    return Err(ConflictReason::Taken(format!(
                        "InstallEventHandler Carbon status {status}"
                    )));
                }
                let mut guard = HANDLER_REF.lock().unwrap();
                *guard = Some(HandlerRef(handler_ref));
            }

            let status =
                RegisterEventHotKey(keycode, carbon_mask, hot_key_id, target, 0, &mut raw_ref);
            if status != 0 {
                // 旧条目原样保留（本函数什么都没改）。
                return Err(ConflictReason::Taken(format!("Carbon status {status}")));
            }
        }

        // 2. 注册成功：先取出同 action 的旧条目，写入新条目，再在锁外注销旧条目。
        let old_refs: Vec<EventHotKeyRef> = {
            let mut hotkeys = HOTKEYS.lock().unwrap();
            let old: Vec<EventHotKeyRef> = hotkeys
                .iter()
                .filter(|(_, _, a, _)| *a == action)
                .map(|(r, _, _, _)| r.0)
                .collect();
            hotkeys.retain(|(_, _, a, _)| *a != action);
            hotkeys.push((HotkeyRef(raw_ref), hot_key_id, action, *combo));
            old
        };
        for old in old_refs {
            unsafe {
                UnregisterEventHotKey(old);
            }
        }

        Ok(())
    }

    /// 注销所有已注册的热键（应用退出路径）。
    pub fn unregister_all() {
        let mut hotkeys = HOTKEYS.lock().unwrap();
        for (ref_, _, _, _) in hotkeys.drain(..) {
            unsafe {
                UnregisterEventHotKey(ref_.0);
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
pub use imp::{check_accessibility, register, unregister_all};

// ---------------------------------------------------------------------------
// KeyCombo → Win32 平台翻译（U2）
// ---------------------------------------------------------------------------

/// 将 `clipvault_core::keymap::Mods` 翻译为 Win32 修饰掩码。
///
/// MOD_ALT=0x0001 / MOD_CONTROL=0x0002 / MOD_SHIFT=0x0004 / MOD_WIN=0x0008。
#[cfg(target_os = "windows")]
fn mods_to_win32(mods: clipvault_core::keymap::Mods) -> u32 {
    use clipvault_core::keymap::Mods;
    let mut mask = 0u32;
    if mods.contains(Mods::ALT) {
        mask |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_ALT.0 as u32;
    }
    if mods.contains(Mods::CTRL) {
        mask |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_CONTROL.0 as u32;
    }
    if mods.contains(Mods::SHIFT) {
        mask |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_SHIFT.0 as u32;
    }
    if mods.contains(Mods::META) {
        mask |= windows::Win32::UI::Input::KeyboardAndMouse::MOD_WIN.0 as u32;
    }
    mask
}

/// 将 `clipvault_core::keymap::CharKind` 翻译为 Win32 虚拟键码（VK_*）。
#[cfg(target_os = "windows")]
fn charkind_to_vk(kind: clipvault_core::keymap::CharKind) -> Option<u32> {
    use clipvault_core::keymap::CharKind;
    match kind {
        CharKind::Char(c) => {
            let upper = c.to_ascii_uppercase();
            match upper {
                'A'..='Z' | '0'..='9' => Some(u32::from(upper)),
                ',' => Some(0xBC), // VK_OEM_COMMA
                _ => None,
            }
        }
        CharKind::Enter => Some(0x0D),     // VK_RETURN
        CharKind::Escape => Some(0x1B),    // VK_ESCAPE
        CharKind::ArrowUp => Some(0x26),   // VK_UP
        CharKind::ArrowDown => Some(0x28), // VK_DOWN
    }
}

#[cfg(target_os = "windows")]
mod imp_win {
    use super::ConflictReason;
    use super::{charkind_to_vk, mods_to_win32};
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::sync::{Mutex, OnceLock};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use clipvault_core::keymap::{HotkeyAction, KeyCombo};
    use tauri::AppHandle;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage, WM_HOTKEY,
    };

    /// 消息泵线程状态。
    struct Pump {
        thread_id: u32,
        /// JoinHandle 在 ensure_pump 中从父线程写入。
        handle: Option<JoinHandle<()>>,
        /// 注册指令 channel（发送方在主线程）。
        tx: mpsc::Sender<PumpMsg>,
    }

    static PUMP: Mutex<Option<Pump>> = Mutex::new(None);
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// 主线程侧「当前应当注册的热键」真值表：`action → (热键 id, combo)`。
    ///
    /// 与 macOS 的 `HOTKEYS` 同角色 —— 让 `register` 能按 action 替换
    /// （先注册新 combo、成功后再注销旧 id），而不是「先 Quit 整个 pump、
    /// 再注册」。后者在注册失败时会让用户永久失去全局热键。
    static REGISTERED: Mutex<Option<HashMap<HotkeyAction, (i32, KeyCombo)>>> = Mutex::new(None);

    /// 串行化 `register` 的整段「查重 → 注册 → 注销旧 id」。
    static REGISTER_LOCK: Mutex<()> = Mutex::new(());

    /// 热键 id 自增计数器（进程内唯一）。
    static NEXT_ID: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

    /// 发送给消息泵的消息。
    enum PumpMsg {
        /// 注册热键。
        Register {
            id: i32,
            combo: KeyCombo,
            action: HotkeyAction,
            reply: mpsc::Sender<Result<(), ConflictReason>>,
        },
        /// 注销热键。
        Unregister { id: i32, reply: mpsc::Sender<()> },
        /// 退出消息泵。
        Quit,
    }

    const STARTUP_TIMEOUT: Duration = Duration::from_millis(500);
    const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

    /// 确保消息泵线程已启动（如尚未启动则 spawn 并等待就绪）。
    fn ensure_pump(app: &AppHandle) -> Result<(), ConflictReason> {
        if STARTED.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(()); // 已启动
        }

        // channel 用来让子线程回报 thread_id。
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();

        let spawned = std::thread::Builder::new()
            .name("clipvault-hotkey".into())
            .spawn({
                let app = app.clone();
                move || run_pump(app, ready_tx)
            });

        let handle = match spawned {
            Ok(h) => h,
            Err(e) => {
                STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
                return Err(ConflictReason::Taken(format!("thread spawn failed: {e}")));
            }
        };

        match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(thread_id)) => {
                // 子线程已就绪，tx 已写入 PUMP；我们把 handle 也写入。
                let mut guard = PUMP.lock().expect("hotkey pump mutex poisoned");
                if let Some(ref mut p) = *guard {
                    p.thread_id = thread_id;
                    p.handle = handle;
                    return Ok(());
                }
                STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
                Err(ConflictReason::Taken("pump not initialized".to_string()))
            }
            Ok(Err(detail)) => {
                STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
                Err(ConflictReason::Taken(detail))
            }
            Err(_) => {
                STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
                Err(ConflictReason::Taken("pump thread timeout".to_string()))
            }
        }
    }

    /// 注册（或替换）某个 action 的全局热键（通道式）。
    ///
    /// **替换语义**（与 macOS 侧一致）：
    /// 1. 同一 `(action, combo)` 已注册 → no-op `Ok`。
    /// 2. 否则先注册新 id；**成功之后**才注销同 action 的旧 id ——
    ///    注册失败时旧热键原样保留。
    pub fn register(
        app: &AppHandle,
        action: HotkeyAction,
        combo: &KeyCombo,
    ) -> Result<(), ConflictReason> {
        let _guard = REGISTER_LOCK.lock().unwrap();

        // 1. 已用同一 combo 注册过？no-op。
        {
            let registered = REGISTERED.lock().unwrap();
            if let Some(map) = registered.as_ref()
                && let Some((_, existing)) = map.get(&action)
                && existing == combo
            {
                return Ok(());
            }
        }

        // 2. 确保 pump 线程在运行。
        ensure_pump(app)?;

        // 3. 注册新 id。
        let new_id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        {
            let pump_guard = PUMP.lock().expect("hotkey pump mutex poisoned");
            let pump = pump_guard
                .as_ref()
                .ok_or_else(|| ConflictReason::Taken("pump not initialized".to_string()))?;
            let (reply_tx, reply_rx) = mpsc::channel();
            pump.tx
                .send(PumpMsg::Register {
                    id: new_id,
                    combo: combo.clone(),
                    action,
                    reply: reply_tx,
                })
                .map_err(|_| ConflictReason::Taken("pump channel closed".to_string()))?;
            drop(pump_guard);
            reply_rx
                .recv_timeout(REPLY_TIMEOUT)
                .map_err(|_| ConflictReason::Taken("pump reply timeout".to_string()))??;
        }

        // 4. 注册成功：注销同 action 的旧 id（若有），再更新真值表。
        let old_id = {
            let mut registered = REGISTERED.lock().unwrap();
            let map = registered.get_or_insert_with(HashMap::new);
            map.insert(action, (new_id, *combo)).map(|(id, _)| id)
        };
        if let Some(old_id) = old_id {
            unregister_id(old_id);
        }

        Ok(())
    }

    /// 注销指定 id 的热键（fire-and-forget，等待 pump 确认）。
    fn unregister_id(id: i32) {
        let pump_guard = PUMP.lock().expect("hotkey pump mutex poisoned");
        if let Some(pump) = pump_guard.as_ref() {
            let (reply_tx, reply_rx) = mpsc::channel();
            if pump
                .tx
                .send(PumpMsg::Unregister {
                    id,
                    reply: reply_tx,
                })
                .is_ok()
            {
                drop(pump_guard);
                let _ = reply_rx.recv_timeout(REPLY_TIMEOUT);
            }
        }
    }

    /// 注销所有已注册热键并停掉消息泵（应用退出路径）。
    pub fn unregister_all() {
        let pump_guard = PUMP.lock().expect("hotkey pump mutex poisoned");
        if let Some(ref pump) = *pump_guard {
            let _ = pump.tx.send(PumpMsg::Quit);
        }
        drop(pump_guard);

        // join pump thread.
        let mut pump_guard = PUMP.lock().expect("hotkey pump mutex poisoned");
        if let Some(p) = pump_guard.take() {
            if let Some(h) = p.handle {
                let _ = h.join();
            }
        }
        STARTED.store(false, std::sync::atomic::Ordering::SeqCst);
        *REGISTERED.lock().unwrap() = None;
    }

    /// 消息泵主体：在子线程中运行，接收主线程发来的 PumpMsg 并执行。
    fn run_pump(app: AppHandle, ready_tx: mpsc::Sender<Result<u32, String>>) {
        // channel 用来接收主线程的 PumpMsg。
        let (tx, rx) = mpsc::channel::<PumpMsg>();

        // id → HotkeyAction 路由表（在 pump 线程内维护）。
        let mut id_map: HashMap<i32, HotkeyAction> = HashMap::new();

        // 把 tx 写入 PUMP（主线程在 ensure_pump 中等待这个）。
        {
            let mut guard = PUMP.lock().expect("hotkey pump mutex poisoned");
            *guard = Some(Pump {
                thread_id: 0, // filled by ensure_pump
                handle: None, // filled by ensure_pump
                tx,
            });
        }

        // 回报 thread_id。
        let thread_id = unsafe { GetCurrentThreadId() };
        if ready_tx.send(Ok(thread_id)).is_err() {
            // 主线程已不等了。
            return;
        }

        // 主消息循环。
        let mut msg = MSG::default();
        loop {
            // 先 try_recv PumpMsg（非阻塞）。
            match rx.try_recv() {
                Ok(PumpMsg::Register {
                    id,
                    combo,
                    action,
                    reply,
                }) => {
                    let vk = match charkind_to_vk(combo.key) {
                        Some(v) => v,
                        None => {
                            let _ = reply.send(Err(ConflictReason::Taken(format!(
                                "unsupported key {:?}",
                                combo.key
                            ))));
                            continue;
                        }
                    };
                    let mask = mods_to_win32(combo.mods);
                    unsafe {
                        if RegisterHotKey(None, id, mask, vk).is_err() {
                            let _ = reply.send(Err(ConflictReason::Taken(format!(
                                "RegisterHotKey failed for id {id}"
                            ))));
                            continue;
                        }
                    }
                    id_map.insert(id, action);
                    let _ = reply.send(Ok(()));
                }
                Ok(PumpMsg::Unregister { id, reply }) => {
                    unsafe {
                        let _ = UnregisterHotKey(None, id);
                    }
                    id_map.remove(&id);
                    let _ = reply.send(());
                }
                Ok(PumpMsg::Quit) => {
                    // 退出前注销所有热键。
                    for id in id_map.keys() {
                        unsafe {
                            let _ = UnregisterHotKey(None, *id);
                        }
                    }
                    unsafe {
                        PostQuitMessage(0);
                    }
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => break,
            }

            // GetMessageW。
            let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if got.0 <= 0 {
                // WM_QUIT 或错误 — 退出前注销所有。
                for id in id_map.keys() {
                    unsafe {
                        let _ = UnregisterHotKey(None, *id);
                    }
                }
                break;
            }
            match msg.message {
                WM_HOTKEY => {
                    let id = msg.wParam.0 as i32;
                    if let Some(action) = id_map.get(&id) {
                        if *action == HotkeyAction::OpenPanel {
                            crate::tray::show_panel(&app);
                        }
                    }
                }
                _ => {}
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Windows `SendInput` 不需要系统授权。
    pub fn check_accessibility() -> bool {
        true
    }
}

#[cfg(target_os = "windows")]
pub use imp_win::{check_accessibility, register, unregister_all};

// ---- 其余平台：no-op 降级，保证跨平台可编译 ----

/// 其余平台：热键注册（no-op）。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn register(
    _app: &tauri::AppHandle,
    _action: clipvault_core::keymap::HotkeyAction,
    _combo: &clipvault_core::keymap::KeyCombo,
) -> Result<(), ConflictReason> {
    Ok(())
}

/// 其余平台：注销所有热键（no-op）。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn unregister_all() {}

/// 其余平台：无 Accessibility 概念，视为已授权。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn check_accessibility() -> bool {
    true
}

// ---------------------------------------------------------------------------
// hotkey_test：翻译表 + ConflictReason 测试（U2）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use clipvault_core::keymap::{CharKind, KeyCombo, Mods};

    // ---- Carbon 翻译表测试（macOS only）----
    #[cfg(target_os = "macos")]
    #[test]
    fn carbon_mods_translation_meta_alt_v() {
        // ⌥⌘V = Mods::META|Mods::ALT, key = 'v'
        use crate::hotkey::mods_to_carbon;
        let mods = Mods::META | Mods::ALT;
        let mask = mods_to_carbon(mods);
        // cmdKey=0x0100 | optionKey=0x0800 = 0x0900
        assert_eq!(mask, 0x0900, "Meta|Alt → cmdKey|optionKey");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn carbon_mods_translation_cmd_shift_v() {
        use crate::hotkey::mods_to_carbon;
        let mods = Mods::META | Mods::SHIFT;
        let mask = mods_to_carbon(mods);
        // cmdKey=0x0100 | shiftKey=0x0200 = 0x0300
        assert_eq!(mask, 0x0300, "Meta|Shift → cmdKey|shiftKey");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn carbon_keycode_v() {
        use crate::hotkey::charkind_to_vkey;
        // kVK_ANSI_V = 0x09, A = 0x00, Z = 0x06, 0 = 0x20
        assert_eq!(charkind_to_vkey(CharKind::Char('v')), Some(0x09));
        assert_eq!(charkind_to_vkey(CharKind::Char('V')), Some(0x09)); // upper normalised
        assert_eq!(charkind_to_vkey(CharKind::Char('a')), Some(0x00));
        assert_eq!(charkind_to_vkey(CharKind::Char('z')), Some(0x06));
        assert_eq!(charkind_to_vkey(CharKind::Char('0')), Some(0x1D)); // kVK_ANSI_0
        // Enter, Escape, ArrowUp, ArrowDown
        assert_eq!(charkind_to_vkey(CharKind::Enter), Some(0x24)); // kVK_Return
        assert_eq!(charkind_to_vkey(CharKind::Escape), Some(0x35)); // kVK_Escape
        assert_eq!(charkind_to_vkey(CharKind::ArrowUp), Some(0x7E)); // kVK_UpArrow
        assert_eq!(charkind_to_vkey(CharKind::ArrowDown), Some(0x7D)); // kVK_DownArrow
        assert_eq!(charkind_to_vkey(CharKind::Comma), Some(0x2B)); // kVK_ANSI_Comma
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn carbon_mods_exhaustive() {
        use crate::hotkey::mods_to_carbon;
        // 8 modifier combinations × 4 single modifiers = 12 cases
        let cases = [
            (Mods::empty(), 0u32),
            (Mods::META, 0x0100),
            (Mods::SHIFT, 0x0200),
            (Mods::ALT, 0x0800),
            (Mods::CTRL, 0x1000),
            (Mods::META | Mods::SHIFT, 0x0300),
            (Mods::META | Mods::ALT, 0x0900),
            (Mods::META | Mods::CTRL, 0x1100),
            (Mods::SHIFT | Mods::ALT, 0x0A00),
            (Mods::SHIFT | Mods::CTRL, 0x1200),
            (Mods::ALT | Mods::CTRL, 0x1800),
            (Mods::META | Mods::SHIFT | Mods::ALT, 0x0B00),
            (Mods::META | Mods::SHIFT | Mods::CTRL, 0x1300),
            (Mods::META | Mods::ALT | Mods::CTRL, 0x1900),
            (Mods::SHIFT | Mods::ALT | Mods::CTRL, 0x1A00),
            (Mods::META | Mods::SHIFT | Mods::ALT | Mods::CTRL, 0x1B00),
        ];
        for (mods, expected) in cases {
            let mask = mods_to_carbon(mods);
            assert_eq!(mask, expected, "mods={:?} → mask={:#06x}", mods, expected);
        }
    }

    // ---- Windows 翻译表测试（Windows only）----
    #[cfg(target_os = "windows")]
    #[test]
    fn win32_mods_translation_meta_alt_v() {
        use crate::hotkey::mods_to_win32;
        let mods = Mods::META | Mods::ALT;
        // MOD_WIN=0x0008 | MOD_ALT=0x0001 = 0x0009
        assert_eq!(mods_to_win32(mods), 0x0009);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn win32_mods_translation_ctrl_alt_v() {
        use crate::hotkey::mods_to_win32;
        let mods = Mods::CTRL | Mods::ALT;
        // MOD_CONTROL=0x0002 | MOD_ALT=0x0001 = 0x0003 (标准 Ctrl+Alt+V)
        assert_eq!(mods_to_win32(mods), 0x0003);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn win32_vk_translation() {
        use crate::hotkey::charkind_to_vk;
        // A-Z → VK_A (0x41) .. VK_Z (0x5A)
        assert_eq!(charkind_to_vk(CharKind::Char('v')), Some(0x56)); // VK_V = 0x56
        assert_eq!(charkind_to_vk(CharKind::Char('V')), Some(0x56));
        assert_eq!(charkind_to_vk(CharKind::Char('a')), Some(0x41)); // VK_A = 0x41
        assert_eq!(charkind_to_vk(CharKind::Char('0')), Some(0x30)); // VK_0 = 0x30
        assert_eq!(charkind_to_vk(CharKind::Char('9')), Some(0x39)); // VK_9 = 0x39
        assert_eq!(charkind_to_vk(CharKind::Enter), Some(0x0D)); // VK_RETURN
        assert_eq!(charkind_to_vk(CharKind::Escape), Some(0x1B)); // VK_ESCAPE
        assert_eq!(charkind_to_vk(CharKind::ArrowUp), Some(0x26)); // VK_UP
        assert_eq!(charkind_to_vk(CharKind::ArrowDown), Some(0x28)); // VK_DOWN
        assert_eq!(charkind_to_vk(CharKind::Comma), Some(0xBC)); // VK_OEM_COMMA
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn win32_mods_exhaustive() {
        use crate::hotkey::mods_to_win32;
        let cases = [
            (Mods::empty(), 0u32),
            (Mods::META, 0x0008),               // MOD_WIN
            (Mods::SHIFT, 0x0004),              // MOD_SHIFT
            (Mods::ALT, 0x0001),                // MOD_ALT
            (Mods::CTRL, 0x0002),               // MOD_CONTROL
            (Mods::META | Mods::SHIFT, 0x000C), // MOD_WIN | MOD_SHIFT
            (Mods::META | Mods::ALT, 0x0009),   // MOD_WIN | MOD_ALT
            (Mods::CTRL | Mods::ALT, 0x0003),   // MOD_CONTROL | MOD_ALT
            (Mods::SHIFT | Mods::ALT, 0x0005),
            (Mods::CTRL | Mods::SHIFT, 0x0006),
            (Mods::META | Mods::CTRL, 0x000A),
            (Mods::META | Mods::SHIFT | Mods::ALT, 0x000D),
            (Mods::CTRL | Mods::SHIFT | Mods::ALT, 0x0007),
        ];
        for (mods, expected) in cases {
            let mask = mods_to_win32(mods);
            assert_eq!(mask, expected, "mods={:?} → mask={:#06x}", mods, expected);
        }
    }

    // ---- ConflictReason Display + Debug ----
    #[test]
    fn conflict_reason_display() {
        use crate::hotkey::ConflictReason;
        let cr = ConflictReason::Taken("Carbon status 1008".to_string());
        let display = format!("{}", cr);
        assert!(display.contains("已被占用"));
        assert!(display.contains("Carbon status 1008"));
    }

    #[test]
    fn conflict_reason_debug() {
        use crate::hotkey::ConflictReason;
        let cr = ConflictReason::Taken("test".to_string());
        let debug = format!("{:?}", cr);
        assert!(debug.contains("Taken"));
    }

    // ---- Mods 位标志 NOT 直接混用于平台掩码（DEBUG note）----
    // 这是一个提醒注释：Mods::META = 0x01 != Carbon cmdKey = 0x0100
    // mods_to_carbon / mods_to_win32 函数必须显式 match，不能直接用 .bits()
    #[test]
    fn mods_bitflag_not_directly_usable_as_carbon_mask() {
        // 验证 Mods::META.bits() = 0x01，而 Carbon cmdKey = 0x0100
        assert_ne!(
            u16::from(Mods::META.bits()),
            0x0100u16,
            "Mods::META bits must NOT equal Carbon cmdKey — translation is required"
        );
        assert_ne!(
            u16::from(Mods::SHIFT.bits()),
            0x0200u16,
            "Mods::SHIFT bits must NOT equal Carbon shiftKey"
        );
        assert_ne!(
            u16::from(Mods::ALT.bits()),
            0x0800u16,
            "Mods::ALT bits must NOT equal Carbon optionKey"
        );
        assert_ne!(
            u16::from(Mods::CTRL.bits()),
            0x1000u16,
            "Mods::CTRL bits must NOT equal Carbon controlKey"
        );
    }

    // ---- apply_global_hotkey 三路径覆盖（转换层 + ConflictReason）----
    // 注意：端到端的成功 / 失败路径依赖 OS 层的 RegisterEventHotKey / RegisterHotKey，
    // 无法在 macOS 上用 mock 测试 Windows 路径。以下测试覆盖转换正确性 + ConflictReason。
    // 端到端 smoke 在 plan 的真机 AE1 验收中覆盖。

    #[test]
    fn keycombo_default_openpanel_translates() {
        // 验证默认 OpenPanel combo 可以被正确翻译
        let combo = KeyCombo {
            mods: Mods::META | Mods::ALT,
            key: CharKind::Char('v'),
        };
        #[cfg(target_os = "macos")]
        {
            use crate::hotkey::{charkind_to_vkey, mods_to_carbon};
            assert_eq!(charkind_to_vkey(combo.key), Some(0x09)); // kVK_ANSI_V
            assert_eq!(mods_to_carbon(combo.mods), 0x0900); // cmdKey|optionKey
        }
        #[cfg(target_os = "windows")]
        {
            use crate::hotkey::{charkind_to_vk, mods_to_win32};
            assert_eq!(charkind_to_vk(combo.key), Some(0x56)); // VK_V
            assert_eq!(mods_to_win32(combo.mods), 0x0009); // MOD_WIN|MOD_ALT
        }
    }
}
