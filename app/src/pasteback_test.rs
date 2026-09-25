//! U8 回贴决策逻辑单测(跨平台,不触碰真实 CGEvent / Accessibility)。
//!
//! 真实按键注入与真实 Accessibility 授权归 AE3 / AE7 的 macOS 真机手动验收;
//! 这里只用 mock [`AccessibilityChecker`] / [`PasteBackend`] 断言
//! [`perform_paste`] 的分支决策:是否写剪贴板、是否触达模拟、失败回退文案。

use std::cell::RefCell;

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

/// 解密 mock：非 vault 条目不会触达它。`Ok(plaintext)` 模拟 Keychain 可用、
/// 解密成功；`Err` 模拟 Keychain 不可用 / 密钥不匹配。
struct MockDecryptor {
    result: Result<String, CryptoError>,
}

impl Decryptor for MockDecryptor {
    fn decrypt(&self, _stored_content: &str, _nonce: &[u8]) -> Result<String, CryptoError> {
        self.result.clone()
    }
}

/// 非 vault 分支使用的哨兵解密器：若被调用即 panic（断言 plain 路径不解密）。
struct NeverDecryptor;

impl Decryptor for NeverDecryptor {
    fn decrypt(&self, _stored_content: &str, _nonce: &[u8]) -> Result<String, CryptoError> {
        panic!("非加密条目不应调用解密器");
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
    wrote_clipboard: RefCell<Option<String>>,
    refocused: RefCell<bool>,
    simulated: RefCell<bool>,
}

impl PasteBackend for MockBackend {
    fn write_clipboard(&self, content: &str) {
        *self.wrote_clipboard.borrow_mut() = Some(content.to_string());
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

/// 构造一条普通(非加密)条目。
fn plain_entry(content: &str) -> Entry {
    Entry::from_row(
        1,
        "hash".to_string(),
        content.to_string(),
        String::new(),
        "plain".to_string(),
        0, // is_pinned
        None,
        0, // is_vault
        1, // is_decryptable
        0,
    )
}

/// 构造一条加密区条目(is_vault = 1)。
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
    )
}

#[test]
fn clipboard_written_even_when_not_trusted() {
    // 未授权 Accessibility 时:写剪贴板不需要该权限,故内容仍应进剪贴板,
    // 只是无法模拟 ⌘V 自动回贴 → NoAccessibilityClipboardOnly。
    // （取代旧的 paste_blocked_when_not_trusted:那条断言未授权就完全不写剪贴板,
    //  正是本次修复的 bug——双击/回贴时内容进不了系统剪贴板。）
    let checker = MockChecker { trusted: false };
    let backend = MockBackend::default();

    let result = perform_paste(
        &checker,
        &backend,
        &plain_entry("hello"),
        None,
        None,
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::NoAccessibilityClipboardOnly));
    // 内容仍应写入剪贴板,让用户能手动 ⌘V。
    assert_eq!(
        backend.wrote_clipboard.borrow().as_deref(),
        Some("hello"),
        "未授权时内容仍应写入剪贴板(写剪贴板不需要 Accessibility)"
    );
    // 但不应尝试模拟按键(那才需要授权)。
    assert!(!*backend.simulated.borrow(), "未授权时不应模拟 ⌘V 按键注入");
}

#[test]
fn vault_entry_not_written_when_not_trusted() {
    // 未授权 + vault 条目:解密仍在写剪贴板前发生。此处 Keychain 可用(解密成功),
    // 未授权只挡自动回贴,内容(解密后明文)仍写入剪贴板。
    let checker = MockChecker { trusted: false };
    let backend = MockBackend::default();
    let decryptor = MockDecryptor {
        result: Ok("vault-明文".to_string()),
    };
    let nonce = [0u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_entry(),
        None,
        Some(&nonce),
        &decryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::NoAccessibilityClipboardOnly));
    assert_eq!(
        backend.wrote_clipboard.borrow().as_deref(),
        Some("vault-明文"),
        "vault 条目解密成功后,未授权也应把明文写入剪贴板供手动 ⌘V"
    );
    assert!(!*backend.simulated.borrow(), "未授权时不应模拟按键");
}

#[test]
fn paste_proceeds_when_trusted() {
    let checker = MockChecker { trusted: true };
    // 前台应用未变化 → Pasted。
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
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    // 授权后应走「写剪贴板 + 模拟」分支。
    assert_eq!(
        backend.wrote_clipboard.borrow().as_deref(),
        Some("payload"),
        "授权后应把内容写入剪贴板"
    );
    assert!(*backend.refocused.borrow(), "应隐藏面板把焦点还给目标应用");
    assert!(*backend.simulated.borrow(), "应触达 ⌘V 模拟分支");
}

#[test]
fn window_switched_when_frontmost_changes() {
    let checker = MockChecker { trusted: true };
    // 注入后前台变成了另一个 app → WindowSwitched。
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
        &NeverDecryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::WindowSwitched));
    // 该分支对应的固定提示文案。
    assert_eq!(
        WINDOW_SWITCHED_MSG,
        "目标窗口已切换,内容已写入剪贴板,请手动 ⌘V 粘贴"
    );
}

#[test]
fn vault_entry_decrypts_when_key_present() {
    // U9：加密区条目在 Keychain 可用时解密成功并正常回贴（AE5）。
    // （U8 旧行为 vault_entry_returns_decrypt_seam_error 已被本用例取代：
    //  seam 由 U9 填实，vault 条目现在真正解密而非返回未实现错误。）
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        frontmost: Some("com.apple.TextEdit".to_string()),
        ..Default::default()
    };
    let decryptor = MockDecryptor {
        result: Ok("解密后的明文".to_string()),
    };
    let nonce = [0u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_entry(),
        Some("com.apple.TextEdit"),
        Some(&nonce),
        &decryptor,
    );

    assert_eq!(result, Ok(PasteOutcome::Pasted));
    // 写入剪贴板的是解密后的明文，而非存储中的密文表示。
    assert_eq!(
        backend.wrote_clipboard.borrow().as_deref(),
        Some("解密后的明文"),
        "vault 条目回贴应写入解密后的明文"
    );
    assert!(*backend.simulated.borrow(), "解密成功后应触达 ⌘V 模拟");
}

#[test]
fn vault_entry_fails_when_keychain_unavailable() {
    // U9：Keychain 不可用（解密器报错）→ VaultDecryptFailed，绝不把密文贴出。
    let checker = MockChecker { trusted: true };
    let backend = MockBackend::default();
    let decryptor = MockDecryptor {
        result: Err(CryptoError::KeyUnavailable("Keychain 不可用".to_string())),
    };
    let nonce = [0u8; 12];

    let result = perform_paste(
        &checker,
        &backend,
        &vault_entry(),
        None,
        Some(&nonce),
        &decryptor,
    );

    assert_eq!(result, Err(PasteError::VaultDecryptFailed));
    assert!(
        backend.wrote_clipboard.borrow().is_none(),
        "解密失败时不应把密文写入剪贴板"
    );
    assert!(!*backend.simulated.borrow(), "解密失败时不应触达模拟");
}

#[test]
fn vault_entry_fails_when_nonce_missing() {
    // U9：vault 条目缺 nonce（数据损坏）→ 解密失败，安全拒绝。
    let checker = MockChecker { trusted: true };
    let backend = MockBackend::default();
    let decryptor = MockDecryptor {
        result: Ok("不该被用到".to_string()),
    };

    let result = perform_paste(&checker, &backend, &vault_entry(), None, None, &decryptor);

    assert_eq!(result, Err(PasteError::VaultDecryptFailed));
    assert!(
        backend.wrote_clipboard.borrow().is_none(),
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
        &NeverDecryptor,
    );

    // 注入失败 → 仅剪贴板;内容仍已写入,提示用户手动 ⌘V。
    assert_eq!(result, Ok(PasteOutcome::ClipboardOnly));
    assert_eq!(
        backend.wrote_clipboard.borrow().as_deref(),
        Some("data"),
        "回退路径下内容仍应已在剪贴板"
    );
    assert_eq!(CGEVENT_FAIL_MSG, "回贴失败,内容已复制到剪贴板,请手动 ⌘V");
}
