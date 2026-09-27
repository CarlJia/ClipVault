//! U8 回贴决策逻辑单测(跨平台,不触碰真实 CGEvent / Accessibility)。
//!
//! 真实按键注入与真实 Accessibility 授权归 AE3 / AE7 的 macOS 真机手动验收;
//! 这里只用 mock [`AccessibilityChecker`] / [`PasteBackend`] 断言
//! [`perform_paste`] 的分支决策:是否写剪贴板、是否触达模拟、失败回退文案。

use std::cell::RefCell;

use clipvault_core::ContentKind;
use clipvault_core::Entry;
use clipvault_core::crypto::CryptoError;

use crate::crypto::Decryptor;
use crate::pasteback::{
    AccessibilityChecker, CGEVENT_FAIL_MSG, InjectError, PasteBackend, PasteError, PasteOutcome,
    WINDOW_SWITCHED_MSG, perform_paste,
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
    assert_eq!(
        WINDOW_SWITCHED_MSG,
        "目标窗口已切换,内容已写入剪贴板,请手动 ⌘V 粘贴"
    );
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
    assert_eq!(CGEVENT_FAIL_MSG, "回贴失败,内容已复制到剪贴板,请手动 ⌘V");
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
