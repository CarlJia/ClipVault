//! IPC 命令注册表守卫（U7 回归）+ 命令线程亲和性守卫。
//!
//! `commands.rs` 里带 `#[tauri::command]` 的函数必须同时在 `main.rs` 的
//! `tauri::generate_handler!` 中注册；漏注册时前端 `invoke` 会直接收到
//! `command xxx not found`（U7 的 `open_url` 就这么漏掉了）。
//!
//! 同一批注册表上还有第二条契约：**会阻塞的命令必须是 `async`**（见
//! [`credential_store_commands_are_async`]）。
//!
//! lib target 的测试都绕过 IPC 直接调函数，两条都抓不到，所以这里用
//! `include_str!` 静态比对源码 —— 与 `watcher_test.rs` 的 `spawn_blocking`
//! 守卫同形：线程亲和性没有可注入的 seam，只能静态钉住。
//!
//! 只守「声明了却没注册」这一个方向。反方向（注册了却没声明）由
//! `generate_handler!` 自己守：它会引用 `__tauri_command_name_<name>` 与
//! `__cmd__<name>`，名称对不上时 bin target 直接编译失败（E0433），
//! 无需测试重复。

const COMMANDS_SRC: &str = include_str!("commands.rs");
const MAIN_SRC: &str = include_str!("main.rs");

/// 排序去重，让断言输出的名字顺序稳定。
fn sorted_unique(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

/// 扫出 `commands.rs` 里所有 `#[tauri::command]` 标注的函数名。
fn declared_commands() -> Vec<String> {
    let lines: Vec<&str> = COMMANDS_SRC.lines().collect();
    let mut names = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // 前缀匹配，兼容 `#[tauri::command(async)]` 这类带参数的写法。
        if !line.trim().starts_with("#[tauri::command") {
            continue;
        }
        // 属性之后到函数签名之间可能还有文档注释，向下找第一条以
        // `pub` / `async` / `fn` 开头的行（不设固定行数窗口）。
        if let Some(name) = lines[i + 1..]
            .iter()
            .map(|l| l.trim())
            .find(|l| l.starts_with("pub") || l.starts_with("async") || l.starts_with("fn"))
            .and_then(|sig| sig.split_once("fn "))
            .and_then(|(_, rest)| rest.split('(').next())
        {
            names.push(name.trim().to_string());
        }
    }
    sorted_unique(names)
}

/// 扫出 `main.rs` 中 `generate_handler!` 块里注册的 `commands::<name>`。
///
/// 起始行本身也扫描一遍，这样 `generate_handler![...]` 写成单行时同样成立。
fn registered_commands() -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in MAIN_SRC.lines() {
        let segment = if inside {
            line
        } else {
            match line.split_once("generate_handler!") {
                Some((_, rest)) => {
                    inside = true;
                    rest
                }
                None => continue,
            }
        };

        let (body, closed) = match segment.split_once("])") {
            Some((body, _)) => (body, true),
            None => (segment, false),
        };
        for token in body.split("commands::").skip(1) {
            let name: String = token
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }

        if closed {
            break;
        }
    }
    sorted_unique(names)
}

/// 每个 `#[tauri::command]` 都必须在 `generate_handler!` 中注册。
#[test]
fn every_tauri_command_is_registered_in_generate_handler() {
    let registered = registered_commands();
    let missing: Vec<String> = declared_commands()
        .into_iter()
        .filter(|c| !registered.contains(c))
        .collect();

    assert!(
        missing.is_empty(),
        "以下 #[tauri::command] 未在 main.rs 的 generate_handler! 中注册，\
         前端 invoke 会报 `command ... not found`：{missing:?}"
    );
}

// ----------------------------------------------------------------
// 命令线程亲和性：会读系统凭据库的命令必须是 async
// ----------------------------------------------------------------

/// 读 / 写系统凭据库（macOS Keychain、Windows 凭据管理器）的命令。
///
/// 新增同类命令时一并登记 —— 判据是「调用链最终落到
/// [`crate::crypto::system_key_available`] / [`crate::crypto::default_vault_cipher`]」，
/// 读源码判定，测试只负责钉住已登记的那几个不被改回同步。
const CREDENTIAL_STORE_COMMANDS: &[&str] = &["keychain_available"];

/// 取出某个 `#[tauri::command]` 的签名行（属性之后第一条 `pub` / `async` / `fn`
/// 开头的行）；命令名对不上或压根没声明时返回 `None`。
fn command_signature(name: &str) -> Option<String> {
    let lines: Vec<&str> = COMMANDS_SRC.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.trim().starts_with("#[tauri::command") {
            continue;
        }
        let Some(sig) = lines[i + 1..]
            .iter()
            .map(|l| l.trim())
            .find(|l| l.starts_with("pub") || l.starts_with("async") || l.starts_with("fn"))
        else {
            continue;
        };
        if sig
            .split_once("fn ")
            .and_then(|(_, rest)| rest.split('(').next())
            == Some(name)
        {
            return Some(sig.to_string());
        }
    }
    None
}

/// 读凭据库的命令声明成 `pub async fn`。
///
/// **为什么这条是契约而不是风格**：Tauri 的同步命令在**主线程**上内联执行 ——
/// IPC 经 `WKScriptMessageHandler` 送达（WebKit 保证主线程回调），
/// `body_blocking` 在同一调用帧里就把命令跑完。而**异步命令的结果回传同样要
/// 主线程**（wry 用 `evaluateJavaScript:completionHandler:`）。所以主线程一卡，
/// 停摆的不只是它自己，而是**所有**在飞的 IPC 响应 —— 包括 `search_results`。
///
/// 真实故障：`keychain_available` 曾是 `pub fn`。`data_key_cached` 在进程级密钥
/// 缓存未命中时，会在调用线程上 `Condvar::wait_timeout` 阻塞最多 `KEYCHAIN_TIMEOUT`
/// （2 秒）。缓存只在成功时写入且从不清理，于是一次超时就让该进程永久留在慢路径；
/// 启动期那次预热（`main.rs` 的 setup 里）超时的话，之后**每次唤出面板**都要付这 2 秒。
/// 面板的列表刷新恰好与它并发发起（`main.ts` 的 focus 处理器里
/// `refreshKeychainBanner` 和 `load()` 一起 `void` 出去），于是首帧停在「上一次的
/// 列表」，面板冻住约 2 秒后响应才送达、列表才变新。
#[test]
fn credential_store_commands_are_async() {
    let sync: Vec<String> = CREDENTIAL_STORE_COMMANDS
        .iter()
        .filter(|name| {
            // 没找到签名也算不通过：命令被删掉/改名时这条会红，而不是静默放过。
            command_signature(name).is_none_or(|sig| !sig.contains("async fn"))
        })
        .map(|name| (*name).to_string())
        .collect();

    assert!(
        sync.is_empty(),
        "以下命令会读系统凭据库（可能阻塞数秒），却声明成同步命令：{sync:?}。\
         同步命令在主线程上内联执行，而异步命令的结果回传也要主线程 —— 这样一个 \
         会阻塞的同步命令会把所有在飞的 IPC 响应一起停摆。改成 `pub async fn`，\
         Tauri 会把它 spawn 到 async runtime 上。"
    );
}

// ----------------------------------------------------------------
// open_url 的 scheme 白名单（U7；Windows 侧 U5 复用它）
// ----------------------------------------------------------------
//
// 判据抽成纯函数后，两端共用同一份校验 —— 这条正是"平台移植时最容易两端各写一遍、
// 然后其中一端的白名单悄悄松掉"的地方。

use crate::commands::openable_url;

#[test]
fn openable_url_accepts_http_and_https() {
    assert_eq!(
        openable_url("http://example.com/a?b=1").unwrap(),
        "http://example.com/a?b=1"
    );
    assert_eq!(
        openable_url("https://example.com").unwrap(),
        "https://example.com"
    );
}

#[test]
fn openable_url_trims_surrounding_whitespace() {
    // 剪贴板内容常带换行（从终端或多行文本里复制的 URL）。
    assert_eq!(
        openable_url("  https://example.com/x\n").unwrap(),
        "https://example.com/x"
    );
}

#[test]
fn openable_url_rejects_dangerous_schemes() {
    // 这些 scheme 被系统处理器接手时可能触发本地执行或脚本执行（clipboard hijacking），
    // 一律拒绝。
    for raw in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "vbscript:msgbox(1)",
        "ftp://example.com/x",
    ] {
        assert!(openable_url(raw).is_err(), "{raw} 不该通过白名单");
    }
}

#[test]
fn openable_url_rejects_empty_and_plain_text() {
    assert!(openable_url("").is_err());
    assert!(openable_url("   ").is_err());
    assert!(openable_url("example.com").is_err());
}

#[test]
fn openable_url_is_case_sensitive_today() {
    // 既有行为：scheme 首字母大写会被拒绝（RFC 上 scheme 不区分大小写，这是已知的严格侧偏差）。
    // 钉住它，免得日后以为是随手写松了。
    assert!(openable_url("HTTPS://example.com").is_err());
}

// ----------------------------------------------------------------
// 关于面板（U13）：仓库地址常量 + 外链命令的同一个白名单
// ----------------------------------------------------------------

use crate::commands::{REPOSITORY_URL, open_external_url, resolve_version};

#[test]
fn resolve_version_prefers_tauri_conf_and_falls_back_to_crate_version() {
    // tauri.conf 的 version 是用户能对照的那一个（DMG / NSIS 文件名、Release tag）。
    assert_eq!(resolve_version(Some("0.8.0")), "0.8.0");
    // 两侧空白不该被原样渲染进关于面板。
    assert_eq!(resolve_version(Some(" 0.8.0\n")), "0.8.0");
    // 缺失 / 空串 / 纯空白都回退 crate 版本，而不是显示一个空键帽似的空白。
    let crate_version = env!("CARGO_PKG_VERSION");
    assert_eq!(resolve_version(None), crate_version);
    assert_eq!(resolve_version(Some("")), crate_version);
    assert_eq!(resolve_version(Some("   ")), crate_version);
}

#[test]
fn repository_url_is_an_https_repo_link() {
    // 关于面板把这条地址直接渲染给用户，写错 scheme 会让「点开」在系统层失败。
    assert!(REPOSITORY_URL.starts_with("https://github.com/"));
    assert!(openable_url(REPOSITORY_URL).is_ok());
    // 不带 .git 后缀：那是给 git 用的克隆地址，不是给人看的仓库主页。
    assert!(!REPOSITORY_URL.ends_with(".git"));
}

#[test]
fn open_external_url_rejects_dangerous_schemes() {
    // 与 open_url 共用 openable_url：这条命令的入参来自前端（可被改），
    // 不能因为「调用方是我们自己的关于面板」就放宽。
    for raw in ["file:///etc/passwd", "javascript:alert(1)", "example.com"] {
        assert!(
            open_external_url(raw.to_string()).is_err(),
            "{raw} 不该通过白名单"
        );
    }
}
