//! 列表旁预览栏的后端（列表旁预览栏 plan U1 / R5 / R6 / R7）。
//!
//! 一条 IPC 命令 [`crate::commands::get_entry_preview`] 的分层：
//! - [`preview_dimensions`]：纯算术，「长边 ≤ 上限、等比」的缩放目标尺寸；
//! - [`make_preview_image`]：解码 → 缩放 → PNG re-encode（复用 `image` crate，
//!   与 [`crate::thumbnail::make_thumbnail`] 同一套写法）；
//! - [`get_entry_preview_inner`]：按 `content_kind` 分派，加密区条目走
//!   `pasteback::decrypt_content` / `decrypt_payload` 的现有解密路径。
//!
//! ## 为什么图片要先缩放
//!
//! 原图（屏幕截图常 20MB+）base64 之后进 IPC、再灌进 WebView 是双重开销：一次
//! 大字符串跨边界拷贝 + 一份常驻 WebView 的位图。预览栏只有约 400pt 宽，1600px
//! 长边已经远超它的像素需求（4K 屏 2x 缩放后约 2000 物理像素，取 1600 是折中）。
//!
//! ## 加密区
//!
//! 加密的是 `payload`（图片原字节）/`content`（文本），不是 64×64 的缩略图
//! （那是明文 `metadata_json` 的一部分）。所以预览必须解密；`is_decryptable = false`
//! 时**一律不解密**，直接回 `undecryptable: true`，绝不下发明文。
//!
//! 解密结果只经这条命令回到前端一次，不进任何持久化路径。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clipvault_core::ContentKind;
use serde::Serialize;

use crate::crypto::Decryptor;
use crate::storage::Storage;

/// 预览图长边上限（plan KTD4）。
pub const PREVIEW_MAX_DIM: u32 = 1600;

/// 缩放后的 PNG 字节 + 渲染尺寸。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 预览栏的数据（与前端 `PreviewDto` 一一对应）。
///
/// 定义在 preview 模块而不是 `commands.rs`：它是这条命令的**返回值契约**，
/// 与缩放 / 解密逻辑放在一起才不会各改一半。`commands.rs` 用 `pub use` 把它
/// 带到命令层（与 [`crate::commands::EntryDto`] 并列可见）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewDto {
    /// `"image"` | `"text"` | `"none"`。
    pub kind: String,
    /// 图片预览的 MIME（重编码后恒为 `image/png`）。文本与 `none` 为 `None`。
    pub mime: Option<String>,
    /// `data:image/png;base64,...`。仅 `kind = "image"` 且解密/解码成功时有值。
    pub data_url: Option<String>,
    /// 完整文本内容。仅 `kind = "text"` 且解密成功时有值。
    pub content: Option<String>,
    /// 预览图渲染宽高（已缩放）。仅图片有值。
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 加密区条目无法解密（密钥缺失 / 已变更 / 密文损坏）。
    ///
    /// 与「id 不存在」的区别是：那种情况命令返回 `Err(String)`。
    pub undecryptable: bool,
}

impl PreviewDto {
    /// file / rtf / color：没有可展示的预览，前端显示一句提示。
    pub fn none() -> Self {
        Self {
            kind: "none".to_string(),
            mime: None,
            data_url: None,
            content: None,
            width: None,
            height: None,
            undecryptable: false,
        }
    }

    /// 加密区条目解不开：**不带任何内容**，只报「无法解密」。
    fn undecryptable(kind: ContentKind) -> Self {
        Self {
            kind: kind.as_str().to_string(),
            undecryptable: true,
            ..Self::none()
        }
    }

    fn image(img: &PreviewImage) -> Self {
        Self {
            kind: ContentKind::Image.as_str().to_string(),
            mime: Some("image/png".to_string()),
            data_url: Some(format!(
                "data:image/png;base64,{}",
                STANDARD.encode(&img.png)
            )),
            width: Some(img.width),
            height: Some(img.height),
            ..Self::none()
        }
    }

    fn text(content: String) -> Self {
        Self {
            kind: ContentKind::Text.as_str().to_string(),
            content: Some(content),
            ..Self::none()
        }
    }
}

/// 等比缩放后的目标尺寸：长边**恰好**不超过 `max_dim`，原图更小则原样返回。
///
/// 长边直接钉成 `max_dim`、短边按比例四舍五入（下限 1），而不是两边各自
/// `round()` —— 后者会让 3200×1600 这种整数比也在四舍五入后偏 1px，测试与
/// 肉眼都会觉得「没缩到位」。宽或高为 0 的退化输入原样返回，交给解码器去失败。
pub fn preview_dimensions(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width, height);
    }
    let long = width.max(height);
    if long <= max_dim {
        return (width, height);
    }
    let scale = |short: u32| -> u32 {
        let scaled =
            (u64::from(short) * u64::from(max_dim) + u64::from(long) / 2) / u64::from(long);
        scaled.max(1) as u32
    };
    if width >= height {
        (max_dim, scale(height))
    } else {
        (scale(width), max_dim)
    }
}

/// 解码任意 `image` 认得的格式 → 等比缩放到长边 ≤ `max_dim` → PNG re-encode。
///
/// 解码失败（不是图片 / 截断）返回 `None`，由调用方转成一条错误。
pub fn make_preview_image(bytes: &[u8], max_dim: u32) -> Option<PreviewImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let (width, height) = preview_dimensions(img.width(), img.height(), max_dim);
    let resized = if (width, height) == (img.width(), img.height()) {
        img
    } else {
        img.resize_exact(width, height, image::imageops::FilterType::Triangle)
    };

    let mut out = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut out),
        resized.as_bytes(),
        resized.width(),
        resized.height(),
        resized.color().into(),
    )
    .ok()?;

    Some(PreviewImage {
        png: out,
        width,
        height,
    })
}

/// 按 id 组装预览数据。id 不存在 / 图片解码失败 → `Err`；其余一律 `Ok`。
///
/// `decryptor` 由命令层给出（真机是系统凭据库里的数据密钥），使这个函数能在
/// 任意平台上用 mock 密钥单测 —— 与 `pasteback::perform_paste` 的接缝同型。
pub async fn get_entry_preview_inner(
    storage: &Storage,
    id: i64,
    decryptor: &dyn Decryptor,
) -> Result<PreviewDto, String> {
    let entry = storage
        .get_by_id(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("entry {id} 不存在"))?;

    // 不可解密：连解密器都不碰（密钥缺失时碰它只会拿到一个没有信息量的错误，
    // 而「解不开」这件事 `is_decryptable` 已经说清楚了）。
    let locked = entry.is_vault && !entry.is_decryptable;

    match ContentKind::parse(&entry.content_kind) {
        ContentKind::Image => {
            if locked {
                return Ok(PreviewDto::undecryptable(ContentKind::Image));
            }
            // 非 vault 条目 `decrypt_payload` 原样返回 payload —— 与
            // `pasteback::perform_paste` 传的是同一种入参，不必在这里先分一次叉。
            let raw: &[u8] = entry.payload.as_deref().unwrap_or_default();
            match crate::pasteback::decrypt_payload(
                &entry,
                raw,
                entry.payload_nonce.as_deref(),
                decryptor,
            ) {
                Ok(bytes) => {
                    let img = make_preview_image(&bytes, PREVIEW_MAX_DIM)
                        .ok_or_else(|| format!("entry {id} 的图片数据无法解码"))?;
                    Ok(PreviewDto::image(&img))
                }
                Err(_) => Ok(PreviewDto::undecryptable(ContentKind::Image)),
            }
        }
        ContentKind::Text => {
            if locked {
                return Ok(PreviewDto::undecryptable(ContentKind::Text));
            }
            // 文本密文的 nonce 在 `nonce` 列（`payload_nonce` 只保护 binary payload），
            // 与 `commands::paste_entry` 取的是同一份。明文条目不必去读它。
            let nonce = if entry.is_vault {
                storage
                    .get_vault_nonce(id)
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                None
            };
            match crate::pasteback::decrypt_content(&entry, nonce.as_deref(), decryptor) {
                Ok(text) => Ok(PreviewDto::text(text)),
                Err(_) => Ok(PreviewDto::undecryptable(ContentKind::Text)),
            }
        }
        _ => Ok(PreviewDto::none()),
    }
}

/// 非 macOS / Windows 平台的解密器：任何解密尝试都失败。
///
/// 生产代码在这两个平台之外不会走到加密区（`move_to_vault` 与 `paste_entry`
/// 同样按 `#[cfg]` 降级），但 `get_entry_preview_inner` 的签名要求一个具体实现。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct NoDecryptor;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Decryptor for NoDecryptor {
    fn decrypt(
        &self,
        _stored_content: &str,
        _nonce: &[u8],
    ) -> Result<String, clipvault_core::crypto::CryptoError> {
        Err(clipvault_core::crypto::CryptoError::KeyUnavailable(
            "当前平台没有系统凭据库".to_string(),
        ))
    }

    fn data_key(
        &self,
    ) -> Result<[u8; clipvault_core::crypto::KEY_LEN], clipvault_core::crypto::CryptoError> {
        Err(clipvault_core::crypto::CryptoError::KeyUnavailable(
            "当前平台没有系统凭据库".to_string(),
        ))
    }
}
