//! 单实例守卫（Windows）。
//!
//! **为什么需要。** Windows 上「再点一次应用图标」就是再启动一个进程：每个进程都会
//! [`crate::tray::build_tray`] 往通知区加一个图标、各起一个 [`crate::watcher::Watcher`]
//! 写同一个库、并各自去抢同一个全局热键 —— 用户看到的就是「多点了两次，状态栏多出两个
//! 图标」。macOS 侧没有这个问题：`.app` 走 LaunchServices，重复打开只会激活已在跑的
//! 实例，所以本模块对 Windows 之外不生效（沿用仓库的平台 `#[cfg]` 惯例）。
//!
//! **做法。** 在启动早期（`setup` 里 `log::init` 之后、`Storage::open` 之前 —— 次实例
//! 要做的每一件事都会和主实例抢资源）用 `CreateMutexW` 抢一个以应用 identifier 命名的
//! 互斥体；抢不到说明已有实例在跑 —— 这时通过一个命名事件请对方唤出面板，自己立刻退出。
//! 之所以在 `log::init` 之后，是为了让「点了图标却没反应」在诊断日志里能读到答案。
//!
//! 名字用 `Local\` 命名空间而不是 `Global\`：剪贴板本身是 per-session 的，快速用户切换
//! 下另一个登录会话需要自己的实例。
//!
//! 拿不到内核对象（权限异常之类的环境问题）时记一行 WARN、并当作主实例继续跑 ——
//! 单实例是体验优化，不能成为启动的硬门槛（与「日志初始化失败不阻塞启动」同一原则）。

/// 单实例互斥体名：`Local\ClipVault.<identifier>.instance`。
pub fn instance_name(identifier: &str) -> String {
    format!("Local\\ClipVault.{identifier}.instance")
}

/// 「又点了一次图标」的事件名：主实例等它，次实例 set 它。
pub fn activate_event_name(identifier: &str) -> String {
    format!("Local\\ClipVault.{identifier}.activate")
}

#[cfg(target_os = "windows")]
mod imp_win {
    use super::{activate_event_name, instance_name};
    use std::sync::Mutex;

    use tauri::AppHandle;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForSingleObject,
    };
    use windows::core::HSTRING;

    /// 本次启动的结论。
    pub enum Startup {
        /// 本进程是主实例：互斥体已持有，可再调 [`spawn_activation_listener`] 接管
        /// 「又点了一次图标」的信号。
        Primary,
        /// 已有实例在跑：已请它唤出面板，调用方应立即退出。
        Secondary,
    }

    /// `HANDLE`（`*mut c_void`）不是 `Send`/`Sync`，但这两个句柄的用途就是「持有到进程
    /// 结束、谁都不会去动它」—— 与 macOS 侧包装 Carbon 句柄同一手法。
    #[derive(Clone, Copy)]
    struct SyncHandle(HANDLE);
    unsafe impl Send for SyncHandle {}
    unsafe impl Sync for SyncHandle {}

    impl SyncHandle {
        /// 等这个事件（自动复位）被 set 一次；`false` 表示等待本身失败。
        ///
        /// 做成方法而不是让调用方读 `.0`，是为了让闭包整体捕获 `SyncHandle`（`Send`）、
        /// 而不是按字段捕获内部的 `HANDLE`（`*mut c_void`，不是 `Send`）。
        fn wait_forever(&self) -> bool {
            unsafe { WaitForSingleObject(self.0, INFINITE).0 == WAIT_OBJECT_0.0 }
        }
    }

    /// 互斥体句柄：**必须在进程存活期间一直持有**（退出即由系统释放，下一个实例才能
    /// 成为主实例）。
    static MUTEX: Mutex<Option<SyncHandle>> = Mutex::new(None);
    /// 「又点了一次图标」的事件句柄（自动复位）：主实例等它。
    static ACTIVATE: Mutex<Option<SyncHandle>> = Mutex::new(None);

    /// 抢单实例锁。语义见模块文档。
    pub fn acquire(identifier: &str) -> Startup {
        let mutex_name = HSTRING::from(instance_name(identifier));
        let event_name = HSTRING::from(activate_event_name(identifier));

        // 1. 抢互斥体，并**紧接着**读 last error：同名对象已存在时 `CreateMutexW` 仍然
        //    成功返回，只是把 last error 置为 ERROR_ALREADY_EXISTS —— 中间夹任何其它
        //    API 调用都会覆盖它。
        let mutex = unsafe { CreateMutexW(None, false, &mutex_name) };
        let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

        if already_running {
            // 2. 次实例：请已有实例唤出面板，然后自己退出。
            match unsafe { CreateEventW(None, false, false, &event_name) } {
                Ok(event) => unsafe {
                    let _ = SetEvent(event);
                    let _ = CloseHandle(event);
                },
                Err(e) => ::log::warn!(
                    target: "single_instance",
                    "打开二次唤醒事件失败（二次点击图标不会唤出面板）：{e}"
                ),
            }
            if let Ok(m) = mutex {
                unsafe {
                    let _ = CloseHandle(m);
                }
            }
            return Startup::Secondary;
        }

        // 3. 主实例：两个句柄各自存进静态量，活到进程退出。两者独立降级 ——
        //    互斥体失败＝本次没有单实例保护，事件失败＝二次启动不会唤出面板，
        //    都不该拦住启动。
        match mutex {
            Ok(m) => *MUTEX.lock().unwrap() = Some(SyncHandle(m)),
            Err(e) => ::log::warn!(
                target: "single_instance",
                "单实例互斥体创建失败，本次运行没有单实例保护：{e}"
            ),
        }
        match unsafe { CreateEventW(None, false, false, &event_name) } {
            Ok(e) => *ACTIVATE.lock().unwrap() = Some(SyncHandle(e)),
            Err(e) => ::log::warn!(
                target: "single_instance",
                "二次唤醒事件创建失败，二次点击图标不会唤出面板：{e}"
            ),
        }
        Startup::Primary
    }

    /// 主实例专用：起一条常驻线程等「又点了一次图标」，收到就唤出面板。
    ///
    /// 线程没有退出条件：它唯一的工作就是等一个自动复位事件，进程结束时随之消失
    /// （Tauri 没有、也不该有给它 join 的点）。
    pub fn spawn_activation_listener(app: AppHandle) {
        let Some(event) = ACTIVATE.lock().unwrap().as_ref().copied() else {
            // 降级路径：没有事件对象，二次启动只会静默退出（不唤出面板）。
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("clipvault-activate".into())
            .spawn(move || {
                loop {
                    if !event.wait_forever() {
                        // 等待失败不是「又点了一次」。继续转只会变成空转线程
                        // 并反复唤出面板，所以退出这条监听。
                        ::log::warn!(
                            target: "single_instance",
                            "二次唤醒事件等待失败，监听线程退出"
                        );
                        break;
                    }
                    crate::panel::show_panel(&app);
                }
            });
        if let Err(e) = spawned {
            ::log::warn!(
                target: "single_instance",
                "二次唤醒线程启动失败，二次点击图标不会唤出面板：{e}"
            );
        }
    }
}

#[cfg(target_os = "windows")]
pub use imp_win::{Startup, acquire, spawn_activation_listener};

#[cfg(test)]
mod tests {
    use super::{activate_event_name, instance_name};

    #[test]
    fn names_are_session_local_and_scoped_to_identifier() {
        // Local\ 命名空间（per-session，与剪贴板同域）；名字里带上 identifier，
        // 不同应用（或改过 bundle id 的构建）不会互相抢锁。
        assert_eq!(
            instance_name("com.clipvault.app"),
            "Local\\ClipVault.com.clipvault.app.instance"
        );
        assert_eq!(
            activate_event_name("com.clipvault.app"),
            "Local\\ClipVault.com.clipvault.app.activate"
        );
        assert_ne!(
            instance_name("com.clipvault.app"),
            instance_name("com.other.app")
        );
        assert_ne!(
            instance_name("com.clipvault.app"),
            activate_event_name("com.clipvault.app")
        );
    }
}

/// 单实例判定的回归（Windows only）：同一进程按名字再抢一次互斥体，内核同样报
/// `ERROR_ALREADY_EXISTS` —— 走的正是「又开了一个进程」那条分支。
#[cfg(all(test, target_os = "windows"))]
mod win_tests {
    use super::{Startup, acquire};

    /// 测试专属 identifier：避免与真机上正在跑的实例抢同一个名字。
    const TEST_ID: &str = "com.clipvault.test.single-instance";

    #[test]
    fn second_acquire_in_same_process_is_secondary() {
        assert!(matches!(acquire(TEST_ID), Startup::Primary));
        assert!(matches!(acquire(TEST_ID), Startup::Secondary));
    }
}
