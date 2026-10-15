//! U1：`app/src/log.rs` 纯函数单测。
//!
//! 这些用例不依赖文件系统、不持有全局状态——U2 才在 `LogState` / `OnceLock` 上
//! 装 I/O 胶水。任何平台/闰年/字节边界的正确性,先在这里钉死,再让 U2 接线。

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::log::{
    Level, MAX_LOG_BYTES, apply_log_level, backup_log_path, boot_load_log_level, clear,
    format_line, format_timestamp, init, log_path_in, parse_level, read_info, read_tail,
    reveal_argv, should_emit, should_rotate, tail_lines,
};
use crate::storage::Storage;
use crate::storage::test_helpers::open_test_db;

#[test]
fn log_path_in_appends_filename() {
    assert_eq!(
        log_path_in(Path::new("/x/ClipVault")),
        Path::new("/x/ClipVault/clipvault.log")
    );
}

#[test]
fn backup_log_path_appends_dot_1() {
    assert_eq!(
        backup_log_path(Path::new("/x/ClipVault")),
        Path::new("/x/ClipVault/clipvault.log.1")
    );
}

#[test]
fn parse_level_known_and_unknown() {
    assert_eq!(parse_level(Some("verbose")), Level::Verbose);
    assert_eq!(parse_level(Some("info")), Level::Info);
    assert_eq!(parse_level(Some("")), Level::Info);
    assert_eq!(parse_level(None), Level::Info);
    // 仓库只有 Info/Verbose 两档;`debug` 之类的未来扩展走 KTD 决策,
    // 此处不应静默接受为 Verbose。
    assert_eq!(parse_level(Some("debug")), Level::Info);
    assert_eq!(parse_level(Some("INFO")), Level::Info); // 大小写不匹配 = 回落
}

#[test]
fn format_timestamp_known_epochs() {
    // Unix 纪元起点。
    assert_eq!(format_timestamp(0), "1970-01-01T00:00:00.000Z");
    // 中段时刻,顺便验 h:m:s:ms。
    assert_eq!(format_timestamp(1234567890123), "2009-02-13T23:31:30.123Z");
    // 闰年 2000-02-29:2000 能被 400 整除,的确是闰年。
    assert_eq!(format_timestamp(951782400000), "2000-02-29T00:00:00.000Z");
    // 1900-02-28:1900 能被 100 整除但不能被 400 整除,**不**是闰年。
    // 关键回归点——若 civil_from_days 退化,会跨到 1900-03-01。
    assert_eq!(
        format_timestamp(-2_203_977_600_000),
        "1900-02-28T00:00:00.000Z"
    );
    // 9999-12-31 23:59:59.000:年份上界仍能正确格式化(不溢出成 10000-...)。
    assert_eq!(
        format_timestamp(253_402_300_799_000),
        "9999-12-31T23:59:59.000Z"
    );
}

#[test]
fn format_line_control_chars() {
    // 记录级别用 log::Level::Info（与"用户设置 Info"对应的常规 INFO 记录）。
    let line = format_line(0, log::Level::Info, "test", "hello\x07world\nnext");
    assert!(line.starts_with("1970-01-01T00:00:00.000Z INFO [test] "));
    // \x07 → '?';\n 保留。
    assert!(line.contains("hello?world\nnext"));
    assert!(line.ends_with('\n'));
}

#[test]
fn format_line_combined() {
    // 记录级别 log::Level::Debug(仅 Verbose 用户设置下才会写出),输出 "DEBUG"。
    let line = format_line(1234567890123, log::Level::Debug, "module::sub", "x = 42");
    assert_eq!(
        line,
        "2009-02-13T23:31:30.123Z DEBUG [module::sub] x = 42\n"
    );
}

#[test]
fn should_rotate_under_and_over_cap() {
    let cap = 1024u64;
    assert!(!should_rotate(0, 1024, cap));
    assert!(should_rotate(1024, 1, cap));
    assert!(should_rotate(1023, 2, cap));
    // 当前 size == cap 但不再写入(陈旧大文件):should_rotate 不动,
    // 真正"陈旧大文件也轮转"由 init 直接用 `size > MAX_LOG_BYTES` 判断。
    assert!(!should_rotate(1024, 0, cap));
    // 仓库常量的契约值。
    assert_eq!(MAX_LOG_BYTES, 1024 * 1024);
}

#[test]
fn should_rotate_with_backup_present() {
    // should_rotate 只回答"该不该轮",与 .1 备份是否存在无关——I/O 层负责
    // remove(.1) + rename(current, .1)。这里仅断言决策边界。
    assert!(should_rotate(2_000_000, 1, MAX_LOG_BYTES));
    assert!(!should_rotate(500_000, 100, MAX_LOG_BYTES));
}

#[test]
fn tail_lines_truncates_by_line_cap() {
    // 行 cap 触发,字节 cap 远大。
    let mut bytes = Vec::new();
    for i in 0..200 {
        bytes.extend_from_slice(format!("line{i}\n").as_bytes());
    }
    let tail = tail_lines(&bytes, 1_000_000, 10);
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 10, "保留最后 10 行");
    assert_eq!(lines[0], "line190");
    assert_eq!(lines[9], "line199");
}

#[test]
fn tail_lines_truncates_by_byte_cap() {
    // 行 cap 远大,字节 cap 触发。line100..199 是 8 字节定长,总 1490 字节;
    // 1490-64=1426 正好是 line192 起点,且 bytes[1425]='\n'(line191 末尾),
    // 切到行边界时 8 行完整落入 64 字节窗口,不丢行。
    let mut bytes = Vec::new();
    for i in 0..200 {
        bytes.extend_from_slice(format!("line{i}\n").as_bytes());
    }
    let tail = tail_lines(&bytes, 64, 1000);
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 8, "切到行边界时字节 cap 留 8 行");
    assert_eq!(lines[0], "line192");
    assert_eq!(lines[7], "line199");
}

#[test]
fn tail_lines_drops_partial_first_line() {
    // 字节 cap 切到行中间,首行被截断的"ne191\n"应丢弃。
    // 总 1490 字节,cut = 1490-70 = 1420,落在 line191("line191\n", 8B, 起 1418)
    // 内部第 2 字节('n');首个 \n 在相对位置 5,跳过 → start = 1426,得到 8 行。
    let mut bytes = Vec::new();
    for i in 0..200 {
        bytes.extend_from_slice(format!("line{i}\n").as_bytes());
    }
    let tail = tail_lines(&bytes, 70, 1000);
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 8, "切到行中间后仍 8 行完整");
    assert_eq!(lines[0], "line192", "不完整首行 'ne191' 已被丢弃");
}

#[test]
fn tail_lines_handles_split_multibyte() {
    // 末尾是孤立的 0xE4——UTF-8 不完整序列。
    let mut raw = b"good line\n".to_vec();
    raw.push(0xE4);
    let tail = tail_lines(&raw, 64, 100);
    // from_utf8_lossy 替换非法字节,产物仍是合法 UTF-8 String。
    assert!(tail.starts_with("good line\n"));
    assert!(tail.ends_with('\n'));
}

#[test]
fn tail_lines_empty_input() {
    assert_eq!(tail_lines(b"", 64, 10), "");
}

#[test]
fn tail_lines_no_newline_in_window() {
    // 字节 cap 切到一段没有 \n 的内容:整段视为不完整首行,丢弃。
    let raw = b"abcdefghij";
    let tail = tail_lines(raw, 5, 10);
    assert_eq!(tail, "");
}

#[test]
fn reveal_argv_macos_uses_open_r() {
    assert_eq!(
        reveal_argv(true, "/var/log/clipvault.log"),
        (
            "open".to_string(),
            vec!["-R".to_string(), "/var/log/clipvault.log".to_string()],
        )
    );
}

#[test]
fn reveal_argv_windows_uses_explorer_select() {
    let path = r"C:\Users\me\AppData\Roaming\ClipVault\clipvault.log";
    let (prog, args) = reveal_argv(false, path);
    assert_eq!(prog, "explorer.exe");
    // /select, 紧贴路径,无空格——explorer 的标准格式。
    assert_eq!(args, vec![format!("/select,{}", path)]);
}

// ----------------------------------------------------------------
// U2：I/O 层 + 全局 sink 接线单测。
//
// 设计要点（与 U1 一脉）：纯函数优先、I/O 测隔离在临时目录。
// log crate 的 `set_boxed_logger` 是进程级唯一,无法在并行测试里多次调用,
// 所以 `init` 在 `cfg(test)` 下不安装到全局（见 `install_global_logger`）,
// 仍能验证 `read_info` / `read_tail` / `clear` 等 I/O 行为。
// ----------------------------------------------------------------

/// 进程内唯一的临时日志目录(用于隔离并行测试)。
fn temp_log_dir(label: &str) -> PathBuf {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!(
        "clipvault_log_test_{label}_{n}_{nanos}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn init_creates_file_when_missing() {
    let dir = temp_log_dir("create");
    let path = crate::log::log_path_in(&dir);
    let level = Arc::new(AtomicU8::new(Level::Info as u8));
    init(&path, level).expect("init 失败");
    assert!(path.exists());
    let size = std::fs::metadata(&path).unwrap().len();
    assert_eq!(size, 0);
    // 幂等:再调一次不报错。
    init(&path, Arc::new(AtomicU8::new(Level::Info as u8))).expect("二次 init");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_rotates_oversize_existing_via_rename() {
    let dir = temp_log_dir("rot");
    let current = crate::log::log_path_in(&dir);
    let backup = crate::log::backup_log_path(&dir);
    // 预写 2 MiB current + 1 KiB backup。init 应 remove(.1) + rename。
    let big = vec![b'x'; 2 * 1024 * 1024];
    std::fs::write(&current, &big).unwrap();
    std::fs::write(&backup, vec![b'y'; 1024]).unwrap();
    let level = Arc::new(AtomicU8::new(Level::Info as u8));
    init(&current, level).expect("init");
    let cur_size = std::fs::metadata(&current).unwrap().len();
    let bk_size = std::fs::metadata(&backup).unwrap().len();
    assert!(cur_size <= crate::log::MAX_LOG_BYTES);
    // .1 是被 rotate 过去的 2 MiB(原 1 KiB .1 已被覆盖)。
    assert_eq!(bk_size, 2 * 1024 * 1024);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_when_only_backup_exists_no_panic() {
    let dir = temp_log_dir("bk");
    let backup = crate::log::backup_log_path(&dir);
    std::fs::write(&backup, b"previous session\n").unwrap();
    let current = crate::log::log_path_in(&dir);
    let level = Arc::new(AtomicU8::new(Level::Info as u8));
    init(&current, level).expect("init");
    assert!(current.exists());
    assert_eq!(current.metadata().unwrap().len(), 0);
    // .1 保持原状(不被误删)。
    let preserved = std::fs::read(&backup).unwrap();
    assert_eq!(preserved, b"previous session\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_failure_is_non_fatal() {
    // 传入一个无法作为目录创建 / 文件写入的 path(空路径名)。init 返回 Err,
    // 整个 app 仍能继续(由 main.rs 决定兜底行为)。
    let bad = std::path::Path::new("");
    let level = Arc::new(AtomicU8::new(Level::Info as u8));
    let res = init(bad, level);
    assert!(res.is_err());
}

#[test]
fn boot_load_log_level_round_trip() {
    let storage = open_test_db();
    let atomic = AtomicU8::new(Level::Verbose as u8);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // 写 verbose → 读出 Verbose。
        crate::log::persist_log_level(&storage, Level::Verbose)
            .await
            .unwrap();
        let loaded = boot_load_log_level(&storage, &atomic).await.unwrap();
        assert_eq!(loaded, Level::Verbose);
        assert_eq!(
            atomic.load(std::sync::atomic::Ordering::SeqCst),
            Level::Verbose as u8
        );
        // 写 garbage → 读出 Info(回落),且原子被覆盖为 Info。
        storage
            .set_setting(crate::log::LOG_LEVEL_KEY, "garbage")
            .await
            .unwrap();
        let loaded = boot_load_log_level(&storage, &atomic).await.unwrap();
        assert_eq!(loaded, Level::Info);
        assert_eq!(
            atomic.load(std::sync::atomic::Ordering::SeqCst),
            Level::Info as u8
        );
    });
}

#[test]
fn set_max_level_changes_log_filter() {
    // 验证 set_max_level 同时更新原子与 `log::set_max_level`。
    let atomic = AtomicU8::new(Level::Info as u8);
    crate::log::set_max_level(&atomic, Level::Verbose);
    assert_eq!(
        log::max_level(),
        Level::Verbose.log_level_upper_bound().to_level_filter()
    );
    assert!(should_emit(&atomic, log::Level::Debug));
    crate::log::set_max_level(&atomic, Level::Info);
    assert_eq!(
        log::max_level(),
        Level::Info.log_level_upper_bound().to_level_filter()
    );
    // Info 模式下不应写出 Debug 级记录。
    assert!(!should_emit(&atomic, log::Level::Debug));
    // 但应写出 Info / Warn / Error。
    assert!(should_emit(&atomic, log::Level::Info));
    assert!(should_emit(&atomic, log::Level::Warn));
    assert!(should_emit(&atomic, log::Level::Error));
}

#[test]
fn clear_truncates_current_and_removes_backup() {
    let dir = temp_log_dir("clear");
    let current = crate::log::log_path_in(&dir);
    let backup = crate::log::backup_log_path(&dir);
    std::fs::write(&current, b"line1\nline2\n").unwrap();
    std::fs::write(&backup, b"line0\n").unwrap();
    clear(&dir).expect("clear");
    assert_eq!(current.metadata().unwrap().len(), 0);
    assert!(!backup.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_info_reports_size_and_level() {
    let dir = temp_log_dir("info");
    let current = crate::log::log_path_in(&dir);
    let backup = crate::log::backup_log_path(&dir);
    std::fs::write(&current, b"hi\n").unwrap();
    std::fs::write(&backup, b"old\n").unwrap();
    let atomic = AtomicU8::new(Level::Verbose as u8);
    let info = read_info(&current, &atomic);
    assert_eq!(info.size_bytes, 3);
    assert_eq!(info.backup_size_bytes, 4);
    assert_eq!(info.level, "verbose");
    assert!(info.path.ends_with("clipvault.log"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_tail_combines_backup_and_current() {
    let dir = temp_log_dir("tail");
    let current = crate::log::log_path_in(&dir);
    let backup = crate::log::backup_log_path(&dir);
    std::fs::write(&backup, b"line1\nline2\n").unwrap();
    std::fs::write(&current, b"line3\nline4\n").unwrap();
    let tail = read_tail(&dir, 1024, 100).expect("read_tail");
    // 拼接顺序: .1 在前,current 在后。
    assert_eq!(tail, "line1\nline2\nline3\nline4\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_tail_handles_missing_backup() {
    let dir = temp_log_dir("tail2");
    let current = crate::log::log_path_in(&dir);
    std::fs::write(&current, b"line3\nline4\n").unwrap();
    let tail = read_tail(&dir, 1024, 100).expect("read_tail");
    assert_eq!(tail, "line3\nline4\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_log_level_persists_and_updates() {
    // 验证 KTD5 形状:clamp + 原子更新 + 持久化 + 重新读出一致。
    let storage: Storage = open_test_db();
    let atomic = AtomicU8::new(Level::Info as u8);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        apply_log_level(&storage, &atomic, Level::Verbose)
            .await
            .unwrap();
        assert_eq!(
            atomic.load(std::sync::atomic::Ordering::SeqCst),
            Level::Verbose as u8
        );
        let reloaded = boot_load_log_level(&storage, &atomic).await.unwrap();
        assert_eq!(reloaded, Level::Verbose);
    });
}
