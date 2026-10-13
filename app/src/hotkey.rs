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
//! **Windows 侧**（U2）用 `RegisterHotKey` 注册 `Ctrl+Alt+V`，并为本热键单开一条
//! 消息泵线程——`RegisterHotKey` 把热键绑定到**调用线程的消息队列**，没有 `GetMessageW`
//! 循环它就不生效。同样不引入任何 shortcut 插件（KTD2 的跨平台延续）。
//!
//! 其余平台整体降级为 no-op（`register` 什么都不做，`check_accessibility` 返回 `true`）。

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tauri::AppHandle;

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

    /// Carbon 热键回调：唤出主面板（F2）。
    ///
    /// 统一走 [`crate::tray::show_panel`]：那里保证「**先**记录回贴目标、**再**
    /// 显示面板」的顺序——面板一旦聚焦，前台应用就变成 ClipVault 自己，目标
    /// App 再也读不回来（`pasteback::capture_target_app`）。热键与托盘共用同一
    /// 入口也避免两条路径的顺序漂移。
    extern "C" fn hotkey_handler(
        _call: EventHandlerCallRef,
        _event: EventRef,
        _user: *mut c_void,
    ) -> OsStatus {
        if let Some(app) = APP_HANDLE.get() {
            crate::tray::show_panel(app);
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

// ================================================================
// Windows：RegisterHotKey + 专用线程消息泵（U2 / KTD11）
// ================================================================

/// Windows 全局热键（`Ctrl+Alt+V`）实现。
///
/// **为什么必须是专用线程。** `RegisterHotKey` 把热键绑定到**调用它的那个线程**的
/// 消息队列（`hwnd` 传 `NULL` 时绑的是线程队列而不是窗口队列），命中后系统往该队列
/// 投一条 `WM_HOTKEY`。所以"有一条线程在跑 `GetMessageW` 循环"不是可选优化，而是这个
/// 热键能否生效的前提。
///
/// **为什么不接进 Tauri 的主线程事件循环。** 那需要往 tao 的消息循环里插钩子，属于侵入
/// Tauri 内部结构；专用线程自成一体，生命周期清晰。回调里调 [`crate::tray::show_panel`]
/// 是跨线程安全的：Tauri 的窗口方法内部会把动作转发到事件循环线程。
///
/// **为什么卸载也要回到这条线程。** MSDN 要求 `UnregisterHotKey` 与注册调用在同一个
/// 线程上。因此 `unregister()` 不自己动手注销，而是用 `PostThreadMessageW` 往那条线程的
/// 队列投一条自定义消息，由它自己注销并 `PostQuitMessage` 退出。
#[cfg(target_os = "windows")]
mod imp_win {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use tauri::AppHandle;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, RegisterHotKey, UnregisterHotKey, VK_V,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PostQuitMessage, PostThreadMessageW, TranslateMessage,
        WM_APP, WM_HOTKEY,
    };

    /// 热键 id。同一个线程内唯一即可，本进程只注册这一个键。
    const HOTKEY_ID: i32 = 1;
    /// "请退出"线程序消息。取 `WM_APP` 起的一个自定义值，避开系统消息号段。
    const WM_APP_QUIT: u32 = WM_APP + 1;
    /// 等子线程回报注册结果的上限。正常只需几百微秒；超时说明消息泵没能起来。
    const STARTUP_TIMEOUT: Duration = Duration::from_millis(500);

    /// 消息泵线程：线程 id（用于投递退出消息）+ 句柄（用于退出时确实等它结束）。
    struct Pump {
        thread_id: u32,
        handle: JoinHandle<()>,
    }

    static PUMP: Mutex<Option<Pump>> = Mutex::new(None);
    /// 消息泵只 spawn 一次（首个调用者胜出）。
    ///
    /// 注册失败时**不复位**它是刻意的:热键被别的程序占用时,唯一出路是关掉那个程序再
    /// 重启 ClipVault —— 运行期没有可重试的路径(热键本身不触发,没有回调能拿来重试)。
    static STARTED: AtomicBool = AtomicBool::new(false);

    /// 注册 `Ctrl+Alt+V`。重复调用是幂等的。
    pub fn register(app: &AppHandle) {
        // `swap` 的返回值做"我是不是第一个调用者"的判定,避免"先 load 再 store"的竞态窗口。
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }

        // 线程 id 只能在子线程里用 `GetCurrentThreadId` 拿到，`JoinHandle` 只有父线程
        // 才有，所以让子线程把 id 通过 channel 报回来，父线程负责把两者凑成 `Pump`。
        let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();
        let app = app.clone();
        let spawned = std::thread::Builder::new()
            .name("clipvault-hotkey".into())
            .spawn(move || run_pump(app, &tx));

        let handle = match spawned {
            Ok(handle) => handle,
            Err(e) => {
                eprintln!("clipvault: 全局热键线程创建失败：{e}");
                return;
            }
        };

        match rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(thread_id)) => {
                *PUMP.lock().expect("hotkey pump mutex poisoned") =
                    Some(Pump { thread_id, handle });
            }
            Ok(Err(detail)) => {
                // 注册失败（多半是 Ctrl+Alt+V 已被别的程序占用）：线程已自行返回，
                // 这里只报告。`handle` 直接 drop —— JoinHandle 的 drop 只是 detach，
                // 而线程此时已经结束。
                eprintln!("clipvault: 注册全局热键 Ctrl+Alt+V 失败：{detail}");
            }
            Err(_) => {
                eprintln!("clipvault: 全局热键线程超时未就绪，Ctrl+Alt+V 不可用");
            }
        }
    }

    /// 消息泵主体：注册热键 → 回报线程 id → 循环分发直到收到退出消息。
    fn run_pump(app: AppHandle, ready: &std::sync::mpsc::Sender<Result<u32, String>>) {
        let thread_id = unsafe { GetCurrentThreadId() };
        // hwnd 传 NULL：把热键绑到本线程的队列。
        if let Err(e) =
            unsafe { RegisterHotKey(None, HOTKEY_ID, MOD_CONTROL | MOD_ALT, VK_V.0 as u32) }
        {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
        if ready.send(Ok(thread_id)).is_err() {
            // 父线程已经不等了（超时路径）。别把热键留在一条没人管的线程上。
            unsafe {
                let _ = UnregisterHotKey(None, HOTKEY_ID);
            }
            return;
        }

        let mut msg = MSG::default();
        loop {
            // `GetMessageW` 的返回值是 BOOL：0 = 收到 WM_QUIT，-1 = 出错，两者都该收摊。
            let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if got.0 <= 0 {
                break;
            }
            match msg.message {
                WM_HOTKEY => {
                    // 与 macOS 侧共用同一入口，保证「先记录回贴目标、再显示面板」的顺序。
                    crate::tray::show_panel(&app);
                }
                WM_APP_QUIT => {
                    // 回到注册它的这条线程来注销（MSDN 的硬要求），然后结束消息循环。
                    unsafe {
                        let _ = UnregisterHotKey(None, HOTKEY_ID);
                        PostQuitMessage(0);
                    }
                }
                _ => {}
            }
            // 线程消息（hwnd 为 NULL）经 `DispatchMessageW` 不会被派发到任何窗口过程，
            // 但保留这一步可让窗口消息走正常路径 —— 目前本线程没有窗口，属于兜底。
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// 卸载热键并等消息泵线程退出。
    pub fn unregister() {
        let pump = PUMP.lock().expect("hotkey pump mutex poisoned").take();
        let Some(pump) = pump else {
            return; // 没注册过（或已卸载）。
        };

        // 投递失败说明线程已经不在——热键随线程结束一并释放，join 会立刻返回。
        // 投递成功时线程会注销热键并退出，join 同样很快返回：它在阻塞的 `GetMessageW`
        // 上被这条消息唤醒，没有别的等待点。因此这里无条件 join 是安全的，
        // 而且比"只投消息不等待"更干净——后者可能在进程退出过程中被打断。
        unsafe {
            let _ = PostThreadMessageW(pump.thread_id, WM_APP_QUIT, WPARAM(0), LPARAM(0));
        }
        let _ = pump.handle.join();
    }

    /// Windows 没有"辅助功能授权"这一前置门槛：`SendInput` 对同权限级别的进程直接生效，
    /// 不需要用户去系统设置里勾选任何开关（需要管理员权限的目标进程是另一回事，
    /// 那属于 UIPI 拦截，归回贴的降级路径处理）。
    ///
    /// 恒返回 `true` 是有意的：前端据此不显示授权横幅、`no_accessibility` 降级分支
    /// 永不触发（RW8）。
    pub fn check_accessibility() -> bool {
        true
    }
}

#[cfg(target_os = "windows")]
pub use imp_win::{check_accessibility, register, unregister};

// ---- 其余平台：no-op 降级，保证跨平台可编译 ----

/// 其余平台：不注册任何全局热键。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn register(_app: &tauri::AppHandle) {}

/// 其余平台：无热键可卸载。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn unregister() {}

/// 其余平台：无 Accessibility 概念，视为已授权。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn check_accessibility() -> bool {
    true
}
