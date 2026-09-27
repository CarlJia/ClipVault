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
//! - **非 macOS**：无真实实现;`commands::paste_entry` 在非 macOS 目标上直接
//!   返回 [`PasteError::UnsupportedPlatform`]。
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

use clipvault_core::{ContentKind, Entry};

/// 目标窗口在回贴后失焦时的固定提示(计划 U8 step 3)。
pub const WINDOW_SWITCHED_MSG: &str = "目标窗口已切换,内容已写入剪贴板,请手动 ⌘V 粘贴";
/// CGEventPost 失败回退的固定提示(计划 U8 step 4,文案固定)。
pub const CGEVENT_FAIL_MSG: &str = "回贴失败,内容已复制到剪贴板,请手动 ⌘V";
/// 未授权 Accessibility 时的提示:内容已进剪贴板,引导用户手动 ⌘V 或开启授权。
pub const NO_ACCESSIBILITY_MSG: &str =
    "已复制到剪贴板,请手动 ⌘V 粘贴。开启「辅助功能」授权后可自动回贴。";
/// 加密区条目解密失败的提示（U9：Keychain 不可用或密钥不匹配 → 不回贴）。
pub const VAULT_DECRYPT_FAIL_MSG: &str = "加密条目解密失败（Keychain 不可用或密钥不匹配）";

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
    /// 在模拟按键前把键盘焦点还给目标应用(隐藏 ClipVault 面板,F2)。
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

/// 回贴编排(纯决策逻辑,平台副作用全走 trait)。
///
/// 顺序对应计划 U8 Approach，并修正了「未授权时内容也进不了剪贴板」的缺陷：
/// 1. 解析内容(加密区接缝:vault 条目走 `decryptor` 解密,失败 →
///    [`PasteError::VaultDecryptFailed`],**在写剪贴板前**失败,保证密文不落盘剪贴板)。
/// 2. `write_clipboard`——写剪贴板**不需要** Accessibility 权限,因此**先做**,
///    这样即便未授权,用户至少能手动 ⌘V 拿到内容。
/// 3. `is_trusted()` 校验;未授权 → [`PasteOutcome::NoAccessibilityClipboardOnly`]
///    (内容已在剪贴板,只是无法自动回贴),不再整体失败。
/// 4. `refocus_target`(隐藏面板,把焦点还给目标应用)。
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
    //    - 非 text kind: entry.content 留空;vault 条目需要 `decrypt_payload` 解密
    //      payload BLOB;非 vault 条目 payload 是明文,直接用。
    let kind = ContentKind::parse(&entry.content_kind);
    let text = if matches!(kind, ContentKind::Text) {
        decrypt_content(entry, nonce, decryptor)?
    } else {
        String::new()
    };
    let payload_bytes: Option<Vec<u8>> = if matches!(kind, ContentKind::Text) {
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

// ================================================================
// macOS 真实实现
// ================================================================
#[cfg(target_os = "macos")]
mod imp {
    use super::{AccessibilityChecker, ContentKind, InjectError, PasteBackend};

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

    /// 捕获当前前台应用的 bundle identifier(在热键触发、面板显示前调用)。
    pub fn capture_frontmost() -> Option<String> {
        use objc2_app_kit::NSWorkspace;
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        app.bundleIdentifier().map(|s| s.to_string())
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
                    // payload 是 JSON-encoded Vec<String>。真实生产应转 NSArray<NSString>
                    // 写 .filenames (NSFilenamesPboardType) 以让 Finder / Slack 文件
                    // 上传框等接受;objc2 0.3 当前未暴露 writeObjects 所需的
                    // ProtocolObject<NSArray<NSString>> 路径，且 setPropertyList_forType
                    // 需要 AnyObject 桥接（遗留 nspasteboard 类型常量为 NSFilenamesPboardType
                    // 字符串字面量）。MVP U4 阶段采用 setString_forType 退化路径：
                    // 把 paths 用 `\n` 拼接写为文本，目标应用收到纯文本路径列表
                    // （AE10 已声明 "若目标应用不支持文件引用，toast 提示降级"）。
                    // 后续增强：补 setPropertyList_forType 把 paths 转 NSArray 写 .filenames。
                    let paths: Vec<String> =
                        serde_json::from_slice(payload.unwrap_or(&[])).unwrap_or_default();
                    let joined = paths.join("\n");
                    let ns = objc2_foundation::NSString::from_str(&joined);
                    let _ = unsafe { pasteboard.setString_forType(&ns, NSPasteboardTypeString) };
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
            // 隐藏 ClipVault 面板:macOS 会把键盘焦点还给此前活跃的应用,
            // 随后注入的 ⌘V 便落到目标应用而非我们的搜索框(F2)。
            let _ = self.window.hide();
            // 给窗口服务器一点时间完成焦点切换,再注入按键。
            std::thread::sleep(std::time::Duration::from_millis(60));
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
pub use imp::{MacosBackend, RealAccessibilityChecker, capture_frontmost};
