//! U8 回贴决策逻辑单测(跨平台,不触碰真实 CGEvent / Accessibility)。
//!
//! 真实按键注入与真实 Accessibility 授权归 AE3 / AE7 的 macOS 真机手动验收;
//! 这里只用 mock [`AccessibilityChecker`] / [`PasteBackend`] 断言
//! [`perform_paste`] 的分支决策:是否写剪贴板、是否触达模拟、失败回退文案。

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use clipvault_core::ContentKind;
use clipvault_core::Entry;
use clipvault_core::crypto::CryptoError;

use crate::crypto::Decryptor;
use crate::pasteback::{
    AccessibilityChecker, InjectError, PasteBackend, PasteError, PasteOutcome, cgevent_fail_msg,
    color_paste_text, copy_text_to_clipboard, file_paths, file_text_fallback, modifier_label,
    no_accessibility_msg, perform_paste, should_replace_target, vault_decrypt_fail_msg,
    wait_for_frontmost, window_switched_msg,
};

/// 固定返回布尔的 Accessibility mock。
struct MockChecker {
    trusted: bool,
}

impl AccessibilityChecker for MockChecker {
    fn is_trusted(&self) -> bool {
        self.trusted
    }
}

/// 解密 mock：测试驱动 `decrypt` 与 `decrypt_bytes` 的成功 / 失败路径。
struct MockDecryptor {
    text: Result<String, CryptoError>,
    bytes: Result<Vec<u8>, CryptoError>,
}

impl Decryptor for MockDecryptor {
    fn decrypt(&self, _stored_content: &str, _nonce: &[u8]) -> Result<String, CryptoError> {
        self.text.clone()
    }
    fn decrypt_bytes(&self, _stored: &[u8], _nonce: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.bytes.clone()
    }
    fn data_key(&self) -> Result<[u8; clipvault_core::crypto::KEY_LEN], CryptoError> {
        Ok([0u8; clipvault_core::crypto::KEY_LEN])
    }
}

/// 非 vault 分支使用的哨兵解密器：若被调用即 panic（断言 plain 路径不解密）。
struct NeverDecryptor;

impl Decryptor for NeverDecryptor {
    fn decrypt(&self, _stored_content: &str, _nonce: &[u8]) -> Result<String, CryptoError> {
        panic!("非加密条目不应调用解密器");
    }
    fn decrypt_bytes(&self, _stored: &[u8], _nonce: &[u8]) -> Result<Vec<u8>, CryptoError> {
        panic!("非加密二进制条目不应调用解密器");
    }
    fn data_key(&self) -> Result<[u8; clipvault_core::crypto::KEY_LEN], CryptoError> {
        panic!("NeverDecryptor 不应被查询 key");
    }
}

/// 记录调用轨迹的回贴后端 mock。
#[derive(Default)]
struct MockBackend {
    /// 是否让 `simulate_paste` 失败(驱动 CGEvent 失败回退)。
    fail_inject: bool,
    /// `frontmost_app` 返回的值(用于失焦比对)。
    frontmost: Option<String>,
    /// 调用轨迹。
    wrote_kind: RefCell<Option<ContentKind>>,
    wrote_content: RefCell<Option<String>>,
    wrote_payload_len: RefCell<Option<usize>>,
    refocused: RefCell<bool>,
    simulated: RefCell<bool>,
}

impl PasteBackend for MockBackend {
    fn write_clipboard(
        &self,
        kind: ContentKind,
        content: &str,
        payload: Option<&[u8]>,
        _mime: Option<&str>,
    ) {
        *self.wrote_kind.borrow_mut() = Some(kind);
        *self.wrote_content.borrow_mut() = Some(content.to_string());
        *self.wrote_payload_len.borrow_mut() = payload.map(|p| p.len());
    }
    fn refocus_target(&self) {
        *self.refocused.borrow_mut() = true;
    }
    fn simulate_paste(&self) -> Result<(), InjectError> {
        *self.simulated.borrow_mut() = true;
        if self.fail_inject {
            Err(InjectError)
        } else {
            Ok(())
        }
    }
    fn frontmost_app(&self) -> Option<String> {
        self.frontmost.clone()
    }
}

/// 构造一条普通(非加密)文本条目。
fn plain_entry(content: &str) -> Entry {
    Entry::from_row(
        1,
        "hash".to_string(),
        content.to_string(),
        String::new(),
        "plain".to_string(),
        0,
        None,
        0,
        1,
        0,
        "text".to_string(),
        None,
        None,
        None,
        None,
    )
}

/// 构造一条加密区文本条目(is_vault = 1, content_kind = text)。
fn vault_entry() -> Entry {
    Entry::from_row(
        2,
        "vhash".to_string(),
        "ciphertext-bytes".to_string(),
        String::new(),
        "plain".to_string(),
        0,
        None,
        1, // is_vault
        1,
        0,
        "text".to_string(),
        None,
        None,
        None,
        None,
    )
}

/// 构造一条加密区图片条目（U4 multi-type plan）。
fn vault_image_entry() -> Entry {
    Entry::from_row(
        3,
        "vimghash".to_string(),
        String::new(), // content 留空
        String::new(),
        "image".to_string(),
        0,
        None,
        1, // is_vault
        1,
        0,
        "image".to_string(),
        Some("image/png".to_string()),
        Some(b"\x89PNG encrypted bytes".to_vec()),
        Some(vec![0u8; 12]), // payload_nonce
        Some(r#"{"width":1,"height":1}"#.to_string()),
    )
}

/// 构造一条非加密图片条目。
fn image_entry() -> Entry {
    Entry::from_row(
        4,
        "imghash".to_string(),
        String::new(),
        String::new(),
        "image".to_string(),
        0,
        None,
        0,
        1,
        0,
        "image".to_string(),
        Some("image/png".to_string()),
        Some(b"\x89PNG raw bytes".to_vec()),
        None,
        Some(r#"{"width":1,"height":1}"#.to_string()),
    )
}

/// 构造一条非加密颜色条目：色值只存在 `metadata_json.rgba`，`payload` 为空
/// （macOS 的颜色捕获本来就只产出一段 rgba 文本）。
fn color_entry(rgba: &str) -> Entry {
    Entry::from_row(
        5,
        "colorhash".to_string(),
        String::new(),
        String::new(),
        "color".to_string(),
        0,
        None,
        0,
        1,
        0,
        "color".to_string(),
        None,
        None,
        None,
        Some(format!(r#"{{"rgba":"{rgba}"}}"#)),
    )
}

/// 构造一条加密区颜色条目（`is_vault = 1`，`payload` / `payload_nonce` 均为空 ——
/// 颜色本来就没有 payload 可加密）。
fn vault_color_entry() -> Entry {
    Entry::from_row(
        6,
        "vcolorhash".to_string(),
        String::new(),
        String::new(),
        "color".to_string(),
        0,
        None,
        1, // is_vault
        1,
        0,
        "color".to_string(),
        None,
        None,
        None,
        Some(r#"{"rgba":"rgba(51,153,230,1)"}"#.to_string()),
    )
}

#[test]
fn clipboard_written_even_when_not_trusted() {
    let checker = MockChecker { trusted: false };
    let backend = MockBackend::default();

    let result = perform_paste(
        &checker,
        &backend,
        &plain_entry("hello"),
        None,
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::NoAccessibilityClipboardOnly));
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some("hello"),
        "未授权时内容仍应写入剪贴板(写剪贴板不需要 Accessibility)"
    );
    assert_eq!(*backend.wrote_kind.borrow(), Some(ContentKind::Text));
    assert!(!*backend.simulated.borrow(), "未授权时不应模拟 ⌘V 按键注入");
}

#[test]
fn copy_text_to_clipboard_writes_path_as_plain_text() {
    let backend = MockBackend::default();
    let path = "/Users/me/Library/Application Support/clipvault/diagnostics/clipvault.log";

    copy_text_to_clipboard(&backend, path);

    // 必须是**文本**类型：写成 File/Rtf 会让接收方拿到别的东西。
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some(path),
        "复制的是原样路径文本"
    );
    assert_eq!(*backend.wrote_kind.borrow(), Some(ContentKind::Text));
    assert_eq!(*backend.wrote_payload_len.borrow(), None);
}

#[test]
fn vault_entry_not_written_when_not_trusted() {
    let checker = MockChecker { trusted: false };
    let backend = MockBackend::default();
    let decryptor = MockDecryptor {
        text: Ok("vault-明文".to_string()),
        bytes: Ok(Vec::new()),
    };
    let nonce = [0u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_entry(),
        None,
        Some(&nonce),
        None,
        &decryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::NoAccessibilityClipboardOnly));
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some("vault-明文"),
        "vault 条目解密成功后,未授权也应把明文写入剪贴板供手动 ⌘V"
    );
    assert!(!*backend.simulated.borrow(), "未授权时不应模拟按键");
}

#[test]
fn paste_proceeds_when_trusted() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.apple.TextEdit".to_string()),
        ..Default::default()
    };

    let result = perform_paste(
        &checker,
        &backend,
        &plain_entry("payload"),
        Some("com.apple.TextEdit"),
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some("payload"),
        "授权后应把内容写入剪贴板"
    );
    assert!(*backend.refocused.borrow(), "应隐藏面板把焦点还给目标应用");
    assert!(*backend.simulated.borrow(), "应触达 ⌘V 模拟分支");
}

#[test]
fn window_switched_when_frontmost_changes() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.other.App".to_string()),
        ..Default::default()
    };

    let result = perform_paste(
        &checker,
        &backend,
        &plain_entry("x"),
        Some("com.apple.TextEdit"),
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::WindowSwitched));
}

#[test]
fn vault_entry_decrypts_when_key_present() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.apple.TextEdit".to_string()),
        ..Default::default()
    };
    let decryptor = MockDecryptor {
        text: Ok("解密后的明文".to_string()),
        bytes: Ok(Vec::new()),
    };
    let nonce = [0u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_entry(),
        Some("com.apple.TextEdit"),
        Some(&nonce),
        None,
        &decryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some("解密后的明文"),
        "vault 条目回贴应写入解密后的明文"
    );
    assert!(*backend.simulated.borrow(), "解密成功后应触达 ⌘V 模拟");
}

#[test]
fn vault_entry_fails_when_keychain_unavailable() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend::default();
    let decryptor = MockDecryptor {
        text: Err(CryptoError::KeyUnavailable("Keychain 不可用".to_string())),
        bytes: Err(CryptoError::KeyUnavailable("Keychain 不可用".to_string())),
    };
    let nonce = [0u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_entry(),
        None,
        Some(&nonce),
        None,
        &decryptor,
    );

    assert_eq!(result, Err(PasteError::VaultDecryptFailed));
    assert!(
        backend.wrote_content.borrow().is_none(),
        "解密失败时不应把密文写入剪贴板"
    );
    assert!(!*backend.simulated.borrow(), "解密失败时不应触达模拟");
}

#[test]
fn vault_entry_fails_when_nonce_missing() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend::default();
    let decryptor = MockDecryptor {
        text: Ok("不该被用到".to_string()),
        bytes: Ok(Vec::new()),
    };

    let result = perform_paste(
        &checker,
        &backend,
        &vault_entry(),
        None,
        None,
        None,
        &decryptor,
    );

    assert_eq!(result, Err(PasteError::VaultDecryptFailed));
    assert!(
        backend.wrote_content.borrow().is_none(),
        "缺 nonce 时不应写入剪贴板"
    );
}

#[test]
fn cgevent_failure_falls_back_to_clipboard_only() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        fail_inject: true,
        ..Default::default()
    };

    let result = perform_paste(
        &checker,
        &backend,
        &plain_entry("data"),
        None,
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::ClipboardOnly));
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some("data"),
        "回退路径下内容仍应已在剪贴板"
    );
}

// ----------------------------------------------------------------
// 平台相关文案（U5 / RW8 / RW9）
// ----------------------------------------------------------------
//
// 四个文案函数都吃一个显式 `is_macos`，所以**两个平台的分支都能在 macOS 上被断言**
// —— 这正是把它们写成纯函数而不是 `cfg!` 分支常量的原因（同 `storage::resolve_data_dir`）。

#[test]
fn paste_messages_follow_platform_modifier() {
    assert_eq!(modifier_label(true), "⌘");
    assert_eq!(modifier_label(false), "Ctrl+");

    // macOS：与移植前逐字一致 —— 平台化不许把 macOS 文案改坏。
    assert_eq!(
        window_switched_msg(true),
        "目标窗口已切换,内容已写入剪贴板,请手动 ⌘V 粘贴"
    );
    assert_eq!(
        cgevent_fail_msg(true),
        "回贴失败,内容已复制到剪贴板,请手动 ⌘V"
    );
    assert_eq!(
        no_accessibility_msg(true),
        "已复制到剪贴板,请手动 ⌘V 粘贴。开启「辅助功能」授权后可自动回贴。"
    );
    assert_eq!(
        vault_decrypt_fail_msg(true),
        "加密条目解密失败（Keychain 不可用或密钥不匹配）"
    );

    // Windows：修饰键换成 Ctrl+，且不得再出现 ⌘。
    for msg in [
        window_switched_msg(false),
        cgevent_fail_msg(false),
        no_accessibility_msg(false),
    ] {
        assert!(msg.contains("Ctrl+V"), "Windows 文案应提示 Ctrl+V：{msg}");
        assert!(!msg.contains('⌘'), "Windows 文案不该出现 ⌘：{msg}");
    }

    // Windows：密钥托管是「凭据管理器」，不是 Keychain。
    assert_eq!(
        vault_decrypt_fail_msg(false),
        "加密条目解密失败（凭据管理器不可用或密钥不匹配）"
    );
}

#[test]
fn image_entry_dispatches_payload_to_setdata() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.apple.Mail".to_string()),
        ..Default::default()
    };

    let result = perform_paste(
        &checker,
        &backend,
        &image_entry(),
        Some("com.apple.Mail"),
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    assert_eq!(*backend.wrote_kind.borrow(), Some(ContentKind::Image));
    assert_eq!(
        *backend.wrote_payload_len.borrow(),
        Some(b"\x89PNG raw bytes".len())
    );
}

#[test]
fn vault_image_entry_decrypts_payload_before_write() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.apple.Mail".to_string()),
        ..Default::default()
    };
    let raw_png = b"\x89PNG decrypted bytes";
    let decryptor = MockDecryptor {
        text: Err(CryptoError::Decrypt),
        bytes: Ok(raw_png.to_vec()),
    };
    let nonce = [0u8; 12];
    let payload_nonce = [1u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_image_entry(),
        Some("com.apple.Mail"),
        Some(&nonce),
        Some(&payload_nonce),
        &decryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    assert_eq!(*backend.wrote_kind.borrow(), Some(ContentKind::Image));
    assert_eq!(*backend.wrote_payload_len.borrow(), Some(raw_png.len()));
    assert!(*backend.simulated.borrow(), "图片 vault 解密后应模拟 ⌘V");
}

// ----------------------------------------------------------------
// 焦点归还的有界等待（F2）：wait_for_frontmost 的平台无关决策
// ----------------------------------------------------------------
//
// 真实激活（NSRunningApplication.activateWithOptions）与真实前台应用读取
// 归 AE3 的 macOS 真机手动验收；这里只断言等待策略本身：命中即返回、
// 异步到位能等到、始终不到位则在 max_wait 截止后放弃（调用方据此保留
// 降级提示），且读不到前台应用时不会卡死或 panic。

/// 第一次采样就命中 → 立即返回，且**只采样一次**（不做无谓 sleep）。
#[test]
fn wait_for_frontmost_returns_on_first_hit() {
    let samples = Cell::new(0);
    let ok = wait_for_frontmost(
        || {
            samples.set(samples.get() + 1);
            Some(42)
        },
        42,
        Duration::from_millis(200),
        Duration::from_millis(1),
    );

    assert!(ok);
    assert_eq!(samples.get(), 1, "已在前台时不应再轮询");
}

/// 目标稍后才回到前台（激活是异步的）→ 轮询到命中为止。
#[test]
fn wait_for_frontmost_polls_until_target_arrives() {
    let samples = Cell::new(0);
    let ok = wait_for_frontmost(
        || {
            samples.set(samples.get() + 1);
            // 前两次采样是别的 App（面板刚隐藏时的旧前台），第三次才轮到目标。
            if samples.get() >= 3 {
                Some(7)
            } else {
                Some(99)
            }
        },
        7,
        Duration::from_millis(200),
        Duration::from_millis(1),
    );

    assert!(ok);
    assert_eq!(samples.get(), 3, "命中后应停止轮询");
}

/// 目标始终没回到前台 → 到 max_wait 截止后返回 false（不再阻塞回贴）。
#[test]
fn wait_for_frontmost_gives_up_after_max_wait() {
    let started = Instant::now();
    let ok = wait_for_frontmost(
        || Some(99),
        7,
        Duration::from_millis(20),
        Duration::from_millis(2),
    );

    assert!(!ok, "目标未回到前台时应超时返回 false");
    assert!(
        started.elapsed() >= Duration::from_millis(20),
        "应真的等到 max_wait 截止"
    );
}

/// 读不到前台应用（None）时按超时收敛，不 panic、不提前成功。
#[test]
fn wait_for_frontmost_handles_unknown_frontmost() {
    let ok = wait_for_frontmost(
        || None,
        7,
        Duration::from_millis(15),
        Duration::from_millis(2),
    );

    assert!(!ok);
}

// ----------------------------------------------------------------
// 回贴目标的记录策略（F2）：should_replace_target 的边界
// ----------------------------------------------------------------
//
// 真实「前台应用是谁」由 macOS 提供（`NSWorkspace.frontmostApplication`），
// 归 AE3 真机验收；这里只锁住覆盖策略这条纯逻辑——它是「面板已在前台时再按
// 热键」这个常见操作的唯一防线。

/// 面板已在前台时再按一次 ⌥⌘V：前台是 ClipVault 自己（pid 相同）→ 不覆盖，
/// 否则回贴后的失焦比对会把「自己」当目标，把 Pasted 误报成 WindowSwitched。
#[test]
fn target_not_replaced_when_panel_is_frontmost() {
    assert!(!should_replace_target(Some(42), 42));
}

/// 正常情形：前台是别的 App → 覆盖。
#[test]
fn target_replaced_when_other_app_is_frontmost() {
    assert!(should_replace_target(Some(7), 42));
}

/// 读不到前台应用 → 保留原记录，不用 None 抹掉已有的真实目标。
#[test]
fn target_kept_when_frontmost_unknown() {
    assert!(!should_replace_target(None, 42));
}

// ----------------------------------------------------------------
// color 回贴：色值从 metadata_json 取出（U2 收口）
// ----------------------------------------------------------------
//
// 颜色条目没有 payload 可写，能贴出去的就是 `metadata_json.rgba` 这段文本。此前
// `perform_paste` 只对 `Text` 填 `content`，于是两个平台上回贴颜色都会写出空文本。

/// 颜色条目回贴时写的是 rgba 色值，而不是空串。
#[test]
fn color_entry_pastes_rgba_text() {
    let checker = MockChecker { trusted: false };
    let backend = MockBackend::default();

    let result = perform_paste(
        &checker,
        &backend,
        &color_entry("rgba(51,153,230,1)"),
        None,
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::NoAccessibilityClipboardOnly));
    assert_eq!(*backend.wrote_kind.borrow(), Some(ContentKind::Color));
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some("rgba(51,153,230,1)"),
        "颜色条目的回贴文本必须来自 metadata_json.rgba"
    );
}

/// 加密区里的颜色条目同样要贴出色值：颜色没有 payload，`perform_paste` 若照旧让非 text
/// 类型走 `decrypt_payload`，会因 `payload_nonce` 为 None 直接 `VaultDecryptFailed` ——
/// 表现是「加密区里的颜色条目按回车后什么都没发生」。
#[test]
fn vaulted_color_entry_pastes_rgba_without_payload_decrypt() {
    let checker = MockChecker { trusted: false };
    let backend = MockBackend::default();

    let result = perform_paste(
        &checker,
        &backend,
        &vault_color_entry(),
        None,
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::NoAccessibilityClipboardOnly));
    assert_eq!(
        backend.wrote_content.borrow().as_deref(),
        Some("rgba(51,153,230,1)"),
        "加密区颜色条目也必须贴出色值,而不是卡在解密 payload 那一步"
    );
}

/// `color_paste_text` 的边界：取不到就空串，绝不 panic，也不把非字符串当成色值。
#[test]
fn color_paste_text_handles_missing_or_odd_metadata() {
    assert_eq!(color_paste_text(None), "");
    assert_eq!(color_paste_text(Some("")), "");
    assert_eq!(color_paste_text(Some("not json")), "");
    assert_eq!(color_paste_text(Some("{}")), "");
    assert_eq!(color_paste_text(Some(r#"{"rgba":123}"#)), "");
    assert_eq!(color_paste_text(Some(r#"{"width":1}"#)), "");
    assert_eq!(
        color_paste_text(Some(r#"{"rgba":"rgba(0,0,0,1)"}"#)),
        "rgba(0,0,0,1)"
    );
}

// ----------------------------------------------------------------
// file 条目：payload 解析 + 剪贴板双写
// ----------------------------------------------------------------

/// 构造一条非加密 file 条目：payload 是 JSON 编码的绝对路径数组
/// （与 `Captured::file` 的编码同源）。
fn file_entry(paths: &[&str]) -> Entry {
    Entry::from_row(
        7,
        "filehash".to_string(),
        String::new(), // content 留空，路径在 payload
        String::new(),
        "file".to_string(),
        0,
        None,
        0,
        1,
        0,
        "file".to_string(),
        Some("text/uri-list".to_string()),
        Some(serde_json::to_vec(paths).unwrap()),
        None,
        Some(serde_json::json!({ "paths": paths, "resolved": true }).to_string()),
    )
}

/// 构造一条加密区 file 条目（payload 是密文，payload_nonce 保护它）。
fn vault_file_entry() -> Entry {
    Entry::from_row(
        8,
        "vfilehash".to_string(),
        String::new(),
        String::new(),
        "file".to_string(),
        0,
        None,
        1, // is_vault
        1,
        0,
        "file".to_string(),
        Some("text/uri-list".to_string()),
        Some(b"encrypted path json".to_vec()),
        Some(vec![0u8; 12]), // payload_nonce
        Some(r#"{"paths":[],"resolved":true}"#.to_string()),
    )
}

/// file 条目的 payload 解出路径数组；非 JSON / 空 payload 得空数组，不 panic。
#[test]
fn file_paths_decodes_json_payload() {
    let entry = file_entry(&["/a.pdf", "/b.csv"]);
    assert_eq!(
        file_paths(entry.payload.as_deref()),
        vec!["/a.pdf".to_string(), "/b.csv".to_string()]
    );
}

#[test]
fn file_paths_tolerates_missing_or_malformed_payload() {
    assert!(file_paths(None).is_empty());
    assert!(file_paths(Some(b"")).is_empty());
    assert!(file_paths(Some(b"not json")).is_empty());
    assert!(
        file_paths(Some(b"{\"paths\":[\"/a\"]}")).is_empty(),
        "只认顶层数组"
    );
    assert!(
        file_paths(Some(b"[\"/a\", 1]")).is_empty(),
        "元素必须是字符串"
    );
    assert!(file_paths(Some(b"[]")).is_empty());
}

/// 纯文本回退内容：路径按 `\n` 拼接（Windows 侧同义，用 `\r\n`）。
#[test]
fn file_text_fallback_joins_paths_with_newline() {
    let paths = vec!["/a.pdf".to_string(), "/b.csv".to_string()];
    assert_eq!(file_text_fallback(&paths), "/a.pdf\n/b.csv");
    assert_eq!(file_text_fallback(&[]), "");
}

/// `perform_paste` 把 file 条目的 payload 原样交给后端（后端再决定怎么写剪贴板）。
#[test]
fn pastebackend_file_reaches_write_clipboard_with_payload() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.apple.TextEdit".to_string()),
        ..Default::default()
    };
    let entry = file_entry(&["/a.pdf", "/b.csv"]);

    let result = perform_paste(
        &checker,
        &backend,
        &entry,
        Some("com.apple.TextEdit"),
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    assert_eq!(*backend.wrote_kind.borrow(), Some(ContentKind::File));
    assert_eq!(
        *backend.wrote_payload_len.borrow(),
        Some(entry.payload.as_ref().unwrap().len()),
        "file 条目应把 JSON payload 原样交给后端"
    );
}

/// 加密区 file 条目：先解密 payload，再把明文交给后端
/// （**不**把密文当路径贴出去）。
#[test]
fn pastebackend_file_vault_decrypts_payload_first() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.apple.TextEdit".to_string()),
        ..Default::default()
    };
    let plaintext = serde_json::to_vec(&["/secret/report.pdf"]).unwrap();
    let decryptor = MockDecryptor {
        text: Ok(String::new()),
        bytes: Ok(plaintext.clone()),
    };
    let nonce = [0u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_file_entry(),
        Some("com.apple.TextEdit"),
        None,
        Some(&nonce),
        &decryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    assert_eq!(*backend.wrote_kind.borrow(), Some(ContentKind::File));
    assert_eq!(
        *backend.wrote_payload_len.borrow(),
        Some(plaintext.len()),
        "交出去的应是解密后的明文路径 JSON，不是密文"
    );
}
