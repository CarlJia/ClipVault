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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
/// `level` 是**记录级别**(`log::Level`:`Error`/`Warn`/`Info`/`Debug`/`Trace`),
/// 与 `Level`（用户设置,`Info`/`Verbose`）是两件事:
/// - 用户设置是过滤器(`log::set_max_level`),决定哪些 `log::*!` 调用进入 sink;
/// - 记录级别是这一行实际写出去的字符串。
///
/// `msg` 内的控制字符(除 `\n` 外)按 `?` 转写;`\n` 保留,允许多行消息。
pub fn format_line(ts_millis: i64, level: log::Level, target: &str, msg: &str) -> String {
    let level_str = match level {
        log::Level::Error => "ERROR",
        log::Level::Warn => "WARN",
        log::Level::Info => "INFO",
        log::Level::Debug => "DEBUG",
        log::Level::Trace => "TRACE",
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

// ----------------------------------------------------------------
// U2：I/O 层 + 全局 sink 接线。
//
// KTD1：`log` crate 作为 facade(0.4.34 在依赖树内,Cargo.toml 已升为直接依赖,
//      `cargo tree` 形状不变,R13 仍空)。本文件实现 `log::Log` trait 并
//      `set_boxed_logger` 安装到全局,U3 的 `log::warn!` / `log::error!` 等宏直接路由到
//      本 sink。KTD3：进程级 `OnceLock<LogState>` 持有 `Mutex<Option<File>>`,
//      写路径持锁 → 检查轮转 → 写,锁中毒用 `unwrap_or_else(|e| e.into_inner())` 恢复。
// ----------------------------------------------------------------

impl Level {
    /// 原子里的 u8 → `Level`(0=Info,1=Verbose;其它值回落 Info)。
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Level::Verbose,
            _ => Level::Info,
        }
    }

    /// 当前级别允许的 `log::Level` 上界:
    /// - `Info` → `log::Level::Info`(= 允许 Error/Warn/Info);
    /// - `Verbose` → `log::Level::Debug`(= 允许 Error/Warn/Info/Debug)。
    pub fn log_level_upper_bound(self) -> log::Level {
        match self {
            Level::Info => log::Level::Info,
            Level::Verbose => log::Level::Debug,
        }
    }
}

/// 进程级日志状态(U2 / KTD3):写路径持锁,锁中毒可恢复。
///
/// `file` 用 `Mutex<Option<File>>` 而不是 `Mutex<File>`,是为了让轮转
/// 路径能把当前 file 临时取出并 drop(在 Windows 上 rename 之前必须先关闭源文件),
/// 重新 open 后再放回去。
pub struct LogState {
    file: Mutex<Option<std::fs::File>>,
    size: AtomicU64,
    dir: PathBuf,
    level: Arc<AtomicU8>,
}

impl LogState {
    /// 打开(或创建)`clipvault.log`;若发现陈旧大文件(> MAX_LOG_BYTES),先按
    /// KTD8 走 `remove(.1)` + `rename(current, .1)`,失败降级 `set_len(0)`。
    fn open(path: &Path, level: Arc<AtomicU8>) -> std::io::Result<Self> {
        let dir = path
            .parent()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("log path {} has no parent directory", path.display()),
                )
            })?
            .to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let current = log_path_in(&dir);
        // 陈旧大文件 → 轮转(降级 set_len)。
        if let Ok(meta) = std::fs::metadata(&current)
            && meta.len() > MAX_LOG_BYTES
        {
            let backup = backup_log_path(&dir);
            let _ = std::fs::remove_file(&backup);
            if std::fs::rename(&current, &backup).is_err() {
                // 降级:截断(权限/锁等场景下保留当前文件,清空内容)。
                if let Ok(f) = std::fs::OpenOptions::new().open(&current) {
                    let _ = f.set_len(0);
                }
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&current)?;
        let size = file.metadata()?.len();
        Ok(Self {
            file: Mutex::new(Some(file)),
            size: AtomicU64::new(size),
            dir,
            level,
        })
    }

    /// 写一行日志:KTD8 的「写之前检查轮转」流程。锁中毒恢复(`into_inner`),
    /// 不 panic(R1 的「不阻塞启动」延伸)。
    pub fn write_line(&self, line: &str) -> std::io::Result<()> {
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = line.as_bytes();
        let current_size = self.size.load(Ordering::Relaxed);
        if should_rotate(current_size, bytes.len() as u64, MAX_LOG_BYTES) {
            // rotate 关闭当前 file(drop)、rename、再 open。
            Self::rotate_locked(&self.dir, &mut guard)?;
            self.size.store(0, Ordering::Relaxed);
        }
        if let Some(f) = guard.as_mut() {
            f.write_all(bytes)?;
            self.size.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// 在持锁状态下做轮转:关 → rename → 重开。失败时把 file 放回 None 的位置
    /// 重新打开一个空的 current(KTD8 的「降级为单文件截断」)。
    fn rotate_locked(dir: &Path, file: &mut Option<std::fs::File>) -> std::io::Result<()> {
        let current = log_path_in(dir);
        let backup = backup_log_path(dir);
        // 1) 取出并 drop 当前的 file:Windows rename 需要源句柄关闭。
        *file = None;
        // 2) 删 .1(若有);失败忽略——后面 rename 会覆盖(Windows 不行,需先删)。
        let _ = std::fs::remove_file(&backup);
        // 3) rename。
        if let Err(e) = std::fs::rename(&current, &backup) {
            // 降级:重新打开空 current(原来的 current 没被 rename,内容仍在)。
            *file = Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .read(true)
                    .open(&current)?,
            );
            return Err(e);
        }
        // 4) 重新打开新 current(空文件)。
        *file = Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(&current)?,
        );
        Ok(())
    }

    /// 取得当前级别字符串(供 `read_info` / IPC 序列化用)。
    pub fn current_level_str(&self) -> String {
        Level::from_u8(self.level.load(Ordering::SeqCst))
            .as_str()
            .to_string()
    }
}

/// `init` 的返回类型(IO error 的 thin alias,U2 内部统一签名)。
pub type InitResult<T> = Result<T, std::io::Error>;

/// 初始化文件日志(R1):打开文件、处理陈旧大文件,安装全局 sink。
///
/// 失败(权限/磁盘满/`APPDATA` 缺失等)不阻塞启动——应用继续运行,后续
/// `log::*!` 宏会变成 no-op(`log` crate 的默认行为),但 `eprintln!` 调用站
/// 仍可工作(U3 完成迁移前 18 处 `eprintln!` 全部存在;U3 之后要看兜底,
/// 见 `LOG_FALLBACK` 静态)。
///
/// `level` 是**用户设置级别**的共享原子,AppState 与 LogState 共持同一份。
pub fn init(path: &Path, level: Arc<AtomicU8>) -> InitResult<()> {
    let state = Arc::new(LogState::open(path, level.clone())?);
    install_global_logger(state, &level);
    Ok(())
}

#[cfg(not(test))]
fn install_global_logger(state: Arc<LogState>, level: &Arc<AtomicU8>) {
    use std::sync::OnceLock;
    static LOG_ADAPTER: OnceLock<()> = OnceLock::new();
    // `log::set_boxed_logger` 进程级唯一;只能 install 一次。
    // 重复调用 init(测试)时直接跳过,不再尝试接管。
    if LOG_ADAPTER.set(()).is_ok()
        && log::set_boxed_logger(Box::new(LoggerAdapter { state })).is_err()
    {
        eprintln!("[clipvault] 警告: 全局 log logger 已被设置;文件日志未接管");
    }
    log::set_max_level(
        Level::from_u8(level.load(Ordering::SeqCst))
            .log_level_upper_bound()
            .to_level_filter(),
    );
}

/// 测试构建:不安装到 log crate 全局,避免并行测试时 `set_boxed_logger` 互撞。
/// U3 静态检查(`rg "eprintln!"` 为空)不依赖全局路由;`LogState` 的 I/O 行为
/// 通过 `read_info` / `read_tail` / `clear` 间接验证。
#[cfg(test)]
fn install_global_logger(_state: Arc<LogState>, _level: &Arc<AtomicU8>) {}

/// `log::Log` trait 的适配器:把全局宏路由到 `LogState::write_line`。
#[cfg(not(test))]
struct LoggerAdapter {
    state: Arc<LogState>,
}

#[cfg(not(test))]
impl log::Log for LoggerAdapter {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let user_max =
            Level::from_u8(self.state.level.load(Ordering::SeqCst)).log_level_upper_bound();
        metadata.level() <= user_max
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let line = format_line(
            now_ms,
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        // 写失败 best-effort:不向 stderr 喷(那会与用户预期冲突);
        // 由 I/O 测试与 `read_info` 间接验证。
        let _ = self.state.write_line(&line);
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.state.file.lock()
            && let Some(f) = guard.as_mut()
        {
            let _ = f.flush();
        }
    }
}

/// 纯函数层:在给定的用户级别下,目标记录级别是否应被写出。
pub fn should_emit(level: &AtomicU8, target: log::Level) -> bool {
    let user_max = Level::from_u8(level.load(Ordering::SeqCst)).log_level_upper_bound();
    target <= user_max
}

/// 把 `Level` 持久化到 `settings` 表(KTD5,与 `privacy.rs` 同形)。
pub async fn persist_log_level(
    storage: &crate::storage::Storage,
    level: Level,
) -> rusqlite::Result<()> {
    storage.set_setting(LOG_LEVEL_KEY, level.as_str()).await
}

/// 从 `settings` 表读取并 clamp 到 `Level`(缺省/非法值 → `Info`)。
pub async fn load_log_level(storage: &crate::storage::Storage) -> rusqlite::Result<Level> {
    let raw = storage.get_setting(LOG_LEVEL_KEY).await?;
    Ok(parse_level(raw.as_deref()))
}

/// 启动路径:从 `settings` 表加载 → 写入共享原子 → 返回该值。
/// 在 spawn watcher 前调用,使 AppState 原子与持久化值对齐。
pub async fn boot_load_log_level(
    storage: &crate::storage::Storage,
    atomic: &AtomicU8,
) -> rusqlite::Result<Level> {
    let level = load_log_level(storage).await?;
    atomic.store(level as u8, Ordering::SeqCst);
    Ok(level)
}

/// 运行时应用日志级别:更新共享原子(即生效)+ 同步 `log::set_max_level`。
pub fn set_max_level(atomic: &AtomicU8, level: Level) {
    atomic.store(level as u8, Ordering::SeqCst);
    log::set_max_level(level.log_level_upper_bound().to_level_filter());
}

/// 写入路径(KTD5):更新共享原子 + 持久化到 `settings` 表(跨重启保留)。
pub async fn apply_log_level(
    storage: &crate::storage::Storage,
    atomic: &AtomicU8,
    level: Level,
) -> rusqlite::Result<()> {
    set_max_level(atomic, level);
    persist_log_level(storage, level).await
}

/// `read_log` / `read_info` 用的 DTO,前端 `LogInfo` 序列化。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogInfo {
    /// 当前文件字节数(U3 显示用 + 触发轮转的判断依据)。
    pub size_bytes: u64,
    /// `.1` 备份字节数;为 0 时 UI 不显示「旧日志」行。
    pub backup_size_bytes: u64,
    /// 用户级别字符串:`"info"` / `"verbose"`。
    pub level: String,
    /// 日志文件绝对路径,供 UI 显示与复制。
    pub path: String,
}

/// 收集给 IPC 用的元信息(R11 / U4)。
pub fn read_info(path: &Path, atomic: &AtomicU8) -> LogInfo {
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    let backup_size_bytes = std::fs::metadata(backup_log_path(dir))
        .map(|m| m.len())
        .unwrap_or(0);
    let level = Level::from_u8(atomic.load(Ordering::SeqCst))
        .as_str()
        .to_string();
    LogInfo {
        size_bytes,
        backup_size_bytes,
        level,
        path: path.to_string_lossy().to_string(),
    }
}

/// 读取尾部(`.1` + current 拼接,按时间顺序),复用 U1 的 `tail_lines` 做 cap。
///
/// 仅 `.1` 存在或仅 current 存在时按各自情况退化;U2 测试覆盖。
pub fn read_tail(dir: &Path, max_bytes: usize, max_lines: usize) -> std::io::Result<String> {
    let current_path = log_path_in(dir);
    let backup_path = backup_log_path(dir);
    // 读 .1(若有),再读 current(若有)。空文件 = 空 Vec。
    let mut combined: Vec<u8> = Vec::new();
    if let Ok(mut f) = std::fs::File::open(&backup_path) {
        use std::io::Read;
        let _ = f.read_to_end(&mut combined);
    }
    if let Ok(mut f) = std::fs::File::open(&current_path) {
        use std::io::Read;
        let _ = f.read_to_end(&mut combined);
    }
    Ok(tail_lines(&combined, max_bytes, max_lines))
}

/// 清空日志(R12):截断 current 到 0 字节 + 删除 `.1` 备份。
///
/// 注意:**不能**简单 `set_len(0)` 整个 `LogState` 持有的 file 句柄——IPC 路径
/// 没有该句柄,只能通过文件系统层操作。轮转中把 file 句柄拿出又放回的设计
/// 保证 U2 / U4 / `clear` 三处的"截断"语义一致:看 `LogState::write_line` 路径。
pub fn clear(dir: &Path) -> std::io::Result<()> {
    let current = log_path_in(dir);
    let backup = backup_log_path(dir);
    // 1) 截断 current
    let f = std::fs::OpenOptions::new().write(true).open(&current)?;
    f.set_len(0)?;
    drop(f);
    // 2) 删除 .1(若存在)。NotFound 视为成功。
    match std::fs::remove_file(&backup) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
