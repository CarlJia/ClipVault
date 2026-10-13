//! IPC 命令注册表守卫（U7 回归）。
//!
//! `commands.rs` 里带 `#[tauri::command]` 的函数必须同时在 `main.rs` 的
//! `tauri::generate_handler!` 中注册；漏注册时前端 `invoke` 会直接收到
//! `command xxx not found`（U7 的 `open_url` 就这么漏掉了）。
//!
//! lib target 的测试都绕过 IPC 直接调函数，抓不到这种缺口，所以这里用
//! `include_str!` 静态比对两份源码。
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
