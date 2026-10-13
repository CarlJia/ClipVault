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
