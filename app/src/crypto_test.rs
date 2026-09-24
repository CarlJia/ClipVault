//! 加密区存储 + 加解密的集成测试（U9）。
//!
//! 用 **mock [`KeyProvider`]** 提供固定密钥，因此整套 round-trip / FTS 排除 /
//! Keychain-不可用拒绝 / 不可解密清除都能在无 macOS Keychain 的环境里确定性地跑。
//! 真实 Keychain 路径（`KeychainKeyProvider`）归 AE5 macOS 真机手动验收。
//! 纯 AES-GCM 算法用例（round-trip / 篡改 / nonce 唯一 / 错误密钥）在
//! `clipvault-core::crypto` 内。

use clipvault_core::crypto::{CryptoError, KEY_LEN, KeyProvider};

use crate::crypto::VaultCipher;
use crate::pasteback::decrypt_content;
use crate::storage::{VaultError, test_helpers::open_test_db};

/// 固定密钥的 mock provider（Keychain 可用）。
struct MockKeyProvider {
    key: [u8; KEY_LEN],
}

impl MockKeyProvider {
    fn fixed() -> Self {
        Self {
            key: [42u8; KEY_LEN],
        }
    }
    fn with_key(byte: u8) -> Self {
        Self {
            key: [byte; KEY_LEN],
        }
    }
}

impl KeyProvider for MockKeyProvider {
    fn data_key(&self) -> Result<[u8; KEY_LEN], CryptoError> {
        Ok(self.key)
    }
}

/// 永远失败的 mock provider（模拟 Keychain 不可用）。
struct FailingKeyProvider;

impl KeyProvider for FailingKeyProvider {
    fn data_key(&self) -> Result<[u8; KEY_LEN], CryptoError> {
        Err(CryptoError::KeyUnavailable(
            "模拟 Keychain 不可用".to_string(),
        ))
    }
}

/// 查询某 rowid 是否在 FTS 索引中（用于断言 vault 排除）。
async fn fts_row_count(db: &crate::storage::Storage, id: i64) -> i64 {
    db.with_conn(move |conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM entries_fts WHERE rowid = ?1",
            [id],
            |row| row.get(0),
        )
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn move_to_vault_encrypts() {
    let db = open_test_db();
    let id = db
        .insert_entry("plainhash", "secret plaintext", "plain")
        .await
        .unwrap();
    // 入库后应在 FTS 中。
    assert_eq!(fts_row_count(&db, id).await, 1);

    let cipher = VaultCipher::new(MockKeyProvider::fixed());
    db.move_to_vault(id, &cipher).await.unwrap();

    let entry = db.get_by_id(id).await.unwrap().unwrap();
    assert!(entry.is_vault, "移入后 is_vault 应为 true");
    assert!(entry.is_decryptable, "刚加密的条目应可解密");
    assert_ne!(
        entry.content, "secret plaintext",
        "content 应变为密文（base64），不再是明文"
    );
    assert!(
        entry.content_hash.starts_with("vault:"),
        "vault 条目应使用 salted content_hash，实际为 {}",
        entry.content_hash
    );

    // nonce 已设置且为 12 字节。
    let nonce = db.get_vault_nonce(id).await.unwrap();
    assert_eq!(nonce.as_ref().map(|n| n.len()), Some(12));

    // 完整排除出 FTS。
    assert_eq!(
        fts_row_count(&db, id).await,
        0,
        "vault 条目必须完整退出 FTS 索引"
    );
}

#[tokio::test]
async fn vault_decrypt_roundtrip() {
    let db = open_test_db();
    let id = db
        .insert_entry("h", "重要的加密内容 42", "plain")
        .await
        .unwrap();

    let cipher = VaultCipher::new(MockKeyProvider::fixed());
    db.move_to_vault(id, &cipher).await.unwrap();

    // 通过 pasteback 的解密接缝还原明文（同一把 key）。
    let entry = db.get_by_id(id).await.unwrap().unwrap();
    let nonce = db.get_vault_nonce(id).await.unwrap();
    let plaintext = decrypt_content(&entry, nonce.as_deref(), &cipher).unwrap();
    assert_eq!(plaintext, "重要的加密内容 42");
}

#[tokio::test]
async fn keychain_unavailable_refuses() {
    let db = open_test_db();
    let id = db
        .insert_entry("h", "should stay plaintext", "plain")
        .await
        .unwrap();

    let cipher = VaultCipher::new(FailingKeyProvider);
    let err = db.move_to_vault(id, &cipher).await.unwrap_err();
    assert!(
        matches!(err, VaultError::Crypto(_)),
        "Keychain 不可用应返回 Crypto 错误，实际 {err:?}"
    );

    // 条目保持明文、is_vault=0（绝不静默写明文进 vault）。
    let entry = db.get_by_id(id).await.unwrap().unwrap();
    assert!(!entry.is_vault, "加密失败后条目必须仍是非 vault");
    assert_eq!(
        entry.content, "should stay plaintext",
        "加密失败后内容必须保持明文不变"
    );
    // 仍在 FTS 中（可正常搜索）。
    assert_eq!(fts_row_count(&db, id).await, 1);
}

#[tokio::test]
async fn undecryptable_marked_and_cleared() {
    let db = open_test_db();
    let id = db
        .insert_entry("h", "migrated secret", "plain")
        .await
        .unwrap();

    // 用 keyA 加密。
    let cipher_a = VaultCipher::new(MockKeyProvider::with_key(1));
    db.move_to_vault(id, &cipher_a).await.unwrap();
    assert!(db.get_by_id(id).await.unwrap().unwrap().is_decryptable);

    // 模拟迁移到新机器：现在只有 keyB（错误 key）。启动校验应把它标为不可解密。
    let cipher_b = VaultCipher::new(MockKeyProvider::with_key(2));
    let report = db.verify_decryptable_on_startup(&cipher_b).await.unwrap();
    assert_eq!(report.checked, 1);
    assert_eq!(report.undecryptable, 1);
    assert!(
        !db.get_by_id(id).await.unwrap().unwrap().is_decryptable,
        "错误 key 下应标记为不可解密"
    );

    // 批量清除不可解密条目。
    let removed = db.clear_undecryptable().await.unwrap();
    assert_eq!(removed, 1);
    assert!(
        db.get_by_id(id).await.unwrap().is_none(),
        "不可解密条目应被清除"
    );
}

#[tokio::test]
async fn vault_excluded_from_fts() {
    // 确认 move_to_vault 后密文不进入全文索引（避免 hex/base64 token 命中、note 泄漏）。
    let db = open_test_db();
    let id = db
        .insert_entry("h", "top secret webhook token", "plain")
        .await
        .unwrap();
    // 加密前可被 FTS 命中。
    let hits_before = db
        .search_entries(crate::search::build_search_query(
            "webhook",
            &crate::search::parse_tab("all"),
            10,
        ))
        .await
        .unwrap();
    assert!(hits_before.iter().any(|e| e.id == id));

    let cipher = VaultCipher::new(MockKeyProvider::fixed());
    db.move_to_vault(id, &cipher).await.unwrap();

    // 加密后不再被全文搜索命中。
    let hits_after = db
        .search_entries(crate::search::build_search_query(
            "webhook",
            &crate::search::parse_tab("all"),
            10,
        ))
        .await
        .unwrap();
    assert!(
        !hits_after.iter().any(|e| e.id == id),
        "vault 条目不应出现在全文搜索结果中"
    );
    assert_eq!(fts_row_count(&db, id).await, 0);
}
