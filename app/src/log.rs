//! 诊断日志模块（U1：纯函数层）。
//!
//! 本文件只放纯函数，不持有全局状态、不做 I/O；具体的 sink 初始化、文件句柄、轮转
//! 调度在 U2 的 `init` / `read_info` / `read_tail` / `clear` 与 `LogState` 上。
//! 见 `docs/plans/2026-10-15-1642-feat-diagnostics-log-viewer-plan.md` 的
//! Product Contract R1–R5、Planning Contract KTD1–KTD3 与 KTD8、Unit U1。
//!
//! 设计要点（与 plan 一一对应）：
//! - `default_log_path()` 复用 `storage::default_db_path()` 的目录,把文件名替换成
//!   `clipvault.log`,所以日志文件与 SQLite 库**在同目录**。两个平台共用一个函数。
//! - 时间戳不引入日期 crate（KTD2）。`format_timestamp` 是纯函数,单测可在 macOS 上
//!   覆盖闰年/世纪年/UTC 边界,与 `storage::resolve_data_dir` 同样可在 macOS 上
//!   测另一平台分支（此处无平台分支,但纯函数性同等重要）。
//! - 轮转决策（KTD8）走「main + `.1`」方案：`should_rotate` 只回答「该不该轮」,
//!   「怎么轮」由 I/O 层负责 remove + rename + 重置计数。
//! - `reveal_argv` 是「显式 is_macos 参数的纯函数」(KTD6),macOS 与 Windows 的
//!   命令行参数都能在 macOS 上被单测覆盖。

use std::path::{Path, PathBuf};

/// 单文件字节上限（1 MiB）。`MAX_LOG_BYTES * 2 = 2 MiB` 是磁盘总占用上限
/// （main + `.1` 备份,R5 / KTD8）。
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// `settings` 表里 `log_level` 行的 key（U2 / R7）。
pub const LOG_LEVEL_KEY: &str = "log_level";

/// 日志级别（U2 / R3 / R4）。
///
/// 任何级别都不记剪贴板正文（R4 / R16）。Verbose 级别多出**结构信息**
/// （条目 kind / mime / 字节数 / 状态转移等）,仍只记诊断事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 标准级别：INFO / WARN / ERROR 都写。打包后默认级别。
    Info,
    /// 详细级别：额外结构信息,但不包含剪贴板内容。
    Verbose,
}

impl Level {
    /// 用于写入 `settings` 表的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Verbose => "verbose",
        }
    }
}

/// 当前活跃的日志文件路径：与 `clipvault.db` 同目录。
///
/// 复用 `storage::default_db_path()` 的目录,只把文件名替换为 `clipvault.log`。
/// 不引入新的路径解析逻辑,确保日志与库文件永远在同一系统位置。
pub fn default_log_path() -> PathBuf {
    crate::storage::default_db_path().with_file_name("clipvault.log")
}

/// 测试用纯函数版:对给定数据目录直接拼 `clipvault.log`。
pub fn log_path_in(dir: &Path) -> PathBuf {
    dir.join("clipvault.log")
}

/// 轮转备份路径:KTD8 的 `clipvault.log.1`。
pub fn backup_log_path(dir: &Path) -> PathBuf {
    dir.join("clipvault.log.1")
}

/// 解析 `settings` 表里的原始值。未知值回落到 `Info`。
pub fn parse_level(raw: Option<&str>) -> Level {
    match raw {
        Some("verbose") => Level::Verbose,
        _ => Level::Info,
    }
}

/// 轮转决策:KTD8「main + .1」方案的纯函数层。
///
/// 命中条件:`current_size + incoming_len > cap`。`u64` 用 `saturating_add` 防溢出。
/// `init` 阶段遇到陈旧大文件时(老文件 > cap)也通过此函数返回 `true` 触发轮转,
/// 而不是直接 `set_len(0)` 截断,这样重启后还能保留启动前最后一段历史。
pub fn should_rotate(current_size: u64, incoming_len: u64, cap: u64) -> bool {
    current_size.saturating_add(incoming_len) > cap
}

/// UTC 时间戳(KTD2):把 Unix 毫秒转 `YYYY-MM-DDTHH:MM:SS.mmmZ`。
///
/// 不引入日期 crate;`civil_from_days` 来自 Howard Hinnant 的算法,
/// 闰年/世纪年边界由 `civil_from_days` 内置处理(1900 不闰、2000 闰)。
pub fn format_timestamp(epoch_millis: i64) -> String {
    let (y, m, d, hh, mm, ss, ms) = utc_datetime(epoch_millis);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{ms:03}Z")
}

/// 单行日志格式(R2):`{ts} {LEVEL} [{target}] {msg}\n`。
///
/// `msg` 内的控制字符(除 `\n` 外)按 `?` 转写;`\n` 保留,允许多行消息。
pub fn format_line(ts_millis: i64, level: Level, target: &str, msg: &str) -> String {
    let level_str = match level {
        Level::Info => "INFO",
        Level::Verbose => "VERBOSE",
    };
    let mut out = String::with_capacity(64 + target.len() + msg.len());
    out.push_str(&format_timestamp(ts_millis));
    out.push(' ');
    out.push_str(level_str);
    out.push_str(" [");
    out.push_str(target);
    out.push_str("] ");
    out.push_str(&sanitize_msg(msg));
    out.push('\n');
    out
}

/// 从 `civil_datetime` 内部组件 → `format_line` 用的控制字符转写。
///
/// 策略:换行保留(允许多行消息),其他控制字符(包含 `\t` / `\r` / `\x07` / 不可打印
/// 的高位控制区)按 `?` 替换,保证日志在文本查看器/编辑器里不出现奇怪字符。
fn sanitize_msg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\n' {
            out.push('\n');
        } else if c.is_control() {
            out.push('?');
        } else {
            out.push(c);
        }
    }
    out
}

/// 把 Unix 毫秒拆成 UTC 年月日时分秒毫秒。
fn utc_datetime(epoch_millis: i64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let (days, ms_in_day) = div_rem_floor(epoch_millis, 86_400_000);
    let hh = (ms_in_day / 3_600_000) as u32;
    let mm = ((ms_in_day / 60_000) % 60) as u32;
    let ss = ((ms_in_day / 1_000) % 60) as u32;
    let ms = (ms_in_day % 1_000) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, hh, mm, ss, ms)
}

/// Euclidean 除/余(Rust 的 `%` 是截断,负数场景下需要 floor 语义)。
fn div_rem_floor(a: i64, b: i64) -> (i64, i64) {
    let q = a / b;
    let r = a % b;
    if (r != 0) && ((r < 0) != (b < 0)) {
        (q - 1, r + b)
    } else {
        (q, r)
    }
}

/// Howard Hinnant `civil_from_days`:把自 1970-01-01 起的天数转 `(y, m, d)`。
///
/// 输入 `z >= 0` 与 `z < 0` 都正确处理(后者在 1970 之前,包括 1900 这种世纪年)。
/// 输出的 `y` 可以是 1970 之前的年份。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    // era:400 年为一组(146097 天)。负数 z 用 floor 除法,避免截断偏差。
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = ((doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365) as i64; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe as u64 + (yoe / 4) as u64 - (yoe / 100) as u64); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    // 1 月/2 月的年份要 +1(因为 mp 的 0/1 对应上一年的 1/2 月)
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// 尾部读取的纯函数层(U2 的 `read_tail` 调它,单测能直接覆盖字节/行 cap 与
/// UTF-8 边界处理,无须真文件)。
///
/// - `bytes` 为文件原始字节;`max_bytes` / `max_lines` 是双重 cap,先到者生效。
/// - 首行被字节 cap 截断时丢弃(避免显示半行);`split('\n')` 后过滤空段,所以
///   `b""` / 全空白返回空串。
/// - 拼接用 `from_utf8_lossy`:不完整 UTF-8 序列按 U+FFFD 替换,绝无 panic。
pub fn tail_lines(bytes: &[u8], max_bytes: usize, max_lines: usize) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // 1) 字节切片:若 cut 之前是 \n,bytes[cut..] 从完整行开始,无需丢弃;
    //    若 cut 落在某行中间,丢弃该行不完整部分;若 cut 后整段无 \n,视为
    //    单一不完整行,直接返回空。
    let start = if bytes.len() <= max_bytes {
        0
    } else {
        let cut = bytes.len() - max_bytes;
        if cut > 0 && bytes[cut - 1] == b'\n' {
            cut
        } else {
            match bytes[cut..].iter().position(|&b| b == b'\n') {
                Some(nl) => cut + nl + 1,
                None => return String::new(),
            }
        }
    };
    let slice = &bytes[start..];
    // 2) 取最后 `max_lines` 行:split 后过滤空段,行号在 `slice` 内是确定的。
    let lines: Vec<&[u8]> = slice
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    let chosen: &[&[u8]] = if lines.len() <= max_lines {
        &lines[..]
    } else {
        &lines[lines.len() - max_lines..]
    };
    // 3) 拼接。每一行都补 `\n`,与 `format_line` 的行尾格式对称。
    let mut out = String::with_capacity(chosen.iter().map(|l| l.len() + 1).sum());
    for line in chosen {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out
}

/// 平台 reveal 命令的纯函数(KTD6):macOS 走 `open -R`,Windows 走
/// `explorer.exe /select,<path>`(`/select,` 紧贴路径,无空格,explorer 的标准格式)。
///
/// `is_macos` 由调用方传 `cfg!(target_os = "macos")`,这样 Windows 分支也能在
/// macOS 上被单测覆盖。
pub fn reveal_argv(is_macos: bool, path: &str) -> (String, Vec<String>) {
    if is_macos {
        ("open".to_string(), vec!["-R".to_string(), path.to_string()])
    } else {
        (
            "explorer.exe".to_string(),
            vec![format!("/select,{}", path)],
        )
    }
}
