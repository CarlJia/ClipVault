//! Keychain 托管的数据密钥 + 加密区内容编解码（U9 / KTD4 / R12）。
//!
//! # 分层
//!
//! - **纯算法**在 [`clipvault_core::crypto`]（AES-256-GCM，无平台符号，独立可测）。
//! - **本模块**提供：
//!   - [`Encryptor`] / [`Decryptor`] 接缝：供 [`crate::storage`] 的 `move_to_vault`
//!     与 [`crate::pasteback`] 的 `decrypt_content` 调用，密文以 base64 存进
//!     `entries.content`（TEXT），nonce 存 `entries.nonce`（BLOB）。
//!   - [`VaultCipher`]：泛型于 [`KeyProvider`] 的两个接缝实现（跨平台，测试用
//!     mock provider 驱动）。
//!   - [`KeychainKeyProvider`]（**仅 macOS**）：用 `security-framework` 从 Keychain
//!     读取/创建 `service = com.clipvault.data_key` 的 32 字节数据密钥，进程内缓存。
//!   - [`salted_content_hash`]：加密区条目的 `content_hash` 用 `hash(random_salt ||
//!     id)`，使该列不再是明文的可枚举指纹（KTD4 / plan U9）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use clipvault_core::crypto::{self, CryptoError, KEY_LEN, KeyProvider, NONCE_LEN};

/// `move_to_vault` 加密一条明文后的落盘表示。
#[derive(Debug, Clone)]
pub struct EncryptedContent {
    /// base64 编码的密文，写入 `entries.content`（TEXT 列，保持 UTF-8 合法）。
    pub content_b64: String,
    /// 12 字节 GCM nonce，写入 `entries.nonce`（BLOB 列）。
    pub nonce: [u8; NONCE_LEN],
}

/// 多类型 plan U6：`move_to_vault` 加密 binary payload 的落盘表示。
/// 密文是 AES-GCM 输出原始 bytes（写入 `entries.payload` BLOB 列）；
/// nonce 写入 `entries.payload_nonce`（BLOB 列）。注意 `EncryptedContent`
/// 与本结构不同——文本密文走 base64 是为了在 TEXT 列保持 UTF-8 合法，
/// BLOB 列没有这个限制。
#[derive(Debug, Clone)]
pub struct EncryptedBytes {
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; NONCE_LEN],
}

/// 加密接缝：明文 → 落盘表示。Keychain 不可用时返回 [`CryptoError::KeyUnavailable`]，
/// 调用方（`move_to_vault`）据此**拒绝加密**、不落盘任何明文到 vault（plan U9）。
///
/// `Send + Sync`：`move_to_vault` 在 Tauri async command（要求 `Send` future）内
/// 跨 await 持有 `&dyn Encryptor`。
pub trait Encryptor: Send + Sync {
    fn encrypt(&self, plaintext: &str) -> Result<EncryptedContent, CryptoError>;
    /// 多类型 plan U6：加密 vault binary payload BLOB。
    /// 默认实现：使用同一 KeyProvider key 做 AES-GCM，返回原始密文 bytes +
    /// 独立 nonce。密文是 GCM 输出（已含 tag），直接写入 BLOB 列。
    fn encrypt_bytes(&self, bytes: &[u8]) -> Result<EncryptedBytes, CryptoError> {
        let key = self.data_key()?;
        let (ciphertext, nonce) = crypto::encrypt(&key, bytes);
        Ok(EncryptedBytes { ciphertext, nonce })
    }
    /// Subclass hook: same pattern as Decryptor::data_key.
    fn data_key(&self) -> Result<[u8; clipvault_core::crypto::KEY_LEN], CryptoError>;
}

/// 解密接缝：`(base64 密文, nonce)` → 明文。密钥不可用或认证失败均返回 `Err`。
pub trait Decryptor: Send + Sync {
    fn decrypt(&self, stored_content: &str, nonce: &[u8]) -> Result<String, CryptoError>;
    /// 解密 vault 二进制 payload（U4 multi-type plan）。
    /// 默认实现：base64 解码后调 [`crypto::decrypt`]。子类可重写以处理
    /// 未 base64 编码的原始密文 BLOB。
    fn decrypt_bytes(&self, stored_bytes: &[u8], nonce: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let key = self.data_key()?;
        let nonce_arr: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Decrypt)?;
        let s = std::str::from_utf8(stored_bytes).map_err(|_| CryptoError::Decrypt)?;
        let ciphertext = B64.decode(s).map_err(|_| CryptoError::Decrypt)?;
        crypto::decrypt(&key, &ciphertext, &nonce_arr)
    }
    /// Subclass hook: give trait impls access to the underlying key provider
    /// for default-impl reuse. Concrete [`VaultCipher`] wires its own
    /// [`KeyProvider`].
    fn data_key(&self) -> Result<[u8; clipvault_core::crypto::KEY_LEN], CryptoError>;
}

/// 泛型于 [`KeyProvider`] 的 vault 加解密器。
///
/// 真机用 `VaultCipher::new(KeychainKeyProvider::new())`；测试注入 mock provider。
pub struct VaultCipher<P: KeyProvider> {
    provider: P,
}

impl<P: KeyProvider> VaultCipher<P> {
    pub fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P: KeyProvider> Encryptor for VaultCipher<P> {
    fn encrypt(&self, plaintext: &str) -> Result<EncryptedContent, CryptoError> {
        let key = self.provider.data_key()?;
        let (ciphertext, nonce) = crypto::encrypt(&key, plaintext.as_bytes());
        Ok(EncryptedContent {
            content_b64: B64.encode(ciphertext),
            nonce,
        })
    }

    fn data_key(&self) -> Result<[u8; clipvault_core::crypto::KEY_LEN], CryptoError> {
        self.provider.data_key()
    }
}

impl<P: KeyProvider> Decryptor for VaultCipher<P> {
    fn decrypt(&self, stored_content: &str, nonce: &[u8]) -> Result<String, CryptoError> {
        let key = self.provider.data_key()?;
        let ciphertext = B64
            .decode(stored_content)
            .map_err(|_| CryptoError::Decrypt)?;
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Decrypt)?;
        let plaintext = crypto::decrypt(&key, &ciphertext, &nonce)?;
        String::from_utf8(plaintext).map_err(|_| CryptoError::Decrypt)
    }

    fn data_key(&self) -> Result<[u8; clipvault_core::crypto::KEY_LEN], CryptoError> {
        self.provider.data_key()
    }
}

/// 加密区条目的 `content_hash`：`SHA-256(random_salt || id)` 的十六进制。
///
/// 普通条目的 `content_hash` 是明文 SHA-256 前缀（可枚举指纹）。vault 条目改用
/// 每条随机 salt，使 hash 列不再泄漏明文、也不可被字典枚举命中（KTD4）。随机
/// salt 保证 `content_hash` 的 UNIQUE 约束以压倒性概率成立。
pub fn salted_content_hash(id: i64) -> String {
    use rand::RngCore as _;
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);

    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(id.to_le_bytes());
    let digest = hasher.finalize();

    let mut hex = String::with_capacity(6 + digest.len() * 2);
    hex.push_str("vault:");
    for byte in digest.iter() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

// ================================================================
// macOS：Keychain 数据密钥
// ================================================================
#[cfg(target_os = "macos")]
mod keychain {
    use super::{CryptoError, KEY_LEN, KeyProvider};
    use security_framework::passwords::{get_generic_password, set_generic_password};
    use std::sync::Mutex;

    /// Keychain generic-password item 的 service 名（KTD4）。
    const SERVICE: &str = "com.clipvault.data_key";
    /// `errSecItemNotFound`：Keychain 中尚无该条目 → 首次使用，需创建。
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    /// 进程内数据密钥缓存。只缓存成功结果；Keychain 不可用时不缓存，
    /// 以便恢复后（如用户解锁 Keychain）下次调用重试成功、横幅可消失。
    static CACHE: Mutex<Option<[u8; KEY_LEN]>> = Mutex::new(None);

    /// 从 macOS Keychain 读取或首次创建 32 字节数据密钥的 [`KeyProvider`]。
    pub struct KeychainKeyProvider {
        account: String,
    }

    impl KeychainKeyProvider {
        /// account = 当前登录用户（`$USER`），回退到固定串。
        pub fn new() -> Self {
            let account = std::env::var("USER").unwrap_or_else(|_| "clipvault".to_string());
            Self { account }
        }
    }

    impl Default for KeychainKeyProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    impl KeyProvider for KeychainKeyProvider {
        fn data_key(&self) -> Result<[u8; KEY_LEN], CryptoError> {
            if let Some(key) = *CACHE.lock().expect("key cache poisoned") {
                return Ok(key);
            }
            let key = load_or_create(&self.account)?;
            *CACHE.lock().expect("key cache poisoned") = Some(key);
            Ok(key)
        }
    }

    fn load_or_create(account: &str) -> Result<[u8; KEY_LEN], CryptoError> {
        match get_generic_password(SERVICE, account) {
            Ok(bytes) => bytes.as_slice().try_into().map_err(|_| {
                CryptoError::KeyUnavailable("Keychain 中的数据密钥长度异常".to_string())
            }),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => {
                use rand::RngCore as _;
                let mut key = [0u8; KEY_LEN];
                rand::rngs::OsRng.fill_bytes(&mut key);
                set_generic_password(SERVICE, account, &key)
                    .map_err(|e| CryptoError::KeyUnavailable(format!("写入 Keychain 失败：{e}")))?;
                Ok(key)
            }
            Err(e) => Err(CryptoError::KeyUnavailable(format!(
                "读取 Keychain 失败：{e}"
            ))),
        }
    }
}

#[cfg(target_os = "macos")]
pub use keychain::KeychainKeyProvider;

/// 构造真机默认的 vault 加解密器（Keychain 数据密钥）。仅 macOS。
#[cfg(target_os = "macos")]
pub fn default_vault_cipher() -> VaultCipher<KeychainKeyProvider> {
    VaultCipher::new(KeychainKeyProvider::new())
}
