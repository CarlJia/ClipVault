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
//!   - [`CredentialKeyProvider`]（**仅 Windows**，U4）：从 Windows 凭据管理器读取/创建
//!     `TargetName = ClipVault/data_key` 的同一份 32 字节密钥，缓存策略与 macOS 侧一致。
//!     两端密钥**互不相通**（各存各的系统凭据库），但密码学完全一致。
//!   - [`salted_content_hash`]：加密区条目的 `content_hash` 用 `hash(random_salt ||
//!     id)`，使该列不再是明文的可枚举指纹（KTD4 / plan U9）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use clipvault_core::crypto::{self, CryptoError, KEY_LEN, KeyProvider, NONCE_LEN};
use std::time::Duration;

/// 系统凭据库访问预算（性能优化 W3 / R13）。
///
/// macOS `securityd` / Windows LSA RPC 一旦卡住，`data_key()` 会在 IPC 派发线程上
/// 无期限地 hold 住 —— 前端此时连不上任何命令（含搜索）。2 秒预算让调用方退回
/// 「凭据库不可用」，下次重试可能就快。2 秒既覆盖 securityd 冷启的常见量级，
/// 也不会让 UI 冻太久。
///
/// 首次创建 Keychain 条目在慢机上可能超过这个预算 —— 但那正是 `preload_spawn`
/// 要解决的问题：它在启动期后台预热，用户触发第一条加密命令时通常已经缓存命中。
pub const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// 在有限时间内跑一次阻塞式凭据库访问（W3 / R13）。
///
/// `data_key()` 是**同步** trait 方法（`KeyProvider` 定义），不能在它内部用
/// `tokio::time::timeout`。这里起一个专职线程跑 `op`，主线程 `recv_timeout` 等它：
/// - 有结果（无论 Ok/Err）就在预算内原样返回；
/// - 超时返回 `Err(KeyUnavailable)`，调用方按「凭据库不可用」处理；
/// - **超时后 worker 不会写进程内 `CACHE`** —— 缓存写入在调用方（`data_key`）
///   收到 `Ok` 之后才发生，所以不会出现「已超时但 cache 却被填了」的错位。
///
/// `timeout` 显式传入：生产代码用 [`KEYCHAIN_TIMEOUT`]；单测传 50 ms 级的小预算，
/// 让超时用例不会拖住测试套件 2 秒。
fn with_keychain_timeout<F>(timeout: Duration, op: F) -> Result<[u8; KEY_LEN], CryptoError>
where
    F: FnOnce() -> Result<[u8; KEY_LEN], CryptoError> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("clipvault-keychain".to_string())
        .spawn(move || {
            // send 失败 = 主线程已超时离开，这是预期路径，不算错误。
            let _ = tx.send(op());
        });
    if spawned.is_err() {
        return Err(CryptoError::KeyUnavailable(
            "无法创建凭据库访问线程".to_string(),
        ));
    }
    match rx.recv_timeout(timeout) {
        Ok(res) => res,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(CryptoError::KeyUnavailable(
            format!("系统凭据库访问超时（{} 秒）", timeout.as_secs()),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(CryptoError::KeyUnavailable(
            "凭据库访问线程异常终止".to_string(),
        )),
    }
}

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
    ///
    /// 默认实现：把 `stored_bytes` 当作**原始** GCM 密文直接解密——即
    /// [`Encryptor::encrypt_bytes`] 默认实现的逆运算。`payload` 是 BLOB 列，
    /// 密文不需要 base64（对比文本 `content` 走 base64 是为了在 TEXT 列保持
    /// UTF-8 合法），故这里**不做** base64 解码。
    fn decrypt_bytes(&self, stored_bytes: &[u8], nonce: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let key = self.data_key()?;
        let nonce_arr: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Decrypt)?;
        crypto::decrypt(&key, stored_bytes, &nonce_arr)
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
            // W3 / R13:带 2 秒超时,securityd 卡住时不让 IPC 派发线程无期限 hold。
            // 缓存写入放在拿到 Ok 之后 —— 超时不会填 cache。
            let account = self.account.clone();
            let key = super::with_keychain_timeout(super::KEYCHAIN_TIMEOUT, move || {
                load_or_create(&account)
            })?;
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

// ================================================================
// Windows：凭据管理器数据密钥（U4 / KTD13）
// ================================================================

/// Windows 侧的数据密钥托管。
///
/// 与 macOS 的关键差异：**两端各存各的密钥，互不相通**。同一份 SQLite 库文件在两台
/// 机器之间手工拷贝后，加密区条目在另一端解不开（钥匙在各自的系统凭据库里）。
/// 这是刻意的 —— 密钥从不离开本机，换来的代价就是库文件不便携，README 里已写明。
/// 密码学本身两端完全一致（同 AES-256-GCM、同 12 字节 nonce、同 v1 密文布局）。
#[cfg(target_os = "windows")]
mod cred {
    use std::sync::Mutex;

    use windows::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW, CredWriteW,
    };
    use windows::core::{HRESULT, PCWSTR, PWSTR};

    use super::{CryptoError, KEY_LEN, KeyProvider};

    /// 凭据条目的 TargetName。与 macOS 侧 Keychain 的 service 名
    /// （`com.clipvault.data_key`）是同一角色的两个平台写法。
    const TARGET: &str = "ClipVault/data_key";
    /// `ERROR_NOT_FOUND`（1168）：凭据库里还没有这个条目 → 首次使用，需要创建。
    /// 与 macOS 侧的 `errSecItemNotFound` 对应。
    const ERROR_NOT_FOUND: u32 = 1168;

    /// 进程内数据密钥缓存。只缓存成功结果；凭据库不可用时**不**缓存，以便恢复后
    /// （用户重新登录、组策略放开）下次调用能重试成功、前端横幅自行消失。
    static CACHE: Mutex<Option<[u8; KEY_LEN]>> = Mutex::new(None);

    /// 从 Windows 凭据管理器读取或首次创建 32 字节数据密钥的 [`KeyProvider`]。
    ///
    /// **选凭据管理器而不是"自己用 DPAPI 加密一个密钥文件"**，是因为它是 Keychain
    /// generic password 的直接对应物：同样按 (target, user) 寻址、同样由系统加密保管、
    /// 同样不需要应用自管密钥文件及其文件权限。`CRED_PERSIST_LOCAL_MACHINE` 是关键的一笔：
    /// 它保证密钥**不随漫游配置文件**同步到别的机器（凭据管理器默认会漫游，
    /// 那样密钥就跑出了本机，与 F3 的设计相悖）。
    pub struct CredentialKeyProvider {
        user: String,
    }

    impl CredentialKeyProvider {
        /// user = 当前登录用户名（`%USERNAME%`），取不到时回退到固定串。
        pub fn new() -> Self {
            let user = std::env::var("USERNAME").unwrap_or_else(|_| "clipvault".to_string());
            Self { user }
        }
    }

    impl Default for CredentialKeyProvider {
        fn default() -> Self {
            Self::new()
        }
    }

    impl KeyProvider for CredentialKeyProvider {
        fn data_key(&self) -> Result<[u8; KEY_LEN], CryptoError> {
            if let Some(key) = *CACHE.lock().expect("key cache poisoned") {
                return Ok(key);
            }
            // W3 / R13:带 2 秒超时,LSA RPC 卡住时不让 IPC 派发线程无期限 hold。
            let user = self.user.clone();
            let key = super::with_keychain_timeout(super::KEYCHAIN_TIMEOUT, move || {
                load_or_create(&user)
            })?;
            *CACHE.lock().expect("key cache poisoned") = Some(key);
            Ok(key)
        }
    }

    /// 读不到条目与读失败是两件事：前者要创建，后者要如实报错（不静默新建密钥 ——
    /// 那会让原本可解密的条目永久解不开）。
    enum ReadError {
        NotFound,
        Other(String),
    }

    fn load_or_create(user: &str) -> Result<[u8; KEY_LEN], CryptoError> {
        match read() {
            Ok(bytes) => bytes.as_slice().try_into().map_err(|_| {
                CryptoError::KeyUnavailable("凭据管理器中的数据密钥长度异常".to_string())
            }),
            Err(ReadError::NotFound) => {
                use rand::RngCore as _;
                let mut key = [0u8; KEY_LEN];
                rand::rngs::OsRng.fill_bytes(&mut key);
                write(user, &key)
                    .map_err(|e| CryptoError::KeyUnavailable(format!("写入凭据管理器失败：{e}")))?;
                Ok(key)
            }
            Err(ReadError::Other(detail)) => Err(CryptoError::KeyUnavailable(format!(
                "读取凭据管理器失败：{detail}"
            ))),
        }
    }

    /// 按 TargetName 读取。**不需要用户名**：Windows 凭据只以 TargetName 为索引
    /// （`UserName` 是随凭据一起存的元数据，不参与查找）。这与 macOS 不同 ——
    /// Keychain generic password 是按 (service, account) 二元组索引的。
    /// 两者的**隔离性**没有区别：Windows 凭据库本身就是按登录用户隔离的。
    fn read() -> Result<Vec<u8>, ReadError> {
        let target = wide(TARGET);
        let mut ptr: *mut CREDENTIALW = std::ptr::null_mut();
        unsafe {
            if let Err(e) = CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) {
                return Err(if e.code() == HRESULT::from_win32(ERROR_NOT_FOUND) {
                    ReadError::NotFound
                } else {
                    // 其余都是失败（凭据库被策略禁用、条目属于别的用户等），如实上报。
                    ReadError::Other(e.to_string())
                });
            }
            if ptr.is_null() {
                return Err(ReadError::NotFound);
            }
            // 返回的 CREDENTIALW 及其 bloc 由 CredFree 释放（不是 free/GlobalFree）。
            let cred = &*ptr;
            let bytes = if cred.CredentialBlobSize == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize)
                    .to_vec()
            };
            CredFree(ptr as *const core::ffi::c_void);
            Ok(bytes)
        }
    }

    fn write(user: &str, key: &[u8; KEY_LEN]) -> windows::core::Result<()> {
        let mut target = wide(TARGET);
        let mut user_wide = wide(user);
        let mut blob = key.to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            UserName: PWSTR(user_wide.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        // flags 传 0：不做 CRED_PRESERVE_CREDENTIAL_BLOB 之类的特殊保留语义。
        unsafe { CredWriteW(&credential, 0) }
    }

    /// UTF-16 + NUL 结尾（Win32 宽字符字符串的通用形式）。
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(target_os = "windows")]
pub use cred::CredentialKeyProvider;

/// 构造真机默认的 vault 加解密器（Windows 凭据管理器数据密钥）。
#[cfg(target_os = "windows")]
pub fn default_vault_cipher() -> VaultCipher<CredentialKeyProvider> {
    VaultCipher::new(CredentialKeyProvider::new())
}

/// 系统凭据库当前是否可用（macOS Keychain / Windows 凭据管理器）。
///
/// 加密区的可用性判据，供 `commands::keychain_available` 与启动校验共用。首次调用可能
/// 触发密钥创建；成功后进程内缓存，所以这个探测是廉价的。
///
/// 直接问 provider 而不是走 `VaultCipher`：`VaultCipher` 同时实现了 `Encryptor` 与
/// `Decryptor`，两者都有 `data_key`，走它反而需要在调用点消歧义。
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn system_key_available() -> bool {
    use clipvault_core::crypto::KeyProvider as _;
    #[cfg(target_os = "macos")]
    {
        KeychainKeyProvider::new().data_key().is_ok()
    }
    #[cfg(target_os = "windows")]
    {
        CredentialKeyProvider::new().data_key().is_ok()
    }
}

/// 后台预热数据密钥（性能优化 W3 / R13）。
///
/// `main::setup` 调一次，起一个 detached 线程去访问系统凭据库。冷启动首次访问
/// 常要几百毫秒（`securityd` / LSA 冷启、首次创建条目），若撞上用户第一条加密 /
/// 解密 IPC 就是白等一次（且在 2 秒超时下可能直接失败）。预热把它挪到启动期后台，
/// 用户触发时基本都已 cache hit。
///
/// 失败静默：预热失败只影响本次；之后每次 `data_key` 都会重试，并会被
/// `system_key_available` / `keychain_available` 如实报出。
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn preload_spawn() {
    std::thread::Builder::new()
        .name("clipvault-key-preload".to_string())
        .spawn(|| {
            use clipvault_core::crypto::KeyProvider as _;
            #[cfg(target_os = "macos")]
            {
                let _ = KeychainKeyProvider::new().data_key();
            }
            #[cfg(target_os = "windows")]
            {
                let _ = CredentialKeyProvider::new().data_key();
            }
        })
        .ok();
}

/// 非 macOS / 非 Windows：无系统凭据库，预热无事可做。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn preload_spawn() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// W3 / R13:op 在预算内返回时,原样透传结果、不额外拖延。
    #[test]
    fn with_keychain_timeout_returns_result_when_op_finishes_in_budget() {
        let start = std::time::Instant::now();
        let res = with_keychain_timeout(Duration::from_millis(50), || Ok([7u8; KEY_LEN]));
        assert_eq!(res.unwrap(), [7u8; KEY_LEN]);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "立即返回的 op 不该等满预算,实际 {:?}",
            start.elapsed()
        );
    }

    /// W3 / R13:op 自己返回 `Err`(例如读凭据库失败)时原样透传,不要变成超时错误。
    #[test]
    fn with_keychain_timeout_propagates_operation_error() {
        let res = with_keychain_timeout(Duration::from_millis(50), || {
            Err(CryptoError::KeyUnavailable("mock fail".to_string()))
        });
        assert_eq!(
            res,
            Err(CryptoError::KeyUnavailable("mock fail".to_string())),
            "op 的 Err 必须原样透传"
        );
    }

    /// W3 / R13 的核心:`op` 挂住超过预算时必须返回 `Err`(而不是无期限等)。
    ///
    /// 用 50 ms 的小预算 + 睡 500 ms 的 op,测试只耗 ~50 ms;这样超时路径既被
    /// 覆盖,又不会拖慢测试套件。
    #[test]
    fn with_keychain_timeout_returns_err_when_op_exceeds_budget() {
        let timeout = Duration::from_millis(50);
        let start = std::time::Instant::now();
        let res = with_keychain_timeout(timeout, move || {
            std::thread::sleep(timeout * 10);
            Ok([1u8; KEY_LEN])
        });
        let elapsed = start.elapsed();
        assert!(
            matches!(res, Err(CryptoError::KeyUnavailable(_))),
            "op 慢于预算必须超时返回 Err"
        );
        assert!(elapsed >= timeout, "应至少等满预算,实际 {elapsed:?}");
        assert!(
            elapsed < timeout * 5,
            "必须在 op 完成前返回(不等 500ms),实际 {elapsed:?}"
        );
    }
}
