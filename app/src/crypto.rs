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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// 系统凭据库访问预算（性能优化 W3 / R13）。
///
/// macOS `securityd` / Windows LSA RPC 一旦卡住，`data_key()` 会在 IPC 派发线程上
/// 无期限地 hold 住 —— 前端此时连不上任何命令（含搜索）。2 秒预算让调用方退回
/// 「凭据库不可用」，下次重试可能就快。2 秒既覆盖 securityd 冷启的常见量级，
/// 也不会让 UI 冻太久。
///
/// 首次创建 Keychain 条目在慢机上可能超过这个预算 —— 这就是 `data_key_cached`
/// 用 INFLIGHT 串行化的另一个理由：cold-start 期间只放一个 caller 进 `load_or_create`,
/// 避免 N 个 worker 并行 drive securityd 触发 N 次 ACL 对话框。
pub const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// 在有限时间内跑一次阻塞式凭据库访问（W3 / R13）。
///
/// **仍保留**作为测试基建 —— v2 改造里 `data_key_cached` 内部已经直接 `std::thread::spawn` +
/// `Condvar.wait_timeout` 不再走 mpsc 通道,所以这个函数在 production 是 dead code,
/// 但 `with_keychain_timeout_*` 三条单测在测"spawn worker + 用 mpsc 实现 timeout"这个原语,
/// 是合法的并发原语测试。删函数会丢测试;加 `#[allow(dead_code)]` 是最低代价。
#[allow(dead_code)]
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

/// KeyProvider 慢路径公共实现（v2 — 钉死「N 个串行 caller 也只 spawn 一次 worker」）。
///
/// **背景与演进**：
/// - v1 用 `Mutex<()>` + 各自 `mpsc::sync_channel` 在慢路径串行化 —— 防住了
///   "多个 caller **同时**调 `data_key`"，但挡不住"**顺序**循环里 N 个 caller"
///   （`verify_decryptable_on_startup` 的 entries 循环就是这形态）。每个顺序 caller
///   在前一个 worker 超时离开后仍 CACHE miss、各自 spawn 新 worker、各独立触发
///   securityd 会话 → vault 条目数 = ACL 弹框数。
/// - v2（此处）换成 `AtomicBool`「in-flight worker 标志」+ `Condvar`「等 CACHE」：
///   * 同一时刻最多一个 worker 在跑（标志位 `swap` 互斥）；
///   * 拿不到标志位的 caller 走 Condvar 等 CACHE,等不到(超时)就 Err
///     但**绝不**再 spawn 新 worker —— worker 由首批 caller 负责；
///   * worker 完成后不论 Ok/Err 都释放标志位 + `notify_all`,让下一批 caller
///     cache-hit 重用 (Ok) 或重新 spawn (Err,仍要重试)。
///
/// 跨平台：macOS / Windows 各自的 `mod keychain` / `mod cred` 各持自己的
/// `CACHE` / `INFLIGHT_RUNNING` / `CVAR`,helper 接受静态引用。
fn data_key_cached<F>(
    cache: &'static Mutex<Option<[u8; KEY_LEN]>>,
    inflight_running: &'static AtomicBool,
    cvar: &'static Condvar,
    timeout: Duration,
    loader: F,
) -> Result<[u8; KEY_LEN], CryptoError>
where
    F: FnOnce() -> Result<[u8; KEY_LEN], CryptoError> + Send + 'static,
{
    // Fast path:CACHE 命中直接返回,完全不进同步逻辑。
    if let Some(key) = *cache.lock().expect("key cache poisoned") {
        return Ok(key);
    }

    // 抢"仅有一个 worker"标志。swap 返回旧值:`false` 表示我们是首批 caller,
    // 由我们负责 spawn worker;`true` 表示已有 worker 在跑,我们只等 CACHE。
    if !inflight_running.swap(true, Ordering::AcqRel) {
        let _ = std::thread::Builder::new()
            .name("clipvault-keychain".to_string())
            .spawn(move || {
                let result = loader();
                // CACHE 写在 worker 内(CACHE 也是 CVAR 的 pair mutex):
                // 主线程即使超时离开,worker 成功仍写 cache,通知所有等的人。
                if let Ok(ref k) = result {
                    *cache.lock().expect("key cache poisoned") = Some(*k);
                }
                // 不论 Ok/Err 都释放标志位 + notify —— 等待者要么 cache-hit
                // (Ok 路径),要么超时/重试 (Err 路径仍有下一批 caller 负责)。
                inflight_running.store(false, Ordering::Release);
                cvar.notify_all();
            });
    }

    // 等 CACHE —— 自己的 timeout 跟 worker 生命周期独立,超时就返 Err,
    // 后续 caller 仍可被已写 CACHE 或新批 caller 唤醒。
    let deadline = Instant::now() + timeout;
    let mut guard = cache.lock().expect("key cache poisoned");
    loop {
        if let Some(key) = *guard {
            return Ok(key);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CryptoError::KeyUnavailable(format!(
                "等待系统凭据库访问超时（{} 秒）",
                timeout.as_secs()
            )));
        }
        let (g, _) = cvar.wait_timeout(guard, remaining).expect("cvar poisoned");
        guard = g;
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
    use std::sync::atomic::AtomicBool;
    use std::sync::{Condvar, Mutex};

    /// Keychain generic-password item 的 service 名（KTD4）。
    const SERVICE: &str = "com.clipvault.data_key";
    /// `errSecItemNotFound`：Keychain 中尚无该条目 → 首次使用，需创建。
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    /// 进程内数据密钥缓存。只缓存成功结果；Keychain 不可用时不缓存，
    /// 以便恢复后（如用户解锁 Keychain）下次调用重试成功、横幅可消失。
    static CACHE: Mutex<Option<[u8; KEY_LEN]>> = Mutex::new(None);
    /// W3 收尾 / R13(v2):「in-flight worker 标志」—— 同进程同一时刻保证只
    /// 一个 `data_key` 慢路径在跑,详见 [`data_key_cached`].
    static INFLIGHT_RUNNING: AtomicBool = AtomicBool::new(false);
    /// `CACHE` 同时是 `CVAR` 的 pair mutex —— worker 写 cache 时锁的就是它，
    /// 等的人也通过它 wait_timeout。CACHE / CVAR / INFLIGHT_RUNNING 三者同寿命。
    static CVAR: Condvar = Condvar::new();

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
            let account = self.account.clone();
            super::data_key_cached(
                &CACHE,
                &INFLIGHT_RUNNING,
                &CVAR,
                super::KEYCHAIN_TIMEOUT,
                move || load_or_create(&account),
            )
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
    use std::sync::Condvar;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

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
    /// W3 收尾 / R13(v2):与 macOS 同形态,详见 [`data_key_cached`].
    static INFLIGHT_RUNNING: AtomicBool = AtomicBool::new(false);
    static CVAR: Condvar = Condvar::new();

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
            let user = self.user.clone();
            super::data_key_cached(
                &CACHE,
                &INFLIGHT_RUNNING,
                &CVAR,
                super::KEYCHAIN_TIMEOUT,
                move || load_or_create(&user),
            )
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

    /// W3 收尾 / R13(v2):两个并发首调用,只有 caller 1 spawn worker,
    /// caller 2 等 CACHE —— 必同 key,loader 仅被调一次。
    ///
    /// 之前 INFLIGHT-Mutex-跑 30ms-OK 那种场景在 v2 下行为一致:
    /// swap 互斥 + cache 写 + notify 让第二个 caller 从 cache 拿,
    /// 不重复跑 loader。
    #[test]
    fn data_key_cached_concurrent_first_callers_only_spawn_one_worker() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        use std::thread;

        let cache: &'static Mutex<Option<[u8; KEY_LEN]>> = Box::leak(Box::new(Mutex::new(None)));
        let inflight: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let cvar: &'static Condvar = Box::leak(Box::new(Condvar::new()));

        let counter = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let c1 = counter.clone();
        let b1 = barrier.clone();
        let h1 = thread::spawn(move || {
            b1.wait();
            data_key_cached(
                cache,
                inflight,
                cvar,
                Duration::from_millis(200),
                move || {
                    c1.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    Ok([11u8; KEY_LEN])
                },
            )
        });

        let c2 = counter.clone();
        let b2 = barrier.clone();
        let h2 = thread::spawn(move || {
            b2.wait();
            data_key_cached(
                cache,
                inflight,
                cvar,
                Duration::from_millis(200),
                move || {
                    c2.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    Ok([22u8; KEY_LEN])
                },
            )
        });

        let r1 = h1.join().expect("thread 1 shouldn't panic");
        let r2 = h2.join().expect("thread 2 shouldn't panic");

        let k1 = r1.expect("first caller should get Ok");
        let k2 = r2.expect("second caller should get Ok");
        assert_eq!(
            k1, k2,
            "两个 caller 必须拿到同一个 key —— 第二个走 cache-hit,取的是第一个 loader 写入的值"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "INFLIGHT_RUNNING 必须保证只 spawn 一次 worker,实际 {} 次",
            counter.load(Ordering::SeqCst)
        );
    }

    /// W3 收尾 / R13(v2):即便主线程超时离开,worker 完成后仍写 CACHE;
    /// 下个 caller 必须 cache-hit、零额外 keychain 访问。
    ///
    /// 钉死这条不变量,后续 refactor 不会退化到「主线程 Err → cache 留空」
    /// 的写法 —— 那会让每个下一次调用都重新 spawn worker,securityd 慢机上
    /// 的根症状(反复弹 ACL)直接复活。
    #[test]
    fn data_key_cached_writes_cache_from_worker_even_when_caller_times_out() {
        let cache: &'static Mutex<Option<[u8; KEY_LEN]>> = Box::leak(Box::new(Mutex::new(None)));
        let inflight: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let cvar: &'static Condvar = Box::leak(Box::new(Condvar::new()));

        // 第一次:loader sleep 200ms,主线程 timeout 50ms → 应得 Err。
        let res1 = data_key_cached(cache, inflight, cvar, Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(200));
            Ok([7u8; KEY_LEN])
        });
        assert!(
            matches!(res1, Err(CryptoError::KeyUnavailable(_))),
            "第一次调用应因超时返回 Err,实际 {res1:?}"
        );

        // 等 worker 跑完 + 写 cache(200ms sleep + 一点 margin)。
        std::thread::sleep(Duration::from_millis(300));

        // 第二次:cache 已被 worker 写入,loader 不该再被调用 —— 它 panic 即失败信号。
        let res2 = data_key_cached(cache, inflight, cvar, Duration::from_millis(50), || {
            panic!("worker 完成后写入的 cache 必须被复用 —— loader 不应再跑");
        });
        assert_eq!(res2.unwrap(), [7u8; KEY_LEN]);
    }

    /// W3 收尾 / R13(v2)的真正钉死根症状的测试:
    ///
    /// 模拟 `verify_decryptable_on_startup` 的 entries 循环 ——**顺序**地
    /// 调 N 次 `data_key`(每条 vault entry 一次 `decrypt()` → `data_key()`)。
    /// 在 v1(INFLIGHT Mutex)下每个顺序调用在前一个 worker 超时后仍 CACHE miss、
    /// 各 spawn 新 worker → N 次 securityd 会话 → vault 条目数 = ACL 弹框数。
    ///
    /// v2(AtomicBool + Condvar)下:首个 spawn worker,N-1 个后到的顺序 caller
    /// 看见 `INFLIGHT_RUNNING=true` → 等 CACHE → worker 完成后 cache-hit。
    /// loader 总共应**只**被调一次。
    #[test]
    fn data_key_cached_sequential_verify_loop_callers_only_spawn_one_worker() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache: &'static Mutex<Option<[u8; KEY_LEN]>> = Box::leak(Box::new(Mutex::new(None)));
        let inflight: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let cvar: &'static Condvar = Box::leak(Box::new(Condvar::new()));

        // 先把 cache 跑空 —— 模拟启动时的冷启路径(verify_decryptable_on_startup
        // 也是从 cache 空开始跑)。
        assert!(cache.lock().expect("...").is_none());

        let counter = Arc::new(AtomicUsize::new(0));

        let mut expected_key = [0u8; KEY_LEN];
        for i in 0..5 {
            let c = counter.clone();
            let res = data_key_cached(
                cache,
                inflight,
                cvar,
                Duration::from_millis(500),
                move || {
                    c.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    Ok([7u8; KEY_LEN])
                },
            )
            .expect("loop caller should get Ok (worker writes cache before i>0)");
            if i == 0 {
                expected_key = res;
            } else {
                assert_eq!(
                    res, expected_key,
                    "所有顺序 caller 必须拿同一个 key —— 第 {} 次没走 cache-hit",
                    i
                );
            }
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "5 个顺序 caller 模拟 verify 循环 —— loader 必须只被调 1 次,实际 {} 次",
            counter.load(Ordering::SeqCst)
        );
    }
}
