//! Accessibility 门控的 CGEvent ⌘V 回贴（U8 / R14 / F2 / AE3 / AE7）。
//!
//! # 结构
//!
//! - **跨平台决策逻辑**（本文件顶部）：[`AccessibilityChecker`] /
//!   [`PasteBackend`] trait、[`perform_paste`] 编排、[`decrypt_content`]
//!   加密区接缝、固定 toast 文案常量。这部分不含任何 macOS 符号，可在任意
//!   平台编译并被 `pasteback_test.rs` 以 mock 驱动两条分支。
//! - **macOS 真实实现**（`imp` 模块，`#[cfg(target_os = "macos")]`）：
//!   [`RealAccessibilityChecker`] 调 `AXIsProcessTrusted`、[`MacosBackend`]
//!   走 NSPasteboard 写入 + CGEvent 模拟 + NSWorkspace 前台应用比对。
//!   **焦点归还**（F2）也在这里：`capture_target_app` 记录唤出面板前的前台应用，
//!   `hide_panel_and_restore_focus` 在隐藏面板后显式把激活权交还它、并有界等待
//!   它真的回到前台——面板是普通 `NSWindow`，`orderOut` 不会自动还激活权。
//! - **Windows 真实实现**（`imp_win` 模块，`#[cfg(target_os = "windows")]`，U3）：
//!   [`imp_win::WindowsBackend`] 走 Win32 剪贴板写入 + `SendInput` 注入 + 前台窗口比对。
//!   结构与本文件同构，三处刻意的差异（还焦点的步骤顺序、注入的事件序列、前台应用标识）
//!   都写在各函数注释里。
//! - **其余平台**：无真实实现；`commands::paste_entry` 在那些目标上直接返回
//!   [`PasteError::UnsupportedPlatform`]。
//!
//! # 为什么真实按键注入不做单测
//!
//! `CGEventPost` 会向系统 HID event tap 注入真实按键、且依赖运行期 TCC
//! Accessibility 授权,无法在无头 CI 中确定性地断言。因此把平台副作用全部
//! 收敛进 [`PasteBackend`] trait,单测只断言 [`perform_paste`] 的**决策分支**
//! (是否写剪贴板、是否触达模拟、失败回退文案),真实注入与真实授权归入
//! AE3 / AE7 的 macOS 真机手动验收。
//!
//! # `AXIsProcessTrustedWithError` 命名说明
//!
//! 计划文案写作 `AXIsProcessTrustedWithError`,但 macOS 公有 API 中并不存在
//! 该符号;可用的是 `AXIsProcessTrusted()`(静默返回当前授权布尔)与
//! `AXIsProcessTrustedWithOptions(CFDictionaryRef)`(可附带系统弹窗选项)。
//! U8 的「每次回贴前重新校验」正是 `AXIsProcessTrusted()` 的语义(防止 major
//! 系统升级后 TCC 缓存失效),故 [`RealAccessibilityChecker`] 调用它;首启引导
//! 走前端深链按钮而非系统弹窗,因此不需要 options 变体。

use std::time::{Duration, Instant};

use clipvault_core::{ContentKind, Entry};

// ---------------- 面向用户的固定文案（平台感知） ----------------
//
// 这些文案里藏着两处平台差异：修饰键（macOS `⌘` / Windows `Ctrl+`）与密钥托管
// （macOS Keychain / Windows 凭据管理器）。按仓库既有惯例（见 `storage::resolve_data_dir`），
// 平台以**显式 bool 参数**传进纯函数、生产调用点传 [`IS_MACOS`] —— 这样 Windows 分支的
// 文案也能在 macOS 上被单测覆盖，而不是等真机跑起来才发现写错。

/// 本进程所在平台是否 macOS。生产调用点用它；纯函数本身只认显式参数。
pub const IS_MACOS: bool = cfg!(target_os = "macos");

/// 修饰键标签：macOS `⌘`，其余平台 `Ctrl+`。
pub fn modifier_label(is_macos: bool) -> &'static str {
    if is_macos { "⌘" } else { "Ctrl+" }
}

/// 目标窗口在回贴后失焦时的固定提示(计划 U8 step 3)。
pub fn window_switched_msg(is_macos: bool) -> String {
    format!(
        "目标窗口已切换,内容已写入剪贴板,请手动 {}V 粘贴",
        modifier_label(is_macos)
    )
}

/// 按键注入失败回退的固定提示(计划 U8 step 4,文案固定)。
///
/// 名字里的 CGEvent 是 macOS 侧的注入手段；Windows 侧是 `SendInput`，文案本身两端通用。
pub fn cgevent_fail_msg(is_macos: bool) -> String {
    format!(
        "回贴失败,内容已复制到剪贴板,请手动 {}V",
        modifier_label(is_macos)
    )
}

/// 未授权 Accessibility 时的提示:内容已进剪贴板,引导用户手动粘贴或开启授权。
///
/// Windows 上 `check_accessibility` 恒为已授权、这条不可达；文案仍按平台走，
/// 免得留下「Windows 上也提示去开 macOS 辅助功能」的隐患。
pub fn no_accessibility_msg(is_macos: bool) -> String {
    format!(
        "已复制到剪贴板,请手动 {}V 粘贴。开启「辅助功能」授权后可自动回贴。",
        modifier_label(is_macos)
    )
}

/// 加密区条目解密失败的提示（U9：密钥托管不可用或密钥不匹配 → 不回贴）。
pub fn vault_decrypt_fail_msg(is_macos: bool) -> String {
    // macOS 侧写英文 `Keychain`（与移植前逐字一致，含其后那个空格）；Windows 侧是中文
    // 「凭据管理器」，中文与「不可用」之间不留空格。
    let store = if is_macos {
        "Keychain "
    } else {
        "凭据管理器"
    };
    format!("加密条目解密失败（{store}不可用或密钥不匹配）")
}

// ---------------- 还焦点的时间策略（F2） ----------------
//
// 面板是普通 `NSWindow`：`set_focus()` 会把 ClipVault 顶成前台 App，用户正在
// 编辑的 App 被 deactivate（输入框光标消失）。而 `window.hide()` 只做 `orderOut`，
// **不会**把激活权交回去。所以「隐藏 → 重新激活目标 App → 等它真的回到前台 →
// 再注入 ⌘V」这条链路是回贴能否落地的前提；下面几个常量是这条链路的全部时间
// 策略，真机手感不对时只调这里，不牵动别处。

/// 等待「目标 App 成为前台应用」的上限。超时不再阻塞回贴，由
/// [`PasteOutcome::WindowSwitched`] 兜底提示手动 ⌘V。
pub const FRONT_WAIT_MAX: Duration = Duration::from_millis(500);
/// 前台应用的轮询间隔。10ms 足以让「激活已完成」在下一次采样中被看到。
pub const FRONT_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// 目标 App 成为前台之后的附加稳定期：让它恢复 key window / first responder
/// （也就是用户那个光标还在闪的输入框），随后注入的 ⌘V 才落得进去。
pub const KEY_WINDOW_SETTLE: Duration = Duration::from_millis(60);
/// 等「隐藏面板」真正生效的上限。`window.hide()` 在非主线程上只是把消息投进
/// 事件循环（异步），要在面板确实 `orderOut` 之后再请求让位。
pub const HIDE_FENCE_MAX: Duration = Duration::from_millis(120);

/// 回贴失败的类型化原因。前端据此区分「显示 Accessibility 横幅」与「toast」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteError {
    /// 进程未获得 Accessibility 授权:不写剪贴板、不模拟按键(AE7)。
    MissingAccessibility,
    /// 加密区条目解密失败（Keychain 不可用 / 密钥不匹配 / nonce 缺失）。
    /// 不写剪贴板、不模拟按键（U9：不把密文当明文贴出）。
    VaultDecryptFailed,
    /// 非 macOS 平台:无回贴能力。
    UnsupportedPlatform,
}

/// 回贴成功路径的结果分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteOutcome {
    /// 内容已写入剪贴板且 ⌘V 已注入、目标应用仍在前台。
    Pasted,
    /// ⌘V 已注入但前台应用已切换:内容在剪贴板,提示用户手动 ⌘V。
    WindowSwitched,
    /// CGEventPost 失败:仅完成剪贴板写入,提示用户手动 ⌘V。
    ClipboardOnly,
    /// 未授权 Accessibility:内容**已写入剪贴板**(写剪贴板不需要该权限),
    /// 但无法模拟 ⌘V 自动回贴。提示用户手动 ⌘V,或去开启辅助功能以自动回贴。
    NoAccessibilityClipboardOnly,
}

/// [`PasteBackend::simulate_paste`] 的失败标记(事件创建/投递失败)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectError;

/// Accessibility 授权检查接缝。真实实现调用 `AXIsProcessTrusted()`;
/// 测试实现返回固定布尔以驱动两条分支。
pub trait AccessibilityChecker {
    /// 进程当前是否已获得 Accessibility 授权。
    fn is_trusted(&self) -> bool;
}

/// 平台副作用接缝:剪贴板写入、失焦前的面板隐藏、⌘V 模拟、前台应用读取。
/// 真机走 macOS 实现;测试用 mock 记录调用轨迹并驱动失败路径。
pub trait PasteBackend {
    /// 写入剪贴板:多类型 plan U4 按 `kind` 分发到 `setString` /
    /// `setData` / `setPropertyList`。
    ///
    /// `content` 仅 `Text` 类型使用;`payload` 是二进制 bytes (image / rtf
    /// / file),`mime` 用于 image 子类型 PNG / JPEG / TIFF 派发。`color`
    /// 走 `content`(rgba 文本)。
    fn write_clipboard(
        &self,
        kind: ContentKind,
        content: &str,
        payload: Option<&[u8]>,
        mime: Option<&str>,
    );
    /// 在模拟按键前把键盘焦点还给目标应用(F2)：隐藏 ClipVault 面板，**显式**
    /// 把激活权交还唤出面板前的前台 App，并等到它真的回到前台。真实实现见
    /// `imp::hide_panel_and_restore_focus`；测试用 mock 只记录是否被调用。
    fn refocus_target(&self);
    /// 通过 CGEvent 模拟 ⌘V。事件创建或投递失败时返回 [`InjectError`]。
    fn simulate_paste(&self) -> Result<(), InjectError>;
    /// 读取当前前台应用标识(bundle id),用于失焦比对。
    fn frontmost_app(&self) -> Option<String>;
}

/// 加密区解密接缝（U9 已实现，U4 多类型 plan 扩展）。
///
/// - 文本条目：直接返回明文 `content`（`nonce` / `decryptor` 被忽略）。
/// - vault 文本条目：用 `decryptor`（Keychain 数据密钥 + AES-GCM）解密
///   `content`（base64 密文）配 `nonce`。
/// - vault 二进制条目：用 `decryptor` 解密 `payload`（BLOB）配
///   `payload_nonce`。Keychain 不可用、密钥不匹配或 nonce 缺失 →
///   返回 [`PasteError::VaultDecryptFailed`]，**不返回错误明文**。
///
/// `nonce` 由调用方（`commands::paste_entry`）在读条目时一并从存储取出。
/// `payload_nonce` 同理；非二进制或非 vault 条目为 `None`。
pub fn decrypt_content(
    entry: &Entry,
    nonce: Option<&[u8]>,
    decryptor: &dyn crate::crypto::Decryptor,
) -> Result<String, PasteError> {
    if entry.is_vault {
        let nonce = nonce.ok_or(PasteError::VaultDecryptFailed)?;
        decryptor
            .decrypt(&entry.content, nonce)
            .map_err(|_| PasteError::VaultDecryptFailed)
    } else {
        Ok(entry.content.clone())
    }
}

/// 解密 vault 二进制条目（U4 multi-type plan）。文本条目走
/// [`decrypt_content`]；非 vault 二进制条目 payload 已是明文。
pub fn decrypt_payload(
    entry: &Entry,
    payload: &[u8],
    payload_nonce: Option<&[u8]>,
    decryptor: &dyn crate::crypto::Decryptor,
) -> Result<Vec<u8>, PasteError> {
    if entry.is_vault {
        let nonce = payload_nonce.ok_or(PasteError::VaultDecryptFailed)?;
        decryptor
            .decrypt_bytes(payload, nonce)
            .map_err(|_| PasteError::VaultDecryptFailed)
    } else {
        Ok(payload.to_vec())
    }
}

/// color 条目的回贴文本:从 `metadata_json` 的 `rgba` 字段取色值(rgb 之外的键忽略)。
///
/// color 是唯一「内容不在 payload 里」的二进制类型 —— 它没有字节可写,能贴出去的就是
/// 这段色值文本。抽成吃 `Option<&str>` 的纯函数,是为了让「metadata 缺字段 / 不是 JSON /
/// 不是对象」这些边界能在任意平台上单测,而不必真机跑一次回贴。
///
/// 取不到时返回空串(与 `content` 为空的其它二进制类型一致,后端照样会写一个空文本)。
pub fn color_paste_text(metadata_json: Option<&str>) -> String {
    metadata_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|meta| {
            meta.get("rgba")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// file 条目的 payload(JSON 编码的 `Vec<String>`,与 `Captured::file` 同源)解出路径数组。
///
/// 抽成纯函数是为了让「payload 缺失 / 不是 JSON / 不是字符串数组」这些边界能在任意平台
/// 单测。任何解不出来的情形都返回空数组 —— 调用方据此跳过写剪贴板,而不是贴出一串乱码。
pub fn file_paths(payload: Option<&[u8]>) -> Vec<String> {
    payload
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(bytes).ok())
        .unwrap_or_default()
}

/// file 条目的**纯文本回退**内容:绝对路径按 `\n` 拼接。
///
/// 文件类型不能只挂 `NSFilenamesPboardType` / `CF_HDROP`:目标应用未必消费文件引用
/// (TextEdit、部分网页输入框只认文本),那时候用户按了回贴却什么都贴不出来。所以两个
/// 平台都**同时**挂"文件引用 + 纯文本"两种表示,由接收方各取所需 —— 见
/// `imp::MacosBackend::write_clipboard` 与 `imp_win::WindowsBackend::write_clipboard`。
///
/// 这里只管 macOS 的 `\n`;Windows 侧的纯文本用 `\r\n` 内联拼接(分隔符是接收方看得见的
/// 序列化差异,不共用一个函数,免得为它长出 `separator` 参数)。
pub fn file_text_fallback(paths: &[String]) -> String {
    paths.join("\n")
}

/// 把一段文本原样写入系统剪贴板(只写剪贴板,不回贴、不碰焦点)。
///
/// 调方是设置面板「诊断日志 → 文件路径」的点击复制(`commands::copy_log_path`)。
/// 抽成吃 `PasteBackend` 的函数,是为了让平台无关单测能用 mock 断言写入的类型与内容
/// (与 [`perform_paste`] 同一手法)。
pub fn copy_text_to_clipboard<B: PasteBackend>(backend: &B, text: &str) {
    backend.write_clipboard(ContentKind::Text, text, None, None);
}

/// 回贴编排(纯决策逻辑,平台副作用全走 trait)。
///
/// 顺序对应计划 U8 Approach，并修正了「未授权时内容也进不了剪贴板」的缺陷：
/// 1. 解析内容(加密区接缝:vault 条目走 `decryptor` 解密,失败 →
///    [`PasteError::VaultDecryptFailed`],**在写剪贴板前**失败,保证密文不落盘剪贴板)。
/// 2. `write_clipboard`——写剪贴板**不需要** Accessibility 权限,因此**先做**,
///    这样即便未授权,用户至少能手动 ⌘V 拿到内容。
/// 3. `is_trusted()` 校验;未授权 → [`PasteOutcome::NoAccessibilityClipboardOnly`]
///    (内容已在剪贴板,只是无法自动回贴),不再整体失败。
/// 4. `refocus_target`(隐藏面板 → 重新激活目标 App → 有界等待它真的回到前台)。
/// 5. `simulate_paste`;失败 → [`PasteOutcome::ClipboardOnly`](内容已在剪贴板)。
/// 6. 用 `frontmost_app` 与 `prev_frontmost` 比对;不一致 →
///    [`PasteOutcome::WindowSwitched`],否则 [`PasteOutcome::Pasted`]。
pub fn perform_paste<C: AccessibilityChecker, B: PasteBackend>(
    checker: &C,
    backend: &B,
    entry: &Entry,
    prev_frontmost: Option<&str>,
    nonce: Option<&[u8]>,
    payload_nonce: Option<&[u8]>,
    decryptor: &dyn crate::crypto::Decryptor,
) -> Result<PasteOutcome, PasteError> {
    // 1. 内容解析(vault 接缝)。在写剪贴板前失败,保证 vault 条目不落盘剪贴板。
    //    - text kind: 走 `decrypt_content`(base64 解码 entry.content 配 nonce)。
    //    - color kind: 色值在 `metadata_json.rgba`,由 `color_paste_text` 取出(无 payload)。
    //    - 其余非 text kind: entry.content 留空;vault 条目需要 `decrypt_payload` 解密
    //      payload BLOB;非 vault 条目 payload 是明文,直接用。
    let kind = ContentKind::parse(&entry.content_kind);
    let text = match kind {
        ContentKind::Text => decrypt_content(entry, nonce, decryptor)?,
        ContentKind::Color => color_paste_text(entry.metadata_json.as_deref()),
        _ => String::new(),
    };
    let payload_bytes: Option<Vec<u8>> = if matches!(kind, ContentKind::Text | ContentKind::Color) {
        // text 的内容在 `content`;color 的色值在 `metadata_json.rgba` —— 两者都没有
        // payload。**color 必须和 text 一样跳过这一段**:它走 vault 时 `payload_nonce`
        // 恒为 None(压根没有 payload 可加密),`decrypt_payload` 会直接
        // `VaultDecryptFailed`,于是加密区里的颜色条目在写剪贴板前就整体失败了。
        None
    } else {
        let raw = entry.payload.as_deref().unwrap_or(&[]);
        Some(decrypt_payload(entry, raw, payload_nonce, decryptor)?)
    };

    // 2. 写剪贴板。写剪贴板不需要 Accessibility 权限,故先于授权检查——
    //    保证即便未授权,内容也已在系统剪贴板,用户可手动 ⌘V。
    backend.write_clipboard(
        kind,
        &text,
        payload_bytes.as_deref(),
        entry.mime_type.as_deref(),
    );

    // 3. 授权校验(每调用重新校验,防 TCC 静默失效)。未授权时内容已在剪贴板,
    //    只是无法模拟 ⌘V,返回降级结果而非整体失败。
    if !checker.is_trusted() {
        return Ok(PasteOutcome::NoAccessibilityClipboardOnly);
    }

    // 4. 隐藏面板,焦点还给目标应用。
    backend.refocus_target();

    // 5. 模拟 ⌘V;失败则回退到「仅剪贴板」。
    if backend.simulate_paste().is_err() {
        return Ok(PasteOutcome::ClipboardOnly);
    }

    // 6. 失焦比对。
    let now_front = backend.frontmost_app();
    match (prev_frontmost, now_front.as_deref()) {
        (Some(prev), Some(cur)) if prev != cur => Ok(PasteOutcome::WindowSwitched),
        _ => Ok(PasteOutcome::Pasted),
    }
}

/// 是否用「当前前台应用」覆盖已记录的回贴目标（记录策略，平台无关故可测）。
///
/// 抽成纯函数是为了让「面板已在前台时再按一次热键」这条边界能被跨平台单测
/// 覆盖，而不必真机去跑 Carbon 回调：
/// - 前台应用是**我们自己**（pid 相同）→ 不覆盖。面板已在前台又按一次热键
///   （或点托盘）是常见操作，把目标记成 ClipVault 会让此后所有回贴的失焦比对
///   永远错判成 `WindowSwitched`。
/// - 读不到前台应用（`None`）→ 也不覆盖，保留上一次的真实目标。
pub fn should_replace_target(front_pid: Option<i32>, self_pid: i32) -> bool {
    matches!(front_pid, Some(pid) if pid != self_pid)
}

/// 有界等待「目标进程成为前台应用」(F2)。
///
/// `read_frontmost_pid` 是平台接缝：macOS 侧传 `NSWorkspace.frontmostApplication()`
/// 的 pid，单测传可控闭包，故本函数留在平台无关层、可被 `pasteback_test.rs` 直测。
///
/// 先采样一次——已经在前台时立刻返回，不做无谓等待；否则按 `poll_interval`
/// 轮询到 `max_wait` 截止，返回是否等到。
///
/// 为什么是轮询而不是「激活 + 固定 sleep」：激活请求是异步的（文档明说
/// "You shouldn't assume the app will be active immediately"），目标 App 忙碌或
/// 正在启动时会晚到若干十毫秒。固定 sleep 要么白等、要么输给它——旧实现
/// `hide()` 后固定 60ms 就注入 ⌘V，正是回贴静默落空的原因。
pub fn wait_for_frontmost<F: Fn() -> Option<i32>>(
    read_frontmost_pid: F,
    target_pid: i32,
    max_wait: Duration,
    poll_interval: Duration,
) -> bool {
    let deadline = Instant::now() + max_wait;
    loop {
        if read_frontmost_pid() == Some(target_pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll_interval);
    }
}

// ================================================================
// macOS 真实实现
// ================================================================

/// `NSPasteboard` 的文件列表类型。它没有 objc2 常量（Apple 从未把它列进
/// `NSPasteboardType*` 一族），只能用字符串字面量取。
#[cfg(target_os = "macos")]
const NSFILENAMES_PBOARD_TYPE: &str = "NSFilenamesPboardType";

/// 把文件路径按 file 条目的双写约定写进一块 `NSPasteboard`(仅 macOS)。
///
/// **生产写侧(`imp::MacosBackend::write_clipboard` 的 `ContentKind::File` 分支)与
/// `capture.rs` 的写读往返测试共用这一份实现** —— 这样「我们写进去的形状读得回来」
/// 由真代码保证,而不是测试里手搭一份可能与生产漂移的克隆写入器。
///
/// 两条写入(详细理由见 `imp::MacosBackend::write_clipboard`):
///   - `paths` 非空时先写 `NSFilenamesPboardType`(`NSArray<NSString>`,文件引用),
///     给 Finder / Slack 上传框这类认文件引用的应用;
///   - 无论 `paths` 是否为空,**无条件**写 `NSPasteboardTypeString`(路径按 `\n`
///     拼接,即 [`file_text_fallback`]),给 TextEdit / 网页输入框这类只认文本的应用。
#[cfg(target_os = "macos")]
pub(crate) fn write_file_paths_to_pasteboard(
    pasteboard: &objc2_app_kit::NSPasteboard,
    paths: &[String],
) {
    use objc2_app_kit::NSPasteboardTypeString;
    use objc2_foundation::NSString;

    if !paths.is_empty() {
        let ns_paths: Vec<objc2::rc::Retained<NSString>> =
            paths.iter().map(|p| NSString::from_str(p)).collect();
        let array = objc2_foundation::NSArray::from_retained_slice(&ns_paths);
        // SAFETY: `cast_unchecked` 只做类型擦除的指针转换，不改变对象的动态类型。
        // `NSArray` 实例转成 `AnyObject`（即 Obj-C 的 `id`）总是合法的；这里转正是因为
        // `setPropertyList_forType` 收 `&AnyObject`。
        let plist: objc2::rc::Retained<objc2::runtime::AnyObject> =
            unsafe { objc2::rc::Retained::cast_unchecked(array) };
        let _ = unsafe {
            pasteboard.setPropertyList_forType(&plist, &NSString::from_str(NSFILENAMES_PBOARD_TYPE))
        };
    }

    let ns = NSString::from_str(&file_text_fallback(paths));
    let _ = unsafe { pasteboard.setString_forType(&ns, NSPasteboardTypeString) };
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
    use objc2_foundation::MainThreadMarker;

    use super::{
        AccessibilityChecker, ContentKind, FRONT_POLL_INTERVAL, FRONT_WAIT_MAX, HIDE_FENCE_MAX,
        InjectError, KEY_WINDOW_SETTLE, PasteBackend, file_paths, should_replace_target,
        wait_for_frontmost, write_file_paths_to_pasteboard,
    };

    // ---- Accessibility ----
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    /// 真实 Accessibility 检查:每次调用即时读取 `AXIsProcessTrusted()`。
    pub struct RealAccessibilityChecker;

    impl AccessibilityChecker for RealAccessibilityChecker {
        fn is_trusted(&self) -> bool {
            unsafe { AXIsProcessTrusted() != 0 }
        }
    }

    /// 捕获**当前**前台应用的 bundle identifier（回贴后用它做失焦比对）。
    pub fn capture_frontmost() -> Option<String> {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        app.bundleIdentifier().map(|s| s.to_string())
    }

    // ---- 回贴目标应用：记录 + 还焦点（F2） ----

    /// 回贴目标应用 = 唤出面板**之前**的前台 App，是「还焦点」的唯一事实来源。
    ///
    /// 存运行实例而不是 bundle id：激活要的就是这个实例，且能顺手查
    /// `isTerminated()`（目标 App 退出后激活必然失败，不必白等一个等待窗口）。
    /// `NSRunningApplication` 在 objc2 绑定里 impl 了 `Send + Sync`，可安全放静态。
    /// 对象常驻不释放是**有意**的：进程内只有一个「上一个前台 App」，泄漏量是常数。
    static TARGET_APP: Mutex<Option<Retained<NSRunningApplication>>> = Mutex::new(None);

    /// 记录回贴目标应用。**必须在 `window.show()` 之前调用**：面板一旦聚焦，
    /// 前台应用就变成 ClipVault 自己，目标 App 再也读不回来。
    /// 由热键回调与托盘 `show_panel` 各调一次。
    ///
    /// 覆盖策略见 [`should_replace_target`]：前台已经是**我们自己**时保留原记录。
    /// 这里用 pid 而不是 bundle id 判断——dev 期跑的是裸二进制（不在 .app 里），
    /// `bundleIdentifier()` 会返回 nil，判不出来。
    pub fn capture_target_app() {
        let front = NSWorkspace::sharedWorkspace().frontmostApplication();
        let self_pid = NSRunningApplication::currentApplication().processIdentifier();
        if !should_replace_target(front.as_ref().map(|app| app.processIdentifier()), self_pid) {
            return;
        }
        if let (Some(front), Ok(mut slot)) = (front, TARGET_APP.lock()) {
            *slot = Some(front);
        }
    }

    /// 目标应用的 bundle identifier（[`perform_paste`] 的失焦比对用）。
    /// 目标是裸二进制（无 bundle id）时返回 `None`，比对退化为「未切换」。
    pub fn target_bundle_id() -> Option<String> {
        target_app()
            .and_then(|app| app.bundleIdentifier())
            .map(|id| id.to_string())
    }

    /// 取当前记录的目标应用（克隆 `Retained`，不跨调用持锁）。
    fn target_app() -> Option<Retained<NSRunningApplication>> {
        TARGET_APP.lock().ok().and_then(|slot| slot.clone())
    }

    /// 隐藏面板并把键盘焦点交还目标应用（F2 的核心，回贴与 Esc 关闭共用）。
    ///
    /// 三步，缺一不可：
    /// 1. `orderOut` 隐藏面板——但**这不会**把激活权还给上一个 App；
    /// 2. 显式激活唤出面板前的前台 App（此刻 ClipVault 自己是前台 App，
    ///    属于「主动让位」，系统会放行）；
    /// 3. 等它真的回到前台，再留一段稳定期让 key window / first responder
    ///    恢复——用户那个光标还在闪的输入框回来了，随后注入的 ⌘V 才落得进去，
    ///    而不是落进没有 key window 的 ClipVault。
    ///
    /// **第 3 步只在非主线程上做**，因为两条调用路径的性质不同：
    /// - 回贴路径（`MacosBackend::refocus_target`，跑在 `spawn_blocking` 线程）：
    ///   紧随其后要注入 ⌘V，必须确认目标已经回到前台，所以完整等待。
    /// - Esc 路径（`commands::hide_panel` 是同步 IPC 命令，跑在**主线程**）：
    ///   只发出让位请求就返回。主线程上绝不能等——轮询会把主 run loop 停转，
    ///   而「前台应用是否已切换」恰恰要靠 run loop 消费窗口服务器消息才观测得到，
    ///   等下去必然超时，还会把 UI 冻住。Esc 之后没有按键要注入，发出请求即达成目的。
    ///
    /// 没有记录 / 目标 App 已退出时退化为「只隐藏」，把激活权交给系统自己移交。
    /// 旧实现固定 `sleep(60ms)` 就注入 ⌘V，正是回贴静默落空的原因。
    pub fn hide_panel_and_restore_focus(window: &tauri::WebviewWindow) -> Result<(), String> {
        window.hide().map_err(|e| e.to_string())?;
        let on_main = MainThreadMarker::new().is_some();

        // 顺序栅栏：只有非主线程才需要。主线程上 `hide()` 是真同步的
        // （tauri 走 `handle_user_message` 就地执行），而跨线程时它只是把
        // `WindowMessage::Hide` 投进事件循环。要在面板确实已经 `orderOut` 之后
        // 才请求让位，否则等于在自己还占着 key window 时要求别人成为前台应用。
        // `is_visible()` 是穿越事件循环的同步往返，FIFO 保证它读到的是 Hide
        // 之后的状态——是顺序栅栏，不是时间猜测。
        if !on_main {
            wait_until_hidden(window, HIDE_FENCE_MAX);
        }

        // 目标缺失或已退出：无法显式让位。
        let Some(target) = target_app().filter(|app| !app.isTerminated()) else {
            return Ok(());
        };

        activate(&target);

        if on_main {
            // 只发请求、不等结果（见函数文档）。
            return Ok(());
        }

        let restored = wait_for_frontmost(
            || {
                NSWorkspace::sharedWorkspace()
                    .frontmostApplication()
                    .map(|app| app.processIdentifier())
            },
            target.processIdentifier(),
            FRONT_WAIT_MAX,
            FRONT_POLL_INTERVAL,
        );
        if !restored {
            // 不在这里失败：⌘V 照常注入，真丢了下游 `perform_paste` 的失焦比对
            // 会返回 WindowSwitched，前端提示用户手动 ⌘V。留一条日志便于排障。
            ::log::warn!(target: "pasteback", "目标应用未在 {FRONT_WAIT_MAX:?} 内回到前台，⌘V 可能落空");
        }
        // 激活完成 ≠ 输入框已恢复，留稳定期再注入按键。
        std::thread::sleep(KEY_WINDOW_SETTLE);
        Ok(())
    }

    /// 有界等待窗口真的不可见（见调用点的「顺序栅栏」说明）。
    /// 超时也继续——最坏情况由 [`wait_for_frontmost`] 的有界等待兜住。
    fn wait_until_hidden(window: &tauri::WebviewWindow, budget: Duration) {
        let deadline = Instant::now() + budget;
        while window.is_visible().unwrap_or(true) {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(FRONT_POLL_INTERVAL);
        }
    }

    /// 请求系统把前台应用切换给 `app`。
    ///
    /// `ActivateIgnoringOtherApps` 在 macOS 14+ 已废弃、flag 被系统忽略（激活
    /// 策略改由系统裁决），但请求本身依旧有效；保留该 flag 是为兼容
    /// `minimumSystemVersion = 13.0` 的旧语义（旧系统上不传可能被拒绝让位）。
    #[allow(deprecated)]
    fn activate(app: &NSRunningApplication) {
        app.activateWithOptions(NSApplicationActivationOptions::ActivateIgnoringOtherApps);
    }

    /// 真实回贴后端:NSPasteboard 写入 + Tauri 面板隐藏 + CGEvent ⌘V + 前台比对。
    ///
    /// 持有 Tauri 主窗口以在模拟按键前隐藏面板(把焦点还给目标应用,F2)。
    pub struct MacosBackend {
        window: tauri::WebviewWindow,
    }

    impl MacosBackend {
        pub fn new(window: tauri::WebviewWindow) -> Self {
            Self { window }
        }
    }

    impl PasteBackend for MacosBackend {
        fn write_clipboard(
            &self,
            kind: ContentKind,
            content: &str,
            payload: Option<&[u8]>,
            mime: Option<&str>,
        ) {
            use objc2_app_kit::{
                NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeRTF, NSPasteboardTypeString,
                NSPasteboardTypeTIFF,
            };
            use objc2_foundation::NSString;

            let pasteboard = NSPasteboard::generalPasteboard();
            pasteboard.clearContents();

            match kind {
                ContentKind::Text => {
                    let ns = NSString::from_str(content);
                    let _ = unsafe { pasteboard.setString_forType(&ns, NSPasteboardTypeString) };
                }
                ContentKind::Image => {
                    let bytes = payload.unwrap_or(&[]);
                    let nsdata = objc2_foundation::NSData::from_vec(bytes.to_vec());
                    let pb_type = unsafe {
                        match mime.unwrap_or("image/png") {
                            "image/tiff" => NSPasteboardTypeTIFF,
                            _ => NSPasteboardTypePNG,
                        }
                    };
                    let _ = pasteboard.setData_forType(Some(&nsdata), pb_type);
                }
                ContentKind::File => {
                    // 双写：同一块剪贴板同时挂「文件引用」与「纯文本路径」，由接收方各取所需。
                    //
                    //   - `.filenames`（`NSFilenamesPboardType` = `NSArray<NSString>`）给
                    //     Finder / Slack 上传框这类认文件引用的应用；
                    //   - `.string`（路径按 `\n` 拼接）给 TextEdit / 网页输入框这类只认
                    //     文本的应用。
                    //
                    // 为什么不是"先写文件引用、写失败再退回文本"：`setPropertyList_forType`
                    // 的返回值只说明**写入剪贴板**是否成功（清空后写入，实际恒为 true），
                    // 它不反映目标应用能否消费文件引用 —— 拿它当降级判据永远不会触发，
                    // 只认文本的应用会什么都贴不出来。Windows 侧同义（`CF_HDROP` +
                    // `CF_UNICODETEXT` 一起挂，见 `imp_win`）。
                    let paths = file_paths(payload);
                    write_file_paths_to_pasteboard(&pasteboard, &paths);
                }
                ContentKind::Rtf => {
                    let bytes = payload.unwrap_or(&[]);
                    let nsdata = objc2_foundation::NSData::from_vec(bytes.to_vec());
                    let _ =
                        unsafe { pasteboard.setData_forType(Some(&nsdata), NSPasteboardTypeRTF) };
                }
                ContentKind::Color => {
                    // color 走 rgba() 文本字符串写入 .string 类型。
                    let ns = NSString::from_str(content);
                    let _ = unsafe { pasteboard.setString_forType(&ns, NSPasteboardTypeString) };
                }
            }
        }

        fn refocus_target(&self) {
            // 隐藏面板 + 把激活权交还目标应用 + 有界等待（见
            // `hide_panel_and_restore_focus` 的三步说明）。失败不外传：
            // 「⌘V 没落到目标应用」已由下游的失焦比对（WindowSwitched）兜底。
            let _ = hide_panel_and_restore_focus(&self.window);
        }

        fn simulate_paste(&self) -> Result<(), InjectError> {
            use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};

            /// `kVK_ANSI_V`。
            const KEY_V: CGKeyCode = 0x09;

            // key-down(V + Cmd flag)。source 传 None 使用组合事件源。
            let key_down = CGEvent::new_keyboard_event(None, KEY_V, true).ok_or(InjectError)?;
            CGEvent::set_flags(Some(&key_down), CGEventFlags::MaskCommand);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&key_down));

            // key-up(V + Cmd flag)。
            let key_up = CGEvent::new_keyboard_event(None, KEY_V, false).ok_or(InjectError)?;
            CGEvent::set_flags(Some(&key_up), CGEventFlags::MaskCommand);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&key_up));

            // 计划 U8 step 3:注入后 ~200ms 内再比对前台应用。
            std::thread::sleep(std::time::Duration::from_millis(180));
            Ok(())
        }

        fn frontmost_app(&self) -> Option<String> {
            capture_frontmost()
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::{
    MacosBackend, RealAccessibilityChecker, capture_frontmost, capture_target_app,
    hide_panel_and_restore_focus, target_bundle_id,
};

// ================================================================
// Windows 真实实现（U3）
// ================================================================
//
// 与 macOS 侧同构：同一组 trait、同一个 `perform_paste` 编排、同一批时间常量。
// 三处**刻意的差异**，都在下面各自的函数注释里写明了理由：
//   1. 还焦点时先激活再隐藏（前台锁），与 macOS 的先隐藏再激活相反；
//   2. 注入的是完整的 Ctrl↓/V↓/V↑/Ctrl↑ 四事件序列，并先等物理修饰键抬起；
//   3. 前台应用标识用进程可执行文件全路径代替 bundle id。
#[cfg(target_os = "windows")]
mod imp_win {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{CloseHandle, GlobalFree, HANDLE, HWND};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU,
        VK_SHIFT, VK_V,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SW_RESTORE, SetForegroundWindow,
        ShowWindow,
    };
    use windows::core::PWSTR;

    use super::{
        AccessibilityChecker, ContentKind, FRONT_POLL_INTERVAL, FRONT_WAIT_MAX, InjectError,
        KEY_WINDOW_SETTLE, PasteBackend, should_replace_target, wait_for_frontmost,
    };

    /// 写侧打开剪贴板的重试预算。比采集侧宽裕：这边的调用由用户操作触发，
    /// 宁可多等几十毫秒，也不要把剪贴板写失败（那会让用户"回车后什么都没发生"）。
    const WRITE_OPEN_ATTEMPTS: u32 = 10;
    const WRITE_OPEN_RETRY_DELAY: Duration = Duration::from_millis(10);
    /// 等物理修饰键全部抬起的上限。
    ///
    /// 热键是 `Ctrl+Alt+V`，"按下热键后用方向键选条目、Alt 一直没松"是真实操作
    /// （AEW5）。此时若直接注入，系统会把合成事件解读成 `Ctrl+Alt+V` 而不是 `Ctrl+V`。
    const MODIFIER_RELEASE_MAX: Duration = Duration::from_millis(400);
    /// 修饰键轮询间隔。
    const MODIFIER_POLL_INTERVAL: Duration = Duration::from_millis(10);
    /// 注入后留给目标应用处理粘贴的时间，之后才比对前台窗口（与 macOS 侧同参）。
    const INJECT_SETTLE: Duration = Duration::from_millis(180);

    /// 回贴目标窗口。
    ///
    /// `hwnd` 存 `isize` 而不是 `HWND`：`HWND` 内部是裸指针、不是 `Send`，而这份状态要
    /// 跨线程（记录发生在托盘/热键线程，读取发生在 `spawn_blocking` 线程）。整数化是
    /// 这个场景的常规做法。
    #[derive(Clone)]
    struct Target {
        hwnd: isize,
        pid: u32,
        /// 进程可执行文件全路径（小写）。bundle id 的等价物，用于回贴后的失焦比对。
        exe: Option<String>,
    }

    static TARGET: Mutex<Option<Target>> = Mutex::new(None);

    /// 真实授权检查器。
    ///
    /// Windows 没有"辅助功能授权"这一前置门槛：`SendInput` 对同权限级别的进程直接生效。
    /// 恒 `true` 让 `perform_paste` 的第 3 步永远放行，`NoAccessibilityClipboardOnly`
    /// 分支在 Windows 上不可达（RW8）——前端也因此不会渲染授权横幅。
    ///
    /// 需要管理员权限的目标进程属于另一回事：那会被 UIPI 拦下（见 `simulate_paste`），
    /// 走的是 `ClipboardOnly` 降级而非授权路径。
    pub struct RealAccessibilityChecker;

    impl AccessibilityChecker for RealAccessibilityChecker {
        fn is_trusted(&self) -> bool {
            true
        }
    }

    /// 读某个窗口所属进程的可执行文件全路径（小写）。
    ///
    /// 取不到是**常态而不是异常**：目标进程可能已退出，或以更高完整性级别运行
    /// （任务管理器这类受保护进程不允许普通进程打开）。返回 `None` 时失焦比对退化为
    /// 「未切换」，与 macOS 侧 `bundleIdentifier()` 返回 nil 时的处理一致。
    fn exe_of_window(hwnd: HWND) -> Option<String> {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return None;
        }
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let result = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(process);
            result.ok()?;
            buf.truncate(len as usize);
            Some(String::from_utf16_lossy(&buf).to_lowercase())
        }
    }

    /// 捕获**当前**前台窗口所属进程的可执行文件路径（回贴后用它做失焦比对）。
    pub fn capture_frontmost() -> Option<String> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return None;
        }
        exe_of_window(hwnd)
    }

    /// 记录回贴目标窗口。**必须在面板 `show()` 之前调用**（与 macOS 侧同一约束：
    /// 面板一旦聚焦，前台窗口就变成 ClipVault 自己，目标再也读不回来）。
    ///
    /// 覆盖策略复用平台无关的 [`should_replace_target`]：前台已经是**我们自己**时保留
    /// 原记录，避免"面板已在前台时又按一次热键"把目标记成 ClipVault。
    pub fn capture_target_app() {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0.is_null() {
            return;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return;
        }
        if !should_replace_target(Some(pid as i32), std::process::id() as i32) {
            return;
        }
        if let Ok(mut slot) = TARGET.lock() {
            // 即使 `exe` 取不到也要记下窗口句柄：还焦点只需要 HWND，
            // 失去的只是失焦比对能力（会退化成"未切换"）。
            *slot = Some(Target {
                hwnd: hwnd.0 as isize,
                pid,
                exe: exe_of_window(hwnd),
            });
        }
    }

    /// 目标进程的可执行文件路径（[`perform_paste`] 的失焦比对用）。
    pub fn target_bundle_id() -> Option<String> {
        target().and_then(|t| t.exe)
    }

    fn target() -> Option<Target> {
        TARGET.lock().ok().and_then(|slot| slot.clone())
    }

    /// 隐藏面板并把前台权交还目标窗口（F2 的核心，回贴与 Esc 关闭共用）。
    ///
    /// **步骤顺序与 macOS 侧相反，这是有意的。** macOS 先 `orderOut` 再激活；Windows 上
    /// 必须先激活再隐藏 —— `SetForegroundWindow` 受**前台锁**约束：只有**当前拥有前台权
    /// 的那个进程**才有资格把前台让给别人。此刻面板还聚焦着，ClipVault 正是那个进程，
    /// 所以请求会被放行；一旦先隐藏，前台权就落到别处，再调 `SetForegroundWindow` 会被
    /// 系统**静默忽略**，随后注入的 Ctrl+V 便落进错误的应用 —— 这正是回贴最怕的那种
    /// 静默失败（内容看似"粘了"，其实粘到了别处）。
    ///
    /// 之后有界等待目标窗口真的成为前台，再隐藏面板并留一段稳定期，让目标应用的输入
    /// 焦点恢复，随后注入的按键才落得进去。
    ///
    /// 与 macOS 的另一点差异：这里**主线程上也做完整等待**。macOS 必须区分主线程/工作
    /// 线程，是因为"前台应用是否已切换"要靠 run loop 消费窗口服务器消息才观测得到；
    /// Windows 的 `GetForegroundWindow` 是同步查询，不依赖谁在跑消息循环，等待只是
    /// 有界 sleep，不会冻住 UI 更新之外的任何东西。
    pub fn hide_panel_and_restore_focus(window: &tauri::WebviewWindow) -> Result<(), String> {
        let Some(target) = target() else {
            // 没有记录目标（例如面板是托盘点开的）：只能隐藏，前台权交给系统自己决定。
            return window.hide().map_err(|e| e.to_string());
        };

        let hwnd = HWND(target.hwnd as *mut core::ffi::c_void);
        unsafe {
            // 最小化的窗口拿不到输入焦点，先还原（对应 macOS 的 activateWithOptions）。
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }

        // 用 HWND 而不是 pid 判等：同一个进程可能开多个窗口，而我们要的是那一个。
        let restored = wait_for_frontmost(
            || {
                let front = unsafe { GetForegroundWindow() };
                (front == hwnd).then_some(target.pid as i32)
            },
            target.pid as i32,
            FRONT_WAIT_MAX,
            FRONT_POLL_INTERVAL,
        );
        if !restored {
            // 不在这里失败：按键照常注入，真落空了由下游的失焦比对返回 WindowSwitched，
            // 前端提示用户手动 Ctrl+V。留一行日志便于排障。
            ::log::warn!(target: "pasteback", "目标窗口未在 {FRONT_WAIT_MAX:?} 内回到前台，Ctrl+V 可能落空");
        }

        window.hide().map_err(|e| e.to_string())?;
        // 激活完成 ≠ 输入焦点已恢复，留稳定期再注入按键。
        std::thread::sleep(KEY_WINDOW_SETTLE);
        Ok(())
    }

    /// 真实回贴后端：Win32 剪贴板写入 + Tauri 面板隐藏 + `SendInput` 注入 + 前台窗口比对。
    pub struct WindowsBackend {
        window: tauri::WebviewWindow,
    }

    impl WindowsBackend {
        pub fn new(window: tauri::WebviewWindow) -> Self {
            Self { window }
        }
    }

    impl PasteBackend for WindowsBackend {
        /// 写剪贴板：一次 `OpenClipboard` 临界区内清空并按类型挂格式（KTD15）。
        ///
        /// 与 macOS 侧的映射差异：
        /// - **图片同时挂注册格式 `PNG` 与 `CF_DIB`**。前者是现代应用（浏览器、Office、
        ///   邮件客户端）的首选且能保留真实 alpha；后者是画图这类传统应用唯一认识的格式。
        ///   两个都挂才有 AEW7 里"画图与 Mail 都能粘"的效果。
        /// - **文件走 `CF_HDROP` 原生文件引用**，不做 macOS 那种"路径拼成换行文本"的退化
        ///   —— 那是 objc2 暴露面不足的权宜之计，Windows 没有这个限制。同时附带一份换行
        ///   文本，使只接受文本的目标（普通输入框）也能拿到路径，这也是资源管理器自己的做法。
        ///
        /// 注：`Color` 也走 `content` —— `perform_paste` 现在会从 `metadata_json.rgba`
        /// 取出色值填进 `content`（见 `color_paste_text`），因此两个平台的 color 条目
        /// 都会粘贴出色值文本（`rgba(r,g,b,a)`）。这里不需要任何 Windows 专属分支。
        fn write_clipboard(
            &self,
            kind: ContentKind,
            content: &str,
            payload: Option<&[u8]>,
            mime: Option<&str>,
        ) {
            if !crate::capture::open_clipboard_with_retry(
                WRITE_OPEN_ATTEMPTS,
                WRITE_OPEN_RETRY_DELAY,
            ) {
                // 剪贴板被别的进程长时间占着，连清空都做不到。放弃并留日志：
                // 用户侧表现为"回车后什么都没发生"，比贴错内容安全。
                ::log::error!(target: "pasteback", "打开剪贴板失败（被其它进程占用），本次未写入内容");
                return;
            }

            // 清空必须在挂任何格式之前；之后可以连续挂多个格式，直到 CloseClipboard。
            let _ = unsafe { EmptyClipboard() };

            match kind {
                ContentKind::Text | ContentKind::Color => {
                    set_format(
                        CF_UNICODETEXT.0 as u32,
                        &utf16_le_bytes(&crate::winclip::utf16_nul(content)),
                    );
                }
                ContentKind::Image => {
                    let bytes = payload.unwrap_or(&[]);
                    if !bytes.is_empty() {
                        // 只有当字节确实是 PNG 时才挂注册格式 PNG —— 否则等于给
                        // 接收方一份"自称 PNG 的 TIFF"，比不挂更糟。
                        let is_png = mime
                            .unwrap_or("image/png")
                            .eq_ignore_ascii_case("image/png");
                        if is_png && let Some(format) = crate::capture::png_format() {
                            set_format(format, bytes);
                        }
                        // 传统格式。`image_to_dib` 能解码任何 `image` crate 认得的格式，
                        // 所以 macOS 侧存下来的 TIFF payload 在这里也能转。
                        if let Some(dib) = crate::winclip::image_to_dib(bytes) {
                            set_format(CF_DIB.0 as u32, &dib);
                        }
                    }
                }
                ContentKind::File => {
                    let paths: Vec<String> =
                        serde_json::from_slice(payload.unwrap_or(&[])).unwrap_or_default();
                    if !paths.is_empty() {
                        set_format(CF_HDROP.0 as u32, &crate::winclip::build_dropfiles(&paths));
                        set_format(
                            CF_UNICODETEXT.0 as u32,
                            &utf16_le_bytes(&crate::winclip::utf16_nul(&paths.join("\r\n"))),
                        );
                    }
                }
                ContentKind::Rtf => {
                    let bytes = payload.unwrap_or(&[]);
                    if !bytes.is_empty()
                        && let Some(format) = crate::capture::rtf_format()
                    {
                        set_format(format, bytes);
                    }
                }
            }

            // 失败没有可挽回的后续动作，但必须关闭 —— 不关闭会让全系统无法使用剪贴板。
            let _ = unsafe { CloseClipboard() };
        }

        fn refocus_target(&self) {
            // 失败不外传："Ctrl+V 没落到目标应用"由下游的失焦比对（WindowSwitched）兜底。
            let _ = hide_panel_and_restore_focus(&self.window);
        }

        /// 注入 `Ctrl+V`（KTD12）。
        fn simulate_paste(&self) -> Result<(), InjectError> {
            // 1. 等物理修饰键抬起。用户很可能按着 Alt 就用方向键选完了条目，
            //    此时注入的 Ctrl+V 会被系统解读成 Ctrl+Alt+V。
            if !wait_for_modifiers_released(MODIFIER_RELEASE_MAX) {
                ::log::warn!(target: "pasteback", "修饰键在 {MODIFIER_RELEASE_MAX:?} 内未抬起，放弃自动回贴");
                return Err(InjectError);
            }

            // 2. 注入完整序列。macOS 那套"给事件挂 Command flag"在 SendInput 里没有
            //    对应物 —— 修饰键必须自己按下、自己抬起。
            let events = [
                key_event(VK_CONTROL, false),
                key_event(VK_V, false),
                key_event(VK_V, true),
                key_event(VK_CONTROL, true),
            ];
            let sent = unsafe { SendInput(&events, size_of::<INPUT>() as i32) };
            if sent as usize != events.len() {
                // 典型成因是 UIPI：目标进程以更高完整性级别（管理员）运行时，
                // 系统会静默丢弃注入。这是 Windows 的安全设计，不绕过 ——
                // 交给调用方降级为"仅剪贴板"。
                ::log::error!(target: "pasteback", "SendInput 被拒绝（目标应用权限更高？），已降级为仅写剪贴板");
                return Err(InjectError);
            }

            // 3. 给目标应用处理粘贴的时间，之后调用方才比对前台窗口。
            std::thread::sleep(INJECT_SETTLE);
            Ok(())
        }

        fn frontmost_app(&self) -> Option<String> {
            capture_frontmost()
        }
    }

    /// 把一个格式的数据挂到**已打开**的剪贴板上。
    ///
    /// `HGLOBAL` 的所有权在 `SetClipboardData` 成功后归系统，**不能再释放**；失败时
    /// 必须自己释放，否则泄漏一块全局内存。
    fn set_format(format: u32, bytes: &[u8]) -> bool {
        unsafe {
            // 大小为 0 的全局内存分配没有意义（且行为未定义），兜一个最小值。
            let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else {
                return false;
            };
            let ptr = GlobalLock(block);
            if ptr.is_null() {
                let _ = GlobalFree(Some(block));
                return false;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
            // 返回值忽略：对 GMEM_MOVEABLE 内存，最后一次解锁返回 FALSE 并把 last error
            // 置为 NO_ERROR（这是成功），windows-rs 把它映射成 Err。见 capture 侧同样说明。
            let _ = GlobalUnlock(block);
            match SetClipboardData(format, Some(HANDLE(block.0))) {
                Ok(_) => true,
                Err(_) => {
                    let _ = GlobalFree(Some(block));
                    false
                }
            }
        }
    }

    /// 构造一个键盘 `INPUT` 事件。
    fn key_event(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    // 一并填扫描码：少数应用（终端、部分游戏）只看扫描码。
                    // 未设 KEYEVENTF_SCANCODE 时系统以 wVk 为准，填上无害。
                    wScan: unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16,
                    dwFlags: if key_up {
                        KEYEVENTF_KEYUP
                    } else {
                        Default::default()
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    /// 有界等待所有物理修饰键抬起。
    fn wait_for_modifiers_released(budget: Duration) -> bool {
        /// Alt / Ctrl / Shift / Win 四个修饰键。
        const MODIFIERS: [VIRTUAL_KEY; 4] = [VK_MENU, VK_CONTROL, VK_SHIFT, VK_LWIN];
        let deadline = Instant::now() + budget;
        loop {
            let any_down = MODIFIERS.iter().any(|vk| {
                // 高位为 1 表示该键当前被物理按住。
                (unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000) != 0
            });
            if !any_down {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(MODIFIER_POLL_INTERVAL);
        }
    }

    /// `Vec<u16>` → 小端字节流（剪贴板的文本与 `DROPFILES` 都要字节而不是 `u16`）。
    fn utf16_le_bytes(units: &[u16]) -> Vec<u8> {
        units.iter().flat_map(|u| u.to_le_bytes()).collect()
    }
}

#[cfg(target_os = "windows")]
pub use imp_win::{
    RealAccessibilityChecker, WindowsBackend, capture_frontmost, capture_target_app,
    hide_panel_and_restore_focus, target_bundle_id,
};

/// 当前平台的真实回贴后端。
///
/// `commands::paste_entry` 的编排逻辑两端完全一致，只有后端类型名不同；用这个别名把
/// 那 80 多行编排收敛成**一份**实现，避免两端各写一遍、日后各自漂移。
#[cfg(target_os = "macos")]
pub use imp::MacosBackend as RealPasteBackend;
#[cfg(target_os = "windows")]
pub use imp_win::WindowsBackend as RealPasteBackend;

// ================================================================
// 其余平台降级（保证跨平台可编译）
// ================================================================

/// 其余平台：没有「前台应用 / 激活权」概念，不记录目标。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn capture_target_app() {}

/// 其余平台：无前台应用可比对。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn target_bundle_id() -> Option<String> {
    None
}

/// 其余平台：只隐藏面板（本来也没有自动回贴）。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn hide_panel_and_restore_focus(window: &tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}
