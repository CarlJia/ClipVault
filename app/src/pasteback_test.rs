//! U8 回贴决策逻辑单测(跨平台,不触碰真实 CGEvent / Accessibility)。
//!
//! 真实按键注入与真实 Accessibility 授权归 AE3 / AE7 的 macOS 真机手动验收;
//! 这里只用 mock [`AccessibilityChecker`] / [`PasteBackend`] 断言
//! [`perform_paste`] 的分支决策:是否写剪贴板、是否触达模拟、失败回退文案。

use std::cell::RefCell;

use clipvault_core::Entry;

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
fn paste_blocked_when_not_trusted() {
    let checker = MockChecker { trusted: false };
    let backend = MockBackend::default();

    let result = perform_paste(&checker, &backend, &plain_entry("hello"), None);

    assert_eq!(result, Err(PasteError::MissingAccessibility));
    // 未授权时绝不触碰剪贴板 / 焦点 / 模拟(AE7)。
    assert!(
        backend.wrote_clipboard.borrow().is_none(),
        "未授权时不应写入剪贴板"
    );
    assert!(!*backend.refocused.borrow(), "未授权时不应隐藏面板");
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
    );

    assert_eq!(result, Ok(PasteOutcome::WindowSwitched));
    // 该分支对应的固定提示文案。
    assert_eq!(
        WINDOW_SWITCHED_MSG,
        "目标窗口已切换,内容已写入剪贴板,请手动 ⌘V 粘贴"
    );
}

#[test]
fn vault_entry_returns_decrypt_seam_error() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend::default();

    let result = perform_paste(&checker, &backend, &vault_entry(), None);

    // is_vault=1 → U9 接缝错误,不 panic、不写剪贴板。
    assert_eq!(result, Err(PasteError::VaultDecryptUnimplemented));
    assert!(
        backend.wrote_clipboard.borrow().is_none(),
        "加密区条目在 U8 不应写入剪贴板"
    );
    assert!(!*backend.simulated.borrow(), "加密区条目不应触达模拟");
}

#[test]
fn cgevent_failure_falls_back_to_clipboard_only() {
    let checker = MockChecker { trusted: true };
    let backend = MockBackend {
        fail_inject: true,
        ..Default::default()
    };

    let result = perform_paste(&checker, &backend, &plain_entry("data"), None);

    // 注入失败 → 仅剪贴板;内容仍已写入,提示用户手动 ⌘V。
    assert_eq!(result, Ok(PasteOutcome::ClipboardOnly));
    assert_eq!(
        backend.wrote_clipboard.borrow().as_deref(),
        Some("data"),
        "回退路径下内容仍应已在剪贴板"
    );
    assert_eq!(CGEVENT_FAIL_MSG, "回贴失败,内容已复制到剪贴板,请手动 ⌘V");
}
