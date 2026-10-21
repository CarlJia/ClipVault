//! 多类型粘贴板捕获（U2 / R18）。
//!
//! 把 MVP `Watcher::poll_change` 返回 `Option<String>` 的契约替换为
//! 返回 `Option<Captured>`,枚举 `filenames → image → rtf → color → string`
//! 优先级取首个非空。后续 `Watcher::process` 直接消费 `Captured`,
//! 文本走 `classify()`,其他 kind 跳过 classify,`auto_category` 与
//! `content_kind` 同名（image / file / rtf / color）。
//!
//! ## 平台边界
//!
//! `poll_first_non_empty` 每个目标平台各有一份实现:macOS 读 `NSPasteboard`,
//! Windows 读 Win32 剪贴板格式(见下方 `mod win`),其余平台返回 `None`。
//! 三者的签名与契约完全一致,`Watcher::process` 及下游全部平台无关;
//! 单元测试只驱动 `Watcher::process` 与纯转换函数,不触碰真实剪贴板。
//!
//! ## 多类型优先级
//!
//! 同一复制事件在 NSPasteboard 上常同时存在多个 type（截图通常 PNG +
//! TIFF + RTF + string 都有）。`poll_first_non_empty` 按
//! `filenames → image → rtf → color → string` 顺序探测,首个非空胜出。
//! 这是 session-settled KTD8,理由是文件名引用是用户最可能丢失的
//! 可恢复信息,图片次之,字符串兜底。
//!
//! ## 文件 payload 编码
//!
//! `Captured::payload` 对 file 类型是 **JSON 序列化的路径数组**（UTF-8
//! bytes, serde_json 编码 `Vec<String>`）。pasteback 与 FTS 都从
//! `metadata_json.paths` 读取路径;`payload` 保留同一份 JSON 字节
//! 用于直接写入 NSPasteboard,避免双重表达漂移。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use clipvault_core::ContentKind;

use crate::storage::Storage;

/// 一次 NSPasteboard 轮询的多类型捕获结果。
///
/// 文本（`kind = Text`）的载荷在 `text` 字段；二进制（`Image` / `File` /
/// `Rtf` / `Color`）的载荷在 `payload` BLOB,并携带结构化 `metadata_json`。
/// `mime` 仅 binary 填写（如 `image/png`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Captured {
    pub kind: ContentKind,
    pub mime: Option<String>,
    pub text: Option<String>,
    pub payload: Option<Vec<u8>>,
    pub metadata: Value,
}

impl Captured {
    /// 构造纯文本捕获（最常见的 MVP 路径）。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: ContentKind::Text,
            mime: None,
            text: Some(text.into()),
            payload: None,
            metadata: Value::Null,
        }
    }

    /// 构造图片捕获：bytes + dimensions。`metadata` 形如
    /// `{width, height, size_bytes}`;`thumb` 字段由 U3 thumbnail 路径
    /// 在 `Watcher::process` 内追加,不在 capture 层。
    pub fn image(mime: impl Into<String>, bytes: Vec<u8>, width: u32, height: u32) -> Self {
        let size_bytes = bytes.len() as u64;
        Self {
            kind: ContentKind::Image,
            mime: Some(mime.into()),
            text: None,
            payload: Some(bytes),
            metadata: json!({
                "width": width,
                "height": height,
                "size_bytes": size_bytes,
            }),
        }
    }

    /// 构造文件路径数组捕获：paths 是绝对路径或相对路径（已
    /// canonicalize 的标 `resolved = true`）。`payload` 是同一份
    /// paths 的 JSON 字节（与 `metadata.paths` 同源）。
    pub fn file(paths: Vec<String>, resolved: bool) -> Self {
        let payload = serde_json::to_vec(&paths).unwrap_or_default();
        let metadata = json!({
            "paths": paths,
            "resolved": resolved,
        });
        Self {
            kind: ContentKind::File,
            mime: Some("text/uri-list".into()),
            text: None,
            payload: Some(payload),
            metadata,
        }
    }

    /// 构造 RTF 捕获：原始 RTF 字节;无结构化 metadata（R22: RTF 不入
    /// FTS,只需 raw bytes 用于 pasteback 写 `.rtf`）。
    ///
    /// `text` 是随 RTF 一起给出的**纯文本兜底**，存进 `metadata_json.text` 而不是
    /// `content`：`entries_fts` 的 INSERT 触发器对非 file 的二进制条目收的就是
    /// `NEW.content`（`clipvault-core/src/schema.rs`），文本一旦进了 `content` 就会被
    /// 顺手变成可搜索内容，与 R22「rtf 不入 FTS」冲突。`metadata_json` 才是二进制类型
    /// 放描述性内容的地方 —— color 的 `rgba` 就在那里。
    ///
    /// 没有它会怎样：回贴时剪贴板上只有 `Rich Text Format`，而 Win32 剪贴板没有 macOS
    /// 那种格式自动转换，记事本、终端这些只认 `CF_UNICODETEXT` 的接收方拿到的就是空。
    pub fn rtf(bytes: Vec<u8>, text: Option<String>) -> Self {
        let mut metadata = serde_json::Map::new();
        if let Some(t) = text.as_deref().filter(|t| !t.is_empty()) {
            metadata.insert("text".into(), Value::String(t.to_string()));
        }
        Self {
            kind: ContentKind::Rtf,
            mime: Some("application/rtf".into()),
            text: None,
            payload: Some(bytes),
            metadata: Value::Object(metadata),
        }
    }

    /// 构造颜色捕获：RGBA 文本描述（如 `rgba(255,0,0,1)`）。
    pub fn color(rgba: impl Into<String>) -> Self {
        Self {
            kind: ContentKind::Color,
            mime: None,
            text: None,
            payload: None,
            metadata: json!({ "rgba": rgba.into() }),
        }
    }

    /// 入库时的指纹。同 MVP `hash_prefix`,但 binary 加 `kind:` 前缀
    /// 避免跨 kind 冲突（KTD8）——**前缀之后必须再喂 payload 字节**:
    /// 只哈希前缀会让同 kind 的所有内容得到同一个指纹,第二次捕获直接撞
    /// `entries.content_hash` 的 UNIQUE 约束而被丢弃（见下方回归测试）。
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        match (&self.text, &self.payload) {
            (Some(t), _) => hasher.update(format!("text:{}", t).as_bytes()),
            (None, Some(payload)) => {
                hasher.update(format!("{}:", self.kind.as_str()).as_bytes());
                hasher.update(payload);
            }
            (None, None) => hasher.update(format!("color:{}", self.metadata).as_bytes()),
        }
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(16);
        for byte in digest.iter().take(8) {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }
}

/// 在 `Storage::insert_entry_binary` / `insert_entry` 之间分发,
/// 把 `Captured` 落库（U2 + U1 集成）。
///
/// `auto_category` 对非文本类型取 `content_kind.as_str()`;文本类型
/// 走 `classify()` 启发式（URL/Email/path/code/JSON/plain）。
///
/// 性能优化 W1 / R7:`fingerprint` 由调用方算一次后传入,这里不再算——
/// 同一份大 payload 走两次 SHA-256(20MB PNG ≈ 30-50ms 纯浪费)是审计
/// 找出来的最大纯收益之一。watcher 在第 233 行已经算过一次,直接传进来。
pub async fn insert_captured(
    storage: &Storage,
    captured: &Captured,
    auto_category: &str,
) -> Result<i64, rusqlite::Error> {
    let metadata = captured.metadata.to_string();
    let fingerprint = captured.fingerprint();
    insert_captured_with_metadata(storage, captured, auto_category, &metadata, &fingerprint).await
}

/// Like [`insert_captured`] but takes a pre-rendered metadata JSON string,
/// letting the caller splice in derived fields (e.g. `thumb` from U3).
///
/// `fingerprint` 由调用方算好后传入(W1 / R7):整条 watcher 流水线里
/// `captured.fingerprint()` 必须只调一次。
pub async fn insert_captured_with_metadata(
    storage: &Storage,
    captured: &Captured,
    auto_category: &str,
    metadata_json: &str,
    fingerprint: &str,
) -> Result<i64, rusqlite::Error> {
    let hash = fingerprint;
    match captured.kind {
        ContentKind::Text => {
            let text = captured.text.as_deref().unwrap_or("");
            storage.insert_entry(hash, text, auto_category).await
        }
        _ => {
            let payload = captured.payload.as_deref().unwrap_or(&[]);
            storage
                .insert_entry_binary(
                    hash,
                    captured.kind.as_str(),
                    captured.mime.as_deref(),
                    payload,
                    None, // payload_nonce (U6 vault 加密后再写入)
                    Some(metadata_json),
                )
                .await
        }
    }
}

// ---------------------------------------------------------------
// macOS 真实读取
// ---------------------------------------------------------------

/// 单次 NSPasteboard 轮询:按 `filenames → image → rtf → color → string`
/// 顺序探测,首个非空胜出。返回 `None` 表示粘贴板未变化或所有 type 都空。
///
/// 仅 macOS 实现。非 macOS 平台 `Watcher` 仍走 MVP 文本路径——本函数
/// 不参与非 macOS 的 capture 测试。
#[cfg(target_os = "macos")]
pub fn poll_first_non_empty(change_count: i64, prev_change_count: i64) -> Option<Captured> {
    if change_count <= prev_change_count {
        return None;
    }
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardTypeColor, NSPasteboardTypePNG, NSPasteboardTypeRTF,
        NSPasteboardTypeString, NSPasteboardTypeTIFF,
    };

    let pb = NSPasteboard::generalPasteboard();

    // 1. filenames: 经 `readObjectsForClasses(_:options:)` 以 NSURL 读回文件引用
    if let Some(paths) = poll_filenames(&pb) {
        return Some(paths);
    }

    // 2. image: 优先 PNG,缺则 TIFF;维度由 PNG/TIFF 头字节解析
    if let Some(bytes) = unsafe { pb.dataForType(NSPasteboardTypePNG) }.map(|d| d.to_vec()) {
        let (w, h) = png_dimensions(&bytes).unwrap_or((0, 0));
        return Some(Captured::image("image/png", bytes, w, h));
    }
    if let Some(bytes) = unsafe { pb.dataForType(NSPasteboardTypeTIFF) }.map(|d| d.to_vec()) {
        let (w, h) = tiff_dimensions(&bytes).unwrap_or((0, 0));
        return Some(Captured::image("image/tiff", bytes, w, h));
    }

    // 3. rtf（顺带取一份纯文本兜底：RTF 只对富文本接收方有用）
    if let Some(bytes) = unsafe { pb.dataForType(NSPasteboardTypeRTF) }.map(|d| d.to_vec()) {
        let plain = unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string());
        return Some(Captured::rtf(bytes, plain));
    }

    // 4. color: 反归档取出真实 sRGB 分量（U2 的长度占位实现已由 U2 收口单元替换）。
    if let Some(bytes) = unsafe { pb.dataForType(NSPasteboardTypeColor) }.map(|d| d.to_vec()) {
        return Some(color_capture(&bytes));
    }

    // 5. string 兜底（MVP 文本路径）
    if let Some(s) = unsafe { pb.stringForType(NSPasteboardTypeString) } {
        return Some(Captured::text(s.to_string()));
    }

    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn poll_first_non_empty(_change_count: i64, _prev_change_count: i64) -> Option<Captured> {
    None
}

// ---------------------------------------------------------------
// Windows 真实读取（U1）
// ---------------------------------------------------------------

/// 单次剪贴板轮询的 Windows 实现。
///
/// 与 macOS 侧同一契约,但格式映射不同 —— Win32 剪贴板是一组编号格式,没有
/// `NSPasteboard` 那种「同一份内容挂多种类型」之外的额外约定:
///
/// | 优先级 | 剪贴板格式 | macOS 对应物 | 产出的 kind |
/// |---|---|---|---|
/// | 1 | `CF_HDROP` | `NSFilenamesPboardType` | File |
/// | 2 | 注册格式 `PNG` | `NSPasteboardTypePNG` | Image(`image/png`) |
/// | 3 | `CF_DIBV5` / `CF_DIB` | `NSPasteboardTypeTIFF`(同为「非 PNG 的位图回退」) | Image(`image/png`,转码后) |
/// | 4 | 注册格式 `Rich Text Format` | `NSPasteboardTypeRTF` | Rtf |
/// | 5 | `CF_UNICODETEXT` | `NSPasteboardTypeString` | Text |
///
/// 两处**刻意的差异**:
/// - 图片回退到 DIB 后转码成 PNG 再落库(macOS 保留 TIFF 原样)。理由:Windows 上
///   只有 DIB 的复制源非常多,而 `thumbnail.rs` / 前端 `<img>` / `payload` 三处下游
///   都按 PNG 消费,保留 DIB 会让这些条目没有缩略图。转码细节见 [`crate::winclip`]。
/// - **不产生 `Color` 条目**:Win32 剪贴板没有颜色类型(颜色通常以文本或应用私有格式
///   出现),因此 `ContentKind::Color` 在 Windows 采集侧永远不会被构造。前端 `color`
///   行的渲染分支保留不动 —— 它服务的是历史库里的 macOS 条目与可跨平台拷贝的同一份库文件。
#[cfg(target_os = "windows")]
mod win {
    use std::sync::OnceLock;
    use std::time::Duration;

    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use windows::core::PCWSTR;

    use super::{Captured, png_dimensions};

    /// `OpenClipboard` 的有限重试次数。
    ///
    /// 剪贴板是进程级独占资源,别的应用(另一家剪贴板管理器、正在复制的下载工具)会短暂
    /// 持有它。此时 `OpenClipboard` 直接失败 —— 真实占用通常只持续几毫秒,所以在这里
    /// 做几次短重试,比把整轮采集丢掉(下一个 100ms 才回来)体验好得多。上界刻意压得
    /// 很小:采集跑在 tokio 任务里,阻塞超过一个轮询周期就会挤压下一轮。
    ///
    /// 写侧(`pasteback::imp_win`)用的是同一份实现但更宽裕的预算 —— 那边的调用由用户
    /// 操作触发,宁可多等几十毫秒也不要把剪贴板写失败。
    pub(crate) const OPEN_ATTEMPTS: u32 = 5;
    pub(crate) const OPEN_RETRY_DELAY: Duration = Duration::from_millis(8);

    /// 二进制格式的读取字节上限。`CF_DIB` 是**未压缩**位数据,`winclip` 的像素上限
    /// (2500 万像素)反推出来约 100MB,这里留一点余量;超过这个体积的 DIB 不可能
    /// 通过转码,提前放弃以免白拷一份到内存。
    const MAX_DIB_READ_BYTES: usize = 110 * 1024 * 1024;
    /// 已经是成品格式(PNG / RTF / 文本)的读取上限。watcher 的 25MB payload 上限
    /// 会再筛一次,这里只挡住明显异常的体量。
    const MAX_BINARY_READ_BYTES: usize = 64 * 1024 * 1024;

    /// 读剪贴板:`changeCount` 未增长即返回 `None`,与 macOS 侧判据一致。
    pub fn poll_first_non_empty(change_count: i64, prev_change_count: i64) -> Option<Captured> {
        if change_count <= prev_change_count {
            return None;
        }
        if !open_clipboard_with_retry(OPEN_ATTEMPTS, OPEN_RETRY_DELAY) {
            // 剪贴板被别的进程占着:本轮放弃,下一轮再来(不报错、不影响序列号推进)。
            return None;
        }
        // 全部探测在同一次 Open/Close 之间完成:临界区里反复开关既慢,又给别人留出
        // 插队抢走剪贴板、让我们读到半新半旧内容的窗口。
        let captured = first_non_empty();
        // `CloseClipboard` 失败没有可挽回的后续动作,忽略返回值,但**必须**调用 ——
        // 不关闭会让全系统无法使用剪贴板。
        let _ = unsafe { CloseClipboard() };
        captured
    }

    /// 有限重试地打开剪贴板。成功返回 `true`；全部失败返回 `false`。
    ///
    /// 读写两侧共用([`crate::pasteback`] 的写路径也调它),所以重试预算由调用方给。
    /// `hwnd` 传 `NULL` 表示归当前任务所有(不需要窗口句柄)。
    pub(crate) fn open_clipboard_with_retry(attempts: u32, retry_delay: Duration) -> bool {
        for attempt in 0..attempts {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return true;
            }
            if attempt + 1 < attempts {
                std::thread::sleep(retry_delay);
            }
        }
        false
    }

    /// 按优先级取首个非空格式。必须在已打开的剪贴板临界区内调用。
    fn first_non_empty() -> Option<Captured> {
        read_files()
            .or_else(read_image)
            .or_else(read_rtf)
            .or_else(read_text)
    }

    /// `CF_HDROP`:资源管理器「复制」的文件列表。
    fn read_files() -> Option<Captured> {
        let handle = unsafe { GetClipboardData(CF_HDROP.0 as u32) }.ok()?;
        // 直接把剪贴板给的 HGLOBAL 当 `HDROP` 用。**不要**调 `DragFinish`:
        // 那个句柄归剪贴板所有,释放它会破坏系统剪贴板内容。
        let hdrop = HDROP(handle.0);
        // ifile 传 0xFFFFFFFF 时返回条目数。
        let count = unsafe { DragQueryFileW(hdrop, 0xFFFF_FFFF, None) };
        if count == 0 {
            return None;
        }

        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            // lpszfile 传 NULL 时返回该路径所需字符数(不含结尾 NUL)。
            let need = unsafe { DragQueryFileW(hdrop, index, None) };
            if need == 0 {
                continue;
            }
            let mut buf = vec![0u16; need as usize + 1];
            let written = unsafe { DragQueryFileW(hdrop, index, Some(&mut buf)) };
            if written == 0 {
                continue;
            }
            buf.truncate(written as usize);
            paths.push(String::from_utf16_lossy(&buf));
        }
        if paths.is_empty() {
            return None;
        }

        // 与 macOS 侧同义:canonicalize 成功用规范路径,失败保留原串并标 `resolved = false`。
        // Windows 的 canonicalize 返回 `\\?\` 扩展长度前缀,入库前剥掉(见 winclip)。
        let mut all_resolved = true;
        let resolved = paths
            .iter()
            .map(|p| match std::fs::canonicalize(p) {
                Ok(canonical) => {
                    crate::winclip::strip_verbatim_prefix(&canonical.to_string_lossy())
                }
                Err(_) => {
                    all_resolved = false;
                    p.clone()
                }
            })
            .collect();
        Some(Captured::file(resolved, all_resolved))
    }

    /// 图片:优先注册格式 `PNG`,回退 `CF_DIBV5` → `CF_DIB`。
    fn read_image() -> Option<Captured> {
        // 浏览器、截图工具、图像编辑器通常直接挂一份合法 PNG,零转码。
        if let Some(format) = png_format()
            && let Some(bytes) = read_bytes(format, MAX_BINARY_READ_BYTES)
            && !bytes.is_empty()
        {
            let (w, h) = png_dimensions(&bytes).unwrap_or((0, 0));
            return Some(Captured::image("image/png", bytes, w, h));
        }

        // 回退:裸位数据。`CF_DIBV5` 带 alpha 掩码,优先于 `CF_DIB`。
        // 顺带说明:系统会在 `CF_BITMAP` 与 `CF_DIB` 之间自动转换,所以只挂
        // `CB_BITMAP` 的复制源也能在这里读到 DIB。
        for format in [CF_DIBV5.0 as u32, CF_DIB.0 as u32] {
            if let Some(bytes) = read_bytes(format, MAX_DIB_READ_BYTES)
                && let Some((png, width, height)) = crate::winclip::dib_to_png(&bytes)
            {
                return Some(Captured::image("image/png", png, width, height));
            }
        }
        None
    }

    /// RTF:注册格式 `Rich Text Format`(Word / WordPad / 多数富文本编辑器)。
    ///
    /// 顺带读一份 `CF_UNICODETEXT` 作为纯文本兜底:Word 这类富文本应用总是两种格式
    /// 一起挂,而纯文本那份才是记事本 / 终端这类接收方认的东西(见 `Captured::rtf`)。
    /// 只挂 RTF 的复制源(理论存在)拿不到文本,`text` 留 `None`,行为与修复前一致。
    fn read_rtf() -> Option<Captured> {
        let format = rtf_format()?;
        let bytes = read_bytes(format, MAX_BINARY_READ_BYTES)?;
        if bytes.is_empty() {
            return None;
        }
        Some(Captured::rtf(bytes, read_plain_text()))
    }

    /// 纯文本兜底。`CF_UNICODETEXT` 一定存在:系统会在 `CF_TEXT` / `CF_OEMTEXT` /
    /// `CF_UNICODETEXT` 之间自动转换,所以只挂旧格式的复制源也能读到。
    ///
    /// RTF 分支与纯文本分支共用它 —— 两边读的是同一份 `CF_UNICODETEXT`。
    fn read_plain_text() -> Option<String> {
        let bytes = read_bytes(CF_UNICODETEXT.0 as u32, MAX_BINARY_READ_BYTES)?;
        // `as_chunks` 而不是 `chunks_exact`：后者在 clippy 1.99 起会报
        // chunks_exact_to_as_chunks。两者都会丢掉末尾不足一组的字节，语义一致
        // （剪贴板文本按 UTF-16 对齐，正常不会出现奇数长度）。
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        // 结尾的 NUL 是格式的一部分,不是内容。
        Some(
            String::from_utf16_lossy(&units)
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    /// 纯文本兜底分支(优先级最低)。见 [`read_plain_text`]。
    fn read_text() -> Option<Captured> {
        Some(Captured::text(read_plain_text()?))
    }

    /// 读一个剪贴板格式的全部字节。
    ///
    /// 拿到的是**全局内存句柄**,所有权仍归剪贴板:只锁不释放(释放会破坏剪贴板)。
    /// 注意到不了 `max_bytes` 的格式直接判为不可用,避免把异常巨大的数据拷进内存。
    fn read_bytes(format: u32, max_bytes: usize) -> Option<Vec<u8>> {
        unsafe {
            // 格式不存在时 `GetClipboardData` 返回 Err,这正是「这个优先级没内容」的信号。
            let handle = GetClipboardData(format).ok()?;
            let hglobal = HGLOBAL(handle.0);
            let ptr = GlobalLock(hglobal);
            if ptr.is_null() {
                return None;
            }
            let len = GlobalSize(hglobal);
            let bytes = if len > 0 && len <= max_bytes {
                Some(std::slice::from_raw_parts(ptr as *const u8, len).to_vec())
            } else {
                None
            };
            // 返回值忽略:对 GMEM_MOVEABLE 内存,最后一次 `GlobalUnlock` 会返回 FALSE
            // 并把 last error 置为 NO_ERROR(这是成功),windows-rs 把它映射成 Err。
            // 因此这里不能把 Err 当失败处理。
            let _ = GlobalUnlock(hglobal);
            bytes
        }
    }

    /// 注册格式 `PNG` 的编号(进程内缓存一次)。
    ///
    /// 写侧(`pasteback::imp_win`)也用:回贴图片时除 `CF_DIB` 外要挂一份注册格式 `PNG`,
    /// 让现代应用拿到带 alpha 的原图。缓存只此一份,免得两端各自注册、各自缓存。
    pub(crate) fn png_format() -> Option<u32> {
        static CACHE: OnceLock<Option<u32>> = OnceLock::new();
        *CACHE.get_or_init(|| register_format("PNG"))
    }

    /// 注册格式 `Rich Text Format` 的编号(进程内缓存一次)。
    ///
    /// 用注册名而不是 `CF_RTF` 常量:后者从来不是一个标准剪贴板格式编号,
    /// 富文本一律通过 `RegisterClipboardFormatW("Rich Text Format")` 挂载。
    pub(crate) fn rtf_format() -> Option<u32> {
        static CACHE: OnceLock<Option<u32>> = OnceLock::new();
        *CACHE.get_or_init(|| register_format("Rich Text Format"))
    }

    fn register_format(name: &str) -> Option<u32> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // 失败返回 0。这个调用是全局原子表查询,成功与否与当前剪贴板内容无关,
        // 所以可以在临界区里安全调用。
        match unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) } {
            0 => None,
            id => Some(id),
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::poll_first_non_empty;

/// 写侧(`pasteback` 的 Windows 实现)复用同一个「有限重试地打开剪贴板」。
///
/// 剪贴板临界区的获取策略只留一处,免得读写两侧各写一份重试逻辑、各自漂移出不同的
/// 超时行为。重试预算由调用方给:采集侧压得紧(不能阻塞一个轮询周期),写侧可以从容些。
#[cfg(target_os = "windows")]
pub(crate) use win::{open_clipboard_with_retry, png_format, rtf_format};

// ---------------------------------------------------------------
// macOS helpers
// ---------------------------------------------------------------

/// 颜色捕获:把 `NSPasteboardTypeColor` 的归档还原成 `rgba(r,g,b,a)` 文本。
///
/// **颜色条目的身份必须来自颜色内容,不能来自「归档有多长」。** `metadata.rgba` 不只是
/// 前端色块的 CSS 值与回贴时写进剪贴板的文本,它还是 [`Captured::fingerprint`] 在
/// color 分支上唯一的内容来源。曾经这里放的是归档字节长度占位(`nskeyed:{}B`),于是
/// **归档长度相同的两个颜色指纹相同**:实测红色与绿色的 NSColor 归档都是 3603 字节,
/// 「先复制红、再复制绿」时绿会撞 `entries.content_hash` 的 UNIQUE 约束被静默丢弃 ——
/// 与图片/文件侧同一个缺陷,只是发生在最后一个还没收口的类型上。
///
/// 解不开归档的颜色(图案色等转不出 sRGB 的)退回**字节摘要**而不是长度:占位串本身
/// 不可读无所谓(这种颜色前端本来就画不出色块),但绝不能丢掉「不同内容 → 不同指纹」,
/// 否则同一种静默丢弃会换个形式回来。
#[cfg(target_os = "macos")]
fn color_capture(bytes: &[u8]) -> Captured {
    match rgba_from_archived(bytes) {
        Some(rgba) => Captured::color(rgba),
        None => Captured::color(format!("nskeyed:{}", bytes_digest(bytes))),
    }
}

/// NSColor 归档字节 → `rgba(r,g,b,a)`;读不出来返回 `None`。
///
/// 用**安全**反归档 `unarchivedObjectOfClass:fromData:error:` 而不是
/// `unarchiveObjectWithData:`:剪贴板是任意进程都能写的不可信输入,非安全反归档会
/// 实例化归档里声明的任意类;指定 `NSColor` 作根类后,归档里出现别的类会直接失败 ——
/// 那正是我们要的结果(失败即走摘要兜底)。
///
/// 实测(本机 macOS):AppKit 自己写出的颜色归档、以及旧式非安全 API 写出的归档,
/// 这套安全解码都能读出;`colorUsingColorSpace:` 对图案色返回 nil。
#[cfg(target_os = "macos")]
fn rgba_from_archived(bytes: &[u8]) -> Option<String> {
    use objc2::ClassType;
    use objc2_app_kit::{NSColor, NSColorSpace};
    use objc2_foundation::{NSData, NSKeyedUnarchiver};

    let data = NSData::with_bytes(bytes);
    let object = unsafe {
        NSKeyedUnarchiver::unarchivedObjectOfClass_fromData_error(NSColor::class(), &data)
    }
    .ok()?;
    let color = object.downcast::<NSColor>().ok()?;
    // 语义色(动态系统色)与图案色在 sRGB 里没有固定分量,这里返回 nil。
    let srgb = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;

    // 夹到 0–255:Display P3 等广色域转 sRGB 会略微越界。分量是 `CGFloat`(64 位平台上
    // 就是 f64),这里不写多余的 `as f64` —— clippy::unnecessary_cast 会拦。
    let r = (srgb.redComponent().clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = (srgb.greenComponent().clamp(0.0, 1.0) * 255.0).round() as u8;
    let b = (srgb.blueComponent().clamp(0.0, 1.0) * 255.0).round() as u8;
    let alpha = srgb.alphaComponent().clamp(0.0, 1.0);
    // 不透明写 `1`;否则最多两位小数并去掉尾随 0(`0.5` / `0` / `0.13`)。
    let alpha = if alpha >= 1.0 {
        "1".to_string()
    } else {
        let text = format!("{alpha:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    };

    Some(format!("rgba({r},{g},{b},{alpha})"))
}

/// 归档字节的 SHA-256 前 8 字节(16 位十六进制)。与 [`Captured::fingerprint`] 同形,
/// 便于一眼认出这是内容摘要而不是长度占位。
#[cfg(target_os = "macos")]
fn bytes_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();

    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// 读 `NSFilenamesPboardType` 得到文件 URL 数组,再转成 `Captured::file`。
///
/// 走 `readObjectsForClasses(_:options:)` 而**不是**自己解 property list:Apple
/// 文档明确推荐这个 API 读粘贴板上的文件引用,系统负责消化
/// `NSFilenamesPboardType` 历史上用过的多种编码(NSKeyedArchiver bplist、XML
/// plist、更早的 NSArchiver),我们只把每个 `NSURL` 的路径取出来。
///
/// 历史教训:这里曾经直接读 `dataForType` 的字节再"扫可打印 ASCII 片段"当作
/// 路径(见 git 历史里的 `parse_filenames_plist`),对 NSKeyedArchiver bplist 几乎
/// 拿不回完整路径,于是 file 优先级被整个旁路 —— 用户可见症状就是"Finder 复制
/// 文件不进历史"。
///
/// 拿不到数组(None 或空)、或每个元素都 downcast 失败 / 非文件 URL 时返回
/// `None`,让 `poll_first_non_empty` 继续走 image → rtf → color → string。
#[cfg(target_os = "macos")]
fn poll_filenames(pb: &objc2_app_kit::NSPasteboard) -> Option<Captured> {
    use objc2::ClassType;
    use objc2_foundation::{NSArray, NSURL};

    // SAFETY: 该 API 要求 class_array 是有效的 ObjC 类数组、options 类型正确。
    // 两者都满足:class_array 只放 `NSURL` 的类对象,options 传 None(不附加
    // 读取选项 —— 我们要的就是粘贴板上全部文件 URL)。
    let class_array = NSArray::from_slice(&[NSURL::class()]);
    let objects = unsafe { pb.readObjectsForClasses_options(&class_array, None) }?;

    // 数组元素是 `AnyObject`,逐个 downcast 到 `NSURL`;不是 NSURL 的条目丢弃
    // (同一粘贴板上不会混入别的类型,但 downcast 是唯一安全的取类型方式)。
    let urls: Vec<_> = (0..objects.count())
        .filter_map(|index| objects.objectAtIndex(index).downcast::<NSURL>().ok())
        .collect();

    paths_from_urls(&urls)
}

/// 把已 downcast 好的文件 URL 转成 `Captured::file`(纯函数,便于单测)。
///
/// 只认真正的文件 URL:`isFileURL()` 为 false 的 URL 先被跳过。这一层过滤不能省 ——
/// `readObjectsForClasses(_:options:)` 会返回粘贴板上的**任意** `NSURL`,包括浏览器
/// "拷贝链接"写下的 `public.url`(如 `https://example.com/page`);而层级 URL 的
/// `path()` 不是 nil(那个例子得到 `/page`,只有 origin 时得到 `""`)。少了这道过滤,
/// 一个链接会被采集成一条假 File 条目,而本该捕获的 URL 文本被 filenames 优先级挡掉、
/// 永久丢失。
///
/// `canonicalize` 成功用规范路径;失败保留 `NSURL` 给的路径并把 `resolved` 标成
/// `false`(多类型 plan Assumptions:拿不到规范路径不是丢弃这条条目的理由)。
/// `isFileURL()` 为 true 但 `path()` 仍返回 nil 的条目同样跳过;全部被跳过时返回
/// `None`,与 "filenames 这一优先级没内容"同义。
#[cfg(target_os = "macos")]
fn paths_from_urls(urls: &[objc2::rc::Retained<objc2_foundation::NSURL>]) -> Option<Captured> {
    let mut all_resolved = true;
    let mut resolved = Vec::with_capacity(urls.len());

    for url in urls {
        // 非文件 URL(public.url 等)必须在取 path 之前排除。
        if !url.isFileURL() {
            continue;
        }
        let Some(path) = url.path() else {
            continue;
        };
        let path = path.to_string();
        match std::fs::canonicalize(&path)
            .ok()
            .and_then(|canonical| canonical.to_str().map(str::to_string))
        {
            Some(canonical) => resolved.push(canonical),
            None => {
                resolved.push(path);
                all_resolved = false;
            }
        }
    }

    if resolved.is_empty() {
        return None;
    }
    Some(Captured::file(resolved, all_resolved))
}

/// 从图片字节（PNG/TIFF）解析 (width, height)。失败时返回 (0, 0)。
/// 真实 ImageIO `CGImageSourceCopyPropertiesAtIndex` 留作后续增强
/// （U3 thumbnail 路径已引入 ImageIO 间接调用）。`#[allow(dead_code)]`
/// 因该函数是公共扩展点(U2 解析 + U3 缩略图可能都会需要);当前 U2
/// 路径直接调 `png_dimensions` / `tiff_dimensions`,该 wrapper 暂时
/// 无人调用,但作为头字节维度解析的统一入口保留。
#[allow(dead_code)]
fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    png_dimensions(bytes).or_else(|| tiff_dimensions(bytes))
}

/// PNG 头字节解析: 8-byte signature + IHDR chunk with width/height big-endian u32.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// TIFF 头字节解析（little-endian magic II*\x2a → 0x002A = width offset 4）;
/// little-endian TIFF 取 bytes 4..8 / 8..12; big-endian (MM) 取同样偏移。
fn tiff_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 12 {
        return None;
    }
    let (le, off) = match &bytes[..2] {
        b"II" => (true, 4usize),
        b"MM" => (false, 4usize),
        _ => return None,
    };
    let w = if le {
        u32::from_le_bytes(bytes[off..off + 4].try_into().ok()?)
    } else {
        u32::from_be_bytes(bytes[off..off + 4].try_into().ok()?)
    };
    let h = if le {
        u32::from_le_bytes(bytes[off + 4..off + 8].try_into().ok()?)
    } else {
        u32::from_be_bytes(bytes[off + 4..off + 8].try_into().ok()?)
    };
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_captured_round_trip() {
        let c = Captured::text("hello");
        assert_eq!(c.kind, ContentKind::Text);
        assert_eq!(c.text.as_deref(), Some("hello"));
        assert!(c.payload.is_none());
        assert!(c.fingerprint().len() == 16);
    }

    #[test]
    fn file_captured_serializes_paths_into_payload() {
        let c = Captured::file(vec!["/a.pdf".into(), "/b.csv".into()], true);
        let payload = c.payload.as_ref().unwrap();
        let back: Vec<String> = serde_json::from_slice(payload).unwrap();
        assert_eq!(back, vec!["/a.pdf", "/b.csv"]);
        assert_eq!(c.metadata["paths"][0], "/a.pdf");
        assert_eq!(c.metadata["resolved"], true);
    }

    #[test]
    fn color_captured_carries_rgba_only() {
        let c = Captured::color("rgba(255,0,0,1)");
        assert_eq!(c.kind, ContentKind::Color);
        assert_eq!(c.metadata["rgba"], "rgba(255,0,0,1)");
        assert!(c.payload.is_none());
    }

    /// 真实粘贴板颜色归档(sRGB 0.2/0.6/0.9/1.0 的 `NSKeyedArchiver` bplist)。
    ///
    /// 生成方式与生产同形:把该 `NSColor` 写进一个私有 `NSPasteboard`,再读
    /// `NSPasteboardTypeColor` 的字节。夹具写死而不是测试里现造,是为了让用例只测我们的
    /// 解码路径,不依赖 AppKit 的写侧 API。
    #[cfg(target_os = "macos")]
    const NSCOLOR_SRGB_ARCHIVE_B64: &str = concat!(
        "YnBsaXN0MDDUAQIDBAUGBwpYJHZlcnNpb25ZJGFyY2hpdmVyVCR0b3BYJG9iamVjdHMSAAGGoF8Q",
        "D05TS2V5ZWRBcmNoaXZlctEICVRyb290gAGmCwwZHyAnVSRudWxs1g0ODxAREhMUFRYXGFYkY2xh",
        "c3NcTlNDb21wb25lbnRzVU5TUkdCXE5TQ29sb3JTcGFjZV8QEk5TQ3VzdG9tQ29sb3JTcGFjZV8Q",
        "EE5TTGluZWFyRXhwb3N1cmWABU0wLjIgMC42IDAuOSAxTxAnMC4xNjU1NTAzMzYyIDAuNTIwMDI5",
        "MTg3MiAwLjg3NDQyMjA3MzQAEAGAAkEw0xobDRwdHlROU0lEVU5TSUNDEAeAA4AETxEMSAAADEhM",
        "aW5vAhAAAG1udHJSR0IgWFlaIAfOAAIACQAGADEAAGFjc3BNU0ZUAAAAAElFQyBzUkdCAAAAAAAA",
        "AAAAAAAAAAD21gABAAAAANMtSFAgIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAAAEWNwcnQAAAFQAAAAM2Rlc2MAAAGEAAAAbHd0cHQAAAHwAAAAFGJrcHQAAAIE",
        "AAAAFHJYWVoAAAIYAAAAFGdYWVoAAAIsAAAAFGJYWVoAAAJAAAAAFGRtbmQAAAJUAAAAcGRtZGQA",
        "AALEAAAAiHZ1ZWQAAANMAAAAhnZpZXcAAAPUAAAAJGx1bWkAAAP4AAAAFG1lYXMAAAQMAAAAJHRl",
        "Y2gAAAQwAAAADHJUUkMAAAQ8AAAIDGdUUkMAAAQ8AAAIDGJUUkMAAAQ8AAAIDHRleHQAAAAAQ29w",
        "eXJpZ2h0IChjKSAxOTk4IEhld2xldHQtUGFja2FyZCBDb21wYW55AABkZXNjAAAAAAAAABJzUkdC",
        "IElFQzYxOTY2LTIuMQAAAAAAAAAAAAAAEnNSR0IgSUVDNjE5NjYtMi4xAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABYWVogAAAAAAAA81EAAQAAAAEWzFhZ",
        "WiAAAAAAAAAAAAAAAAAAAAAAWFlaIAAAAAAAAG+iAAA49QAAA5BYWVogAAAAAAAAYpkAALeFAAAY",
        "2lhZWiAAAAAAAAAkoAAAD4QAALbPZGVzYwAAAAAAAAAWSUVDIGh0dHA6Ly93d3cuaWVjLmNoAAAA",
        "AAAAAAAAAAAWSUVDIGh0dHA6Ly93d3cuaWVjLmNoAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAAAAAAAAAAAAGRlc2MAAAAAAAAALklFQyA2MTk2Ni0yLjEgRGVmYXVsdCBSR0Ig",
        "Y29sb3VyIHNwYWNlIC0gc1JHQgAAAAAAAAAAAAAALklFQyA2MTk2Ni0yLjEgRGVmYXVsdCBSR0Ig",
        "Y29sb3VyIHNwYWNlIC0gc1JHQgAAAAAAAAAAAAAAAAAAAAAAAAAAAABkZXNjAAAAAAAAACxSZWZl",
        "cmVuY2UgVmlld2luZyBDb25kaXRpb24gaW4gSUVDNjE5NjYtMi4xAAAAAAAAAAAAAAAsUmVmZXJl",
        "bmNlIFZpZXdpbmcgQ29uZGl0aW9uIGluIElFQzYxOTY2LTIuMQAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAdmlldwAAAAAAE6T+ABRfLgAQzxQAA+3MAAQTCwADXJ4AAAABWFlaIAAAAAAATAlWAFAA",
        "AABXH+dtZWFzAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAAACjwAAAAJzaWcgAAAAAENSVCBjdXJ2",
        "AAAAAAAABAAAAAAFAAoADwAUABkAHgAjACgALQAyADcAOwBAAEUASgBPAFQAWQBeAGMAaABtAHIA",
        "dwB8AIEAhgCLAJAAlQCaAJ8ApACpAK4AsgC3ALwAwQDGAMsA0ADVANsA4ADlAOsA8AD2APsBAQEH",
        "AQ0BEwEZAR8BJQErATIBOAE+AUUBTAFSAVkBYAFnAW4BdQF8AYMBiwGSAZoBoQGpAbEBuQHBAckB",
        "0QHZAeEB6QHyAfoCAwIMAhQCHQImAi8COAJBAksCVAJdAmcCcQJ6AoQCjgKYAqICrAK2AsECywLV",
        "AuAC6wL1AwADCwMWAyEDLQM4A0MDTwNaA2YDcgN+A4oDlgOiA64DugPHA9MD4APsA/kEBgQTBCAE",
        "LQQ7BEgEVQRjBHEEfgSMBJoEqAS2BMQE0wThBPAE/gUNBRwFKwU6BUkFWAVnBXcFhgWWBaYFtQXF",
        "BdUF5QX2BgYGFgYnBjcGSAZZBmoGewaMBp0GrwbABtEG4wb1BwcHGQcrBz0HTwdhB3QHhgeZB6wH",
        "vwfSB+UH+AgLCB8IMghGCFoIbgiCCJYIqgi+CNII5wj7CRAJJQk6CU8JZAl5CY8JpAm6Cc8J5Qn7",
        "ChEKJwo9ClQKagqBCpgKrgrFCtwK8wsLCyILOQtRC2kLgAuYC7ALyAvhC/kMEgwqDEMMXAx1DI4M",
        "pwzADNkM8w0NDSYNQA1aDXQNjg2pDcMN3g34DhMOLg5JDmQOfw6bDrYO0g7uDwkPJQ9BD14Peg+W",
        "D7MPzw/sEAkQJhBDEGEQfhCbELkQ1xD1ERMRMRFPEW0RjBGqEckR6BIHEiYSRRJkEoQSoxLDEuMT",
        "AxMjE0MTYxODE6QTxRPlFAYUJxRJFGoUixStFM4U8BUSFTQVVhV4FZsVvRXgFgMWJhZJFmwWjxay",
        "FtYW+hcdF0EXZReJF64X0hf3GBsYQBhlGIoYrxjVGPoZIBlFGWsZkRm3Gd0aBBoqGlEadxqeGsUa",
        "7BsUGzsbYxuKG7Ib2hwCHCocUhx7HKMczBz1HR4dRx1wHZkdwx3sHhYeQB5qHpQevh7pHxMfPh9p",
        "H5Qfvx/qIBUgQSBsIJggxCDwIRwhSCF1IaEhziH7IiciVSKCIq8i3SMKIzgjZiOUI8Ij8CQfJE0k",
        "fCSrJNolCSU4JWgllyXHJfcmJyZXJocmtyboJxgnSSd6J6sn3CgNKD8ocSiiKNQpBik4KWspnSnQ",
        "KgIqNSpoKpsqzysCKzYraSudK9EsBSw5LG4soizXLQwtQS12Last4S4WLkwugi63Lu4vJC9aL5Ev",
        "xy/+MDUwbDCkMNsxEjFKMYIxujHyMioyYzKbMtQzDTNGM38zuDPxNCs0ZTSeNNg1EzVNNYc1wjX9",
        "Njc2cjauNuk3JDdgN5w31zgUOFA4jDjIOQU5Qjl/Obw5+To2OnQ6sjrvOy07azuqO+g8JzxlPKQ8",
        "4z0iPWE9oT3gPiA+YD6gPuA/IT9hP6I/4kAjQGRApkDnQSlBakGsQe5CMEJyQrVC90M6Q31DwEQD",
        "REdEikTORRJFVUWaRd5GIkZnRqtG8Ec1R3tHwEgFSEtIkUjXSR1JY0mpSfBKN0p9SsRLDEtTS5pL",
        "4kwqTHJMuk0CTUpNk03cTiVObk63TwBPSU+TT91QJ1BxULtRBlFQUZtR5lIxUnxSx1MTU19TqlP2",
        "VEJUj1TbVShVdVXCVg9WXFapVvdXRFeSV+BYL1h9WMtZGllpWbhaB1pWWqZa9VtFW5Vb5Vw1XIZc",
        "1l0nXXhdyV4aXmxevV8PX2Ffs2AFYFdgqmD8YU9homH1YklinGLwY0Njl2PrZEBklGTpZT1lkmXn",
        "Zj1mkmboZz1nk2fpaD9olmjsaUNpmmnxakhqn2r3a09rp2v/bFdsr20IbWBtuW4SbmtuxG8eb3hv",
        "0XArcIZw4HE6cZVx8HJLcqZzAXNdc7h0FHRwdMx1KHWFdeF2Pnabdvh3VnezeBF4bnjMeSp5iXnn",
        "ekZ6pXsEe2N7wnwhfIF84X1BfaF+AX5ifsJ/I3+Ef+WAR4CogQqBa4HNgjCCkoL0g1eDuoQdhICE",
        "44VHhauGDoZyhteHO4efiASIaYjOiTOJmYn+imSKyoswi5aL/IxjjMqNMY2Yjf+OZo7OjzaPnpAG",
        "kG6Q1pE/kaiSEZJ6kuOTTZO2lCCUipT0lV+VyZY0lp+XCpd1l+CYTJi4mSSZkJn8mmia1ZtCm6+c",
        "HJyJnPedZJ3SnkCerp8dn4uf+qBpoNihR6G2oiailqMGo3aj5qRWpMelOKWpphqmi6b9p26n4KhS",
        "qMSpN6mpqhyqj6sCq3Wr6axcrNCtRK24ri2uoa8Wr4uwALB1sOqxYLHWskuywrM4s660JbSctRO1",
        "irYBtnm28Ldot+C4WbjRuUq5wro7urW7LrunvCG8m70VvY++Cr6Evv+/er/1wHDA7MFnwePCX8Lb",
        "w1jD1MRRxM7FS8XIxkbGw8dBx7/IPci8yTrJuco4yrfLNsu2zDXMtc01zbXONs62zzfPuNA50LrR",
        "PNG+0j/SwdNE08bUSdTL1U7V0dZV1tjXXNfg2GTY6Nls2fHadtr724DcBdyK3RDdlt4c3qLfKd+v",
        "4DbgveFE4cziU+Lb42Pj6+Rz5PzlhOYN5pbnH+ep6DLovOlG6dDqW+rl63Dr++yG7RHtnO4o7rTv",
        "QO/M8Fjw5fFy8f/yjPMZ86f0NPTC9VD13vZt9vv3ivgZ+Kj5OPnH+lf65/t3/Af8mP0p/br+S/7c",
        "/23//9IhIiMkWiRjbGFzc25hbWVYJGNsYXNzZXNcTlNDb2xvclNwYWNloiUmXE5TQ29sb3JTcGFj",
        "ZVhOU09iamVjdNIhIigpV05TQ29sb3KiKCYACAARABoAJAApADIANwBJAEwAUQBTAFoAYABtAHQA",
        "gQCHAJQAqQC8AL4AzAD2APgA+gD8AQMBCAEOARABEgEUDWANZQ1wDXkNhg2JDZYNnw2kDawAAAAA",
        "AAACAQAAAAAAAAAqAAAAAAAAAAAAAAAAAAANrw==",
    );

    #[cfg(target_os = "macos")]
    fn nscolor_fixture() -> Vec<u8> {
        use base64::Engine;

        base64::engine::general_purpose::STANDARD
            .decode(NSCOLOR_SRGB_ARCHIVE_B64)
            .expect("夹具必须是合法 base64")
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn color_capture_decodes_to_srgb_rgba() {
        // 回归:修前 color 的身份是归档字节长度占位(`nskeyed:{}B`),而实测红与绿的
        // NSColor 归档都是 3603 字节 —— 等长即同指纹,第二个颜色会被 content_hash
        // UNIQUE 静默丢弃,和图片侧是同一个缺陷。
        let captured = color_capture(&nscolor_fixture());
        assert_eq!(captured.kind, ContentKind::Color);
        assert_eq!(
            captured.metadata["rgba"], "rgba(51,153,230,1)",
            "身份必须来自解码出的颜色内容,而不是归档有多长"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn color_capture_falls_back_to_a_content_digest() {
        // 解不开归档的颜色也不能退回长度占位:两个**等长**但内容不同的垃圾输入必须
        // 得到不同指纹(长度占位会让它们相同,静默丢弃就换个形式回来了)。
        let a = color_capture(b"not a color archive!!");
        let b = color_capture(b"not a color archive??");
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert!(
            a.metadata["rgba"]
                .as_str()
                .is_some_and(|s| s.starts_with("nskeyed:")),
            "兜底仍是归档摘要占位串"
        );
    }

    #[test]
    fn image_captured_records_dimensions_and_size() {
        // 1x1 transparent PNG
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
        let c = Captured::image("image/png", png.to_vec(), 1, 1);
        assert_eq!(c.kind, ContentKind::Image);
        assert_eq!(c.metadata["width"], 1);
        assert_eq!(c.metadata["height"], 1);
        assert_eq!(c.metadata["size_bytes"], png.len() as u64);
    }

    #[test]
    fn fingerprint_differs_across_kinds_for_same_payload() {
        let bytes = b"identical-bytes".to_vec();
        let text = Captured::text("identical-bytes");
        let file = Captured::file(vec!["identical-bytes".into()], false);
        let image = Captured::image("image/png", bytes.clone(), 1, 1);
        let rtf = Captured::rtf(bytes, None);
        assert_ne!(text.fingerprint(), file.fingerprint());
        assert_ne!(file.fingerprint(), image.fingerprint());
        assert_ne!(image.fingerprint(), rtf.fingerprint());
        assert_ne!(rtf.fingerprint(), text.fingerprint());
    }

    #[test]
    fn fingerprint_differs_within_kind_for_different_payloads() {
        // 回归：binary 分支曾只把 kind 名喂进摘要,同 kind 的第二次捕获会得到
        // 完全相同的指纹,撞 entries.content_hash UNIQUE 后被静默丢弃 ——
        // 用户可见症状就是「复制图片不进历史」(只有第一张进得去)。
        let img_a = Captured::image("image/png", b"png-bytes-a".to_vec(), 1, 1);
        let img_b = Captured::image("image/png", b"png-bytes-b".to_vec(), 1, 1);
        assert_ne!(
            img_a.fingerprint(),
            img_b.fingerprint(),
            "同 kind 不同 payload 必须得到不同指纹"
        );

        let file_a = Captured::file(vec!["/tmp/a.txt".into()], true);
        let file_b = Captured::file(vec!["/tmp/b.txt".into()], true);
        assert_ne!(file_a.fingerprint(), file_b.fingerprint());

        let rtf_a = Captured::rtf(b"{\\rtf1 a}".to_vec(), None);
        let rtf_b = Captured::rtf(b"{\\rtf1 b}".to_vec(), None);
        assert_ne!(rtf_a.fingerprint(), rtf_b.fingerprint());

        // 指纹必须仍然是 payload 的确定函数:5 秒去重窗口靠它判「同一条内容」,
        // 换个不稳定来源(时间戳、地址)会让去重整体失效。
        assert_eq!(
            img_a.fingerprint(),
            Captured::image("image/png", b"png-bytes-a".to_vec(), 1, 1).fingerprint(),
            "同 kind 同 payload 的指纹必须稳定"
        );
    }

    #[test]
    fn png_dimensions_parses_1x1() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";
        assert_eq!(png_dimensions(png), Some((1, 1)));
    }

    #[test]
    fn png_dimensions_rejects_jpeg_signature() {
        let jpeg = b"\xff\xd8\xff\xe0\x00\x10JFIF";
        assert!(png_dimensions(jpeg).is_none());
    }

    #[test]
    fn tiff_dimensions_parses_le_2x3() {
        // II* little-endian, width=2, height=3
        let mut tiff = vec![b'I', b'I', 0x2a, 0x00];
        tiff.extend(2u32.to_le_bytes());
        tiff.extend(3u32.to_le_bytes());
        assert_eq!(tiff_dimensions(&tiff), Some((2, 3)));
    }

    #[test]
    fn tiff_dimensions_parses_be_4x5() {
        let mut tiff = vec![b'M', b'M', 0x00, 0x2a];
        tiff.extend(4u32.to_be_bytes());
        tiff.extend(5u32.to_be_bytes());
        assert_eq!(tiff_dimensions(&tiff), Some((4, 5)));
    }

    /// 构造一个文件 URL，作为 [`paths_from_urls`] 的测试输入
    /// （生产侧这个数组由 `readObjectsForClasses(_:options:)` 产出）。
    #[cfg(target_os = "macos")]
    fn url(path: &str) -> objc2::rc::Retained<objc2_foundation::NSURL> {
        use objc2_foundation::{NSString, NSURL};
        NSURL::fileURLWithPath(&NSString::from_str(path))
    }

    /// 解出 file payload 里的路径数组（与 `Captured::file` 的编码同源）。
    #[cfg(target_os = "macos")]
    fn payload_paths(captured: &Captured) -> Vec<String> {
        serde_json::from_slice(captured.payload.as_deref().unwrap_or(&[])).unwrap()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_resolves_existing_paths() {
        let dir = std::env::temp_dir();
        let dir_str = dir.to_str().expect("临时目录必须是 UTF-8");
        let expected = std::fs::canonicalize(&dir)
            .expect("临时目录必须存在")
            .to_str()
            .expect("规范路径必须是 UTF-8")
            .to_string();

        let captured = paths_from_urls(&[url(dir_str)]).expect("存在的目录应产出 Captured");

        assert_eq!(captured.kind, ContentKind::File);
        assert_eq!(payload_paths(&captured), vec![expected]);
        assert_eq!(captured.metadata["resolved"], true);
        assert_eq!(captured.mime.as_deref(), Some("text/uri-list"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_unresolvable_keeps_original_with_resolved_false() {
        let missing = "/nonexistent/path/that/does/not/exist";

        let captured =
            paths_from_urls(&[url(missing)]).expect("canonicalize 失败仍应产出 Captured");

        assert_eq!(payload_paths(&captured), vec![missing.to_string()]);
        assert_eq!(captured.metadata["resolved"], false);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_empty_returns_none() {
        assert!(
            paths_from_urls(&[]).is_none(),
            "空数组必须返回 None，让 watcher 走下一优先级"
        );
    }

    /// 回归:浏览器"拷贝链接"写下的 `public.url`(层级 URL)也是 `NSURL`,且 `path()`
    /// 在它上面不是 nil(`https://example.com/page` → `/page`)。混在文件 URL 里时必须
    /// 被 `isFileURL` 过滤掉,不能当成假文件路径采集。
    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_skips_non_file_urls() {
        use objc2_foundation::{NSString, NSURL};

        let link = NSURL::URLWithString(&NSString::from_str("https://example.com/page"))
            .expect("示例 URL 必须合法");

        let captured =
            paths_from_urls(&[url("/tmp"), link]).expect("至少一个文件 URL 应产出 Captured");

        let expected = std::fs::canonicalize("/tmp")
            .expect("/tmp 必须存在")
            .to_str()
            .expect("规范路径必须是 UTF-8")
            .to_string();
        assert_eq!(captured.kind, ContentKind::File);
        assert_eq!(
            payload_paths(&captured),
            vec![expected],
            "只应命中真正的文件路径,链接的 `/page` 不得混进来"
        );
    }

    /// 回归:粘贴板上只有非文件 URL 时必须返回 `None`,让链路落到 string 优先级去
    /// 捕获 URL 文本 —— 否则 URL 文本永久丢失、只剩一条假 File 条目。
    #[cfg(target_os = "macos")]
    #[test]
    fn paths_from_urls_all_non_file_returns_none() {
        use objc2_foundation::{NSString, NSURL};

        let link = NSURL::URLWithString(&NSString::from_str("https://example.com/page"))
            .expect("示例 URL 必须合法");

        assert!(
            paths_from_urls(&[link]).is_none(),
            "全是非文件 URL 时必须返回 None"
        );
    }

    /// 回归:只有文本(例如 URL 以 `public.utf8-plain-text` 落在粘贴板上)时,
    /// filenames 这一优先级不得命中,免得把普通文本采成 File 条目。
    ///
    /// 用**命名**粘贴板而不是 `generalPasteboard`(同
    /// [`read_objects_round_trips_filenames_written_as_property_list`] 的理由);名字与那个
    /// 测试不同,避免两个用例并发跑时互相 `clearContents` 造成 flake。
    #[cfg(target_os = "macos")]
    #[test]
    fn poll_filenames_returns_none_for_text_only_pasteboard() {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;

        let pb = NSPasteboard::pasteboardWithName(&NSString::from_str("ClipVaultCaptureTextOnly"));
        pb.clearContents();

        let _ = unsafe {
            pb.setString_forType(
                &NSString::from_str("https://example.com/page"),
                NSPasteboardTypeString,
            )
        };

        assert!(
            poll_filenames(&pb).is_none(),
            "只有文本的粘贴板不得被采集为 File 条目"
        );
    }

    #[test]
    fn poll_first_non_empty_returns_none_when_change_count_unchanged() {
        // 非 macOS 路径返回 None;但 macOS 路径也会因 change_count 未增返回 None
        let result = poll_first_non_empty(5, 5);
        assert!(result.is_none());
    }

    /// U1 的核心假设的自动化证明：**一致地**写进 `.filenames` 的
    /// `NSArray<NSString>`（U2 的回贴写侧正是这个形状）能被
    /// `readObjectsForClasses(_:options:)` 以 `NSURL` 读回来、再由
    /// [`paths_from_urls`] 还原成路径数组。
    ///
    /// 这条链路之前在两边都是断的（采集侧扫 ASCII 字节、回贴侧只写纯文本），
    /// 单测各自"通过"却谁也没覆盖"写进去读得回来"。写侧直接调**生产**的
    /// [`crate::pasteback::write_file_paths_to_pasteboard`]，生产写侧一旦漂移这里就会红，
    /// 而不是对着测试里手搭的克隆写入器自说自话。
    ///
    /// 这里用**命名**粘贴板而不是 `generalPasteboard`：测试绝不碰用户正在用的剪贴板。
    /// 命名粘贴板会在本次登录会话里留存（objc2 0.3 未暴露 `releaseGlobally`），但它只装
    /// 两个路径、用户不可见。
    ///
    /// 它证明的是「我们自己的写入形状读得回来」；Finder 复制文件时粘贴板上还有哪些
    /// 附带类型，仍归真机 AE1 验收。
    #[cfg(target_os = "macos")]
    #[test]
    fn read_objects_round_trips_filenames_written_as_property_list() {
        use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
        use objc2_foundation::NSString;

        // 取两个一定存在的路径（macOS 上 /tmp 是指向 /private/tmp 的符号链接，
        // 正好顺带覆盖 canonicalize 这一步）。
        let sources = ["/tmp", "/usr"];

        let pb = NSPasteboard::pasteboardWithName(&NSString::from_str("ClipVaultCaptureTest"));
        pb.clearContents();

        // 走生产写侧（与 `MacosBackend::write_clipboard` 的 File 分支同一份实现）。
        let paths: Vec<String> = sources.iter().map(|p| p.to_string()).collect();
        crate::pasteback::write_file_paths_to_pasteboard(&pb, &paths);

        // 双写契约：文件引用之外还必须无条件挂纯文本表示（路径按 `\n` 拼接），
        // 给只认文本的接收方（此处此前无人证明）。
        let string_repr = pb.stringForType(unsafe { NSPasteboardTypeString });
        assert_eq!(
            string_repr.map(|s| s.to_string()),
            Some(paths.join("\n")),
            "写侧必须同时挂 `\\n` 拼接的纯文本表示"
        );

        let captured = poll_filenames(&pb).expect("刚写进去的文件引用应能被读回");
        assert_eq!(captured.kind, ContentKind::File);
        assert_eq!(captured.metadata["resolved"], true, "两个路径都存在");

        let mut got = payload_paths(&captured);
        got.sort();
        let mut expected: Vec<String> = sources
            .iter()
            .map(|p| {
                std::fs::canonicalize(p)
                    .expect("测试路径必须存在")
                    .to_str()
                    .expect("规范路径必须是 UTF-8")
                    .to_string()
            })
            .collect();
        expected.sort();
        assert_eq!(got, expected);
    }

    #[test]
    fn poll_first_non_empty_returns_none_when_change_count_decreased() {
        let result = poll_first_non_empty(3, 5);
        assert!(result.is_none());
    }

    /// 性能优化 W1 / R7:`insert_captured_with_metadata` 不再自己算
    /// `fingerprint()`,完全由调用方传入。watcher 在第 233 行已经算过
    /// 一次再传进来;这是一条端到端契约——`hash == "caller-provided"`
    /// 才说明我们没有偷跑一次 SHA-256。
    #[tokio::test]
    async fn insert_captured_with_metadata_uses_caller_hash_not_internal_fingerprint() {
        let storage = crate::storage::test_helpers::open_test_db();

        // 抓一个普通文本捕获。
        let captured = Captured::text("caller-supplied hash test");
        let caller_fingerprint = "callerprovided"; // 16 hex chars 风格, 长度任意
        let id = insert_captured_with_metadata(
            &storage,
            &captured,
            "plain",
            r#"{"note":"test"}"#,
            caller_fingerprint,
        )
        .await
        .unwrap();
        assert!(id > 0);

        // 直接读 entries.content_hash:必须等于调用方传入的值,**不应**
        // 等于 captured.fingerprint() 的实际结果(那是另一次 SHA-256)。
        let stored_hash: String = storage
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT content_hash FROM entries WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(
            stored_hash, caller_fingerprint,
            "rows.content_hash 必须严格等于调用方传入的 fingerprint, 等于就说明没偷跑 fingerprint()"
        );

        // 反向证明:captured.fingerprint() 的实际结果必须跟 stored_hash 不同——
        // 否则我们可能只是偷跑了一个等于传值的 fingerprint(), 仍算偷跑。
        let real_fp = captured.fingerprint();
        assert_ne!(
            stored_hash, real_fp,
            "rows.content_hash 绝不能等于 captured.fingerprint() 的真实结果（说明函数真的用 caller 值）"
        );
    }

    /// rtf 捕获必须带一份纯文本兜底，且它落在 `metadata_json` 而不是 `content`：
    /// `entries_fts` 的 INSERT 触发器对非 file 的二进制条目收的就是 `NEW.content`
    /// （`clipvault-core/src/schema.rs`），文本进了 `content` 就会被顺手变成可搜索
    /// 内容，与 R22「rtf 不入 FTS」冲突。
    #[test]
    fn rtf_capture_keeps_plain_text_in_metadata_not_content() {
        let captured = Captured::rtf(b"{\\rtf1 hi}".to_vec(), Some("hi".into()));

        assert_eq!(captured.kind, ContentKind::Rtf);
        assert!(
            captured.text.is_none(),
            "Captured.text 是条目的 content 位，必须留空"
        );
        assert_eq!(captured.metadata["text"], "hi");
    }

    /// 没有纯文本（只挂 RTF 的复制源）或空串时，metadata 里不留 `text` 键 ——
    /// 回贴侧据此只挂 RTF，行为与修复前一致。
    #[test]
    fn rtf_capture_omits_empty_text_fallback() {
        for text in [None, Some(String::new())] {
            let captured = Captured::rtf(b"{\\rtf1 hi}".to_vec(), text);
            assert!(
                captured.metadata.get("text").is_none(),
                "没有可用文本兜底时不该写一个空 text 键"
            );
        }
    }

    /// 端到端：rtf 捕获落库后 `content` 仍是空（不污染 FTS），纯文本在 metadata 里。
    #[tokio::test]
    async fn rtf_capture_round_trips_through_insert() {
        let storage = crate::storage::test_helpers::open_test_db();
        let captured = Captured::rtf(b"{\\rtf1 hi}".to_vec(), Some("hi".into()));
        let metadata_json = serde_json::to_string(&captured.metadata).unwrap();

        let id = insert_captured_with_metadata(
            &storage,
            &captured,
            "rtf",
            &metadata_json,
            &captured.fingerprint(),
        )
        .await
        .unwrap();

        let (content, stored_metadata): (String, Option<String>) = storage
            .with_conn(move |conn| {
                conn.query_row(
                    "SELECT content, metadata_json FROM entries WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .await
            .unwrap();

        assert_eq!(content, "", "rtf 条目的 content 必须留空（R22：不入 FTS）");
        let meta: serde_json::Value =
            serde_json::from_str(stored_metadata.as_deref().unwrap_or("{}")).unwrap();
        assert_eq!(meta["text"], "hi", "纯文本兜底应随条目一起入库");
    }
}
