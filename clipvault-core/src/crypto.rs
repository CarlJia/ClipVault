//! 纯 AES-256-GCM 加密算法（U9 / KTD4 / R12）。
//!
//! 本模块**不含任何 Keychain / macOS 符号**，只做机密性 + 完整性的对称加密，
//! 因此可在任意平台编译并被单元测试覆盖（round-trip / 篡改检测 / nonce 唯一 /
//! 错误密钥）。真实的数据密钥托管（Keychain）在 `app/src/crypto.rs` 的
//! [`KeyProvider`] 实现里完成，本模块只消费一个已解析的 32 字节密钥。
//!
//! # 设计
//!
//! - 每次 [`encrypt`] 生成一个 12 字节随机 nonce（GCM 标准 nonce 长度），随密文
//!   一起返回；调用方负责把 nonce 与密文一起持久化。GCM 的安全性依赖「同一密钥
//!   下 nonce 不重复」，随机 96-bit nonce 在本应用的写入量级下碰撞概率可忽略。
//! - 密文内含 GCM 认证标签，[`decrypt`] 在标签校验失败（密文被篡改、nonce 不符、
//!   密钥错误）时返回 [`CryptoError::Decrypt`]，绝不返回错误明文。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::RngCore;

/// GCM nonce 长度（字节）。
pub const NONCE_LEN: usize = 12;
/// 数据密钥长度（字节，AES-256）。
pub const KEY_LEN: usize = 32;

/// 加密子系统错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// 解密失败：GCM 认证标签校验不通过（密文被篡改、nonce 不符或密钥错误）。
    Decrypt,
    /// 数据密钥不可用（如 Keychain 不可访问）。携带底层原因描述。
    KeyUnavailable(String),
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::Decrypt => write!(f, "AES-GCM 解密失败（密文被篡改或密钥不匹配）"),
            CryptoError::KeyUnavailable(why) => write!(f, "数据密钥不可用：{why}"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// 数据密钥来源接缝。
///
/// 真实实现（`KeychainKeyProvider`）从 macOS Keychain 读取或创建；测试实现返回
/// 固定密钥或返回错误以驱动「Keychain 不可用」分支。放在 core 是为了让高层的
/// vault 加解密逻辑可以在无 macOS 的环境里以 mock 驱动测试。
///
/// `Send + Sync`：数据密钥在 Tauri async command / `spawn_blocking` 间被持有。
pub trait KeyProvider: Send + Sync {
    /// 返回当前的 32 字节数据密钥；不可用时返回 [`CryptoError::KeyUnavailable`]。
    fn data_key(&self) -> Result<[u8; KEY_LEN], CryptoError>;
}

/// 用 `key` 加密 `plaintext`，返回 `(密文, nonce)`。
///
/// nonce 为每次调用新生成的 12 字节随机值。密文已内含 GCM 认证标签。
///
/// # Panics
/// 仅当底层 AEAD 因超大输入（GCM 上限约 64 GiB）失败时 panic；剪贴板文本远低于
/// 该阈值，正常路径不会触发。
pub fn encrypt(key: &[u8; KEY_LEN], plaintext: &[u8]) -> (Vec<u8>, [u8; NONCE_LEN]) {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .expect("AES-GCM 加密不应在剪贴板文本量级下失败");
    (ciphertext, nonce_bytes)
}

/// 用 `key` 解密 `ciphertext`（配 `nonce`）。
///
/// # Errors
/// 认证标签校验失败（篡改 / nonce 不符 / 密钥错误）时返回 [`CryptoError::Decrypt`]。
pub fn decrypt(
    key: &[u8; KEY_LEN],
    ciphertext: &[u8],
    nonce: &[u8; NONCE_LEN],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(nonce);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| CryptoError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: [u8; KEY_LEN] = [7u8; KEY_LEN];

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let plaintext = b"super secret clipboard content";
        let (ciphertext, nonce) = encrypt(&TEST_KEY, plaintext);
        // 密文与明文不同（确实做了加密）。
        assert_ne!(ciphertext.as_slice(), plaintext.as_slice());
        let recovered = decrypt(&TEST_KEY, &ciphertext, &nonce).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn decrypt_tampered_fails() {
        let (mut ciphertext, nonce) = encrypt(&TEST_KEY, b"tamper me");
        // 翻转密文第一个字节 → GCM 认证失败。
        ciphertext[0] ^= 0xff;
        assert_eq!(
            decrypt(&TEST_KEY, &ciphertext, &nonce),
            Err(CryptoError::Decrypt),
        );
    }

    #[test]
    fn nonce_is_unique() {
        // 同一明文两次加密应得到不同 nonce 与不同密文。
        let (ct1, n1) = encrypt(&TEST_KEY, b"same plaintext");
        let (ct2, n2) = encrypt(&TEST_KEY, b"same plaintext");
        assert_ne!(n1, n2, "两次加密的 nonce 必须不同");
        assert_ne!(ct1, ct2, "不同 nonce 下同明文的密文也应不同");
        // 但都能解回原文。
        assert_eq!(decrypt(&TEST_KEY, &ct1, &n1).unwrap(), b"same plaintext");
        assert_eq!(decrypt(&TEST_KEY, &ct2, &n2).unwrap(), b"same plaintext");
    }

    #[test]
    fn wrong_key_fails() {
        let (ciphertext, nonce) = encrypt(&TEST_KEY, b"secret");
        let wrong_key = [9u8; KEY_LEN];
        assert_eq!(
            decrypt(&wrong_key, &ciphertext, &nonce),
            Err(CryptoError::Decrypt),
        );
    }
}
