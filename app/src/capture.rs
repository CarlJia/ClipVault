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
    pub fn rtf(bytes: Vec<u8>) -> Self {
        Self {
            kind: ContentKind::Rtf,
            mime: Some("application/rtf".into()),
            text: None,
            payload: Some(bytes),
            metadata: json!({}),
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
pub async fn insert_captured(
    storage: &Storage,
    captured: &Captured,
    auto_category: &str,
) -> Result<i64, rusqlite::Error> {
    let metadata = captured.metadata.to_string();
    insert_captured_with_metadata(storage, captured, auto_category, &metadata).await
}

/// Like [`insert_captured`] but takes a pre-rendered metadata JSON string,
/// letting the caller splice in derived fields (e.g. `thumb` from U3).
pub async fn insert_captured_with_metadata(
    storage: &Storage,
    captured: &Captured,
    auto_category: &str,
    metadata_json: &str,
) -> Result<i64, rusqlite::Error> {
    let hash = captured.fingerprint();
    match captured.kind {
        ContentKind::Text => {
            let text = captured.text.as_deref().unwrap_or("");
            storage.insert_entry(&hash, text, auto_category).await
        }
        _ => {
            let payload = captured.payload.as_deref().unwrap_or(&[]);
            storage
                .insert_entry_binary(
                    &hash,
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

    // 3. rtf
    if let Some(bytes) = unsafe { pb.dataForType(NSPasteboardTypeRTF) }.map(|d| d.to_vec()) {
        return Some(Captured::rtf(bytes));
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
    fn read_rtf() -> Option<Captured> {
        let format = rtf_format()?;
        let bytes = read_bytes(format, MAX_BINARY_READ_BYTES)?;
        if bytes.is_empty() {
            return None;
        }
        Some(Captured::rtf(bytes))
    }

    /// 纯文本兜底。`CF_UNICODETEXT` 一定存在:系统会在 `CF_TEXT` / `CF_OEMTEXT` /
    /// `CF_UNICODETEXT` 之间自动转换,所以只挂旧格式的复制源也能读到。
    fn read_text() -> Option<Captured> {
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
        let text = String::from_utf16_lossy(&units);
        Some(Captured::text(text.trim_end_matches('\0')))
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
pub(crate) fn color_capture(bytes: &[u8]) -> Captured {
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
pub(crate) fn poll_filenames(pb: &objc2_app_kit::NSPasteboard) -> Option<Captured> {
    use objc2::ClassType;
    use objc2_foundation::{NSArray, NSURL};

    // SAFETY: 该 API 要求 class_array 是有效的 ObjC 类数组、options 类型正确。
    // 两者都满足:class_array 只放 `NSURL` 的类对象,options 传 None(不附加
    // 读取选项 —— 我们要的就是粘贴板上全部文件 URL)。
    let class_array = NSArray::from_slice(&[NSURL::class()]);
    let objects = unsafe { pb.readObjectsForClasses_options(&class_array, None) }?;

    paths_from_urls(&urls_from_objects(&objects))
}

/// 把 `readObjectsForClasses(_:options:)` 返回的异质数组过滤成 `NSURL` 数组。
///
/// 数组元素的静态类型是 `AnyObject`,所以 downcast 是取具体类型唯一安全的方式;
/// 不是 `NSURL` 的元素直接丢弃。
///
/// 这段过滤抽成吃 `&NSArray` 的纯函数,是为了让它**可测**:真实粘贴板走
/// `readObjectsForClasses([NSURL.class])` 时引擎层只返回 `NSURL` 实例,构造不出
/// "数组里混入非 NSURL 元素"的输入,过滤分支就永远没有用例覆盖。手工搭一个
/// 混入 `NSString` 的数组即可驱动它（见 `capture_test::urls_from_objects_*`）。
#[cfg(target_os = "macos")]
pub(crate) fn urls_from_objects(
    objects: &objc2_foundation::NSArray,
) -> Vec<objc2::rc::Retained<objc2_foundation::NSURL>> {
    use objc2_foundation::NSURL;

    (0..objects.count())
        .filter_map(|index| objects.objectAtIndex(index).downcast::<NSURL>().ok())
        .collect()
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
pub(crate) fn paths_from_urls(
    urls: &[objc2::rc::Retained<objc2_foundation::NSURL>],
) -> Option<Captured> {
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
pub(crate) fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

/// TIFF 头字节解析（little-endian magic II*\x2a → 0x002A = width offset 4）;
/// little-endian TIFF 取 bytes 4..8 / 8..12; big-endian (MM) 取同样偏移。
pub(crate) fn tiff_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
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
