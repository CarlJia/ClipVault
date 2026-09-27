//! 图片缩略图生成（U3 / R19 / R24）。
//!
//! 用 [`image`] crate 解码 PNG / JPEG / TIFF 原字节、降采样到 64×64（fallback
//! 48×48）、再编码为 PNG、最后 base64。产物写入 `metadata_json.thumb`。
//!
//! 10KB base64 上限（R24）：超限时降级到 48×48 重试；仍超则放弃 thumb
//! 字段（保留 width/height/size_bytes，UI 降级到文件大小文本）。
//!
//! ## 跨平台
//!
//! `image` 是纯 Rust 实现，不依赖 ImageIO/CGImageSource，跨平台一致。
//! macOS 真实 ImageIO 路径（CGImageSourceCreateThumbnailAtIndex）留作后续
//! 增强——MVP U3 走 `image` 已能保证 ≤ 10KB 输出在常规 1024×768 PNG 上。

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// 缩略图目标尺寸（主路径）。R19 锁定为 64×64。
const PRIMARY_DIM: u32 = 64;
/// 缩略图降级尺寸（base64 仍 > 10KB 时使用）。R24 锁定为 48×48。
const FALLBACK_DIM: u32 = 48;
/// 缩略图 base64 上限（字节数）。R24 锁定为 10KB。
const THUMB_B64_BUDGET: usize = 10 * 1024;

/// 缩略图生成结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// base64 编码的 PNG 字节。
    pub b64: String,
    /// 实际边长（PRIMARY_DIM 或 FALLBACK_DIM）。
    pub dim: u32,
}

/// 构造 64×64 PNG 缩略图；超 10KB base64 时降级到 48×48；仍超则返
/// 回 `None`（调用方丢弃 `thumb` 字段）。
///
/// `bytes` 是图片原始字节（PNG/JPEG 等，由 [`image`] 探测格式）。
/// `mime` 仅用于日志诊断；不影响处理。
pub fn make_thumbnail(bytes: &[u8], _mime: &str) -> Option<Thumbnail> {
    let img = image::load_from_memory(bytes).ok()?;
    let primary = encode_thumbnail(&img, PRIMARY_DIM);
    if let Some(b64) = primary {
        if b64.len() <= THUMB_B64_BUDGET {
            return Some(Thumbnail { b64, dim: PRIMARY_DIM });
        }
    }
    let fallback = encode_thumbnail(&img, FALLBACK_DIM);
    if let Some(b64) = fallback {
        if b64.len() <= THUMB_B64_BUDGET {
            return Some(Thumbnail { b64, dim: FALLBACK_DIM });
        }
    }
    None
}

/// 把 [`image::DynamicImage`] 缩放到目标边长，编码 PNG，返回 base64。
fn encode_thumbnail(img: &image::DynamicImage, dim: u32) -> Option<String> {
    let resized = img.resize_exact(dim, dim, image::imageops::FilterType::Triangle);
    let mut out = Vec::with_capacity(8 * 1024);
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    image::ImageEncoder::write_image(
        encoder,
        resized.as_bytes(),
        resized.width(),
        resized.height(),
        resized.color().into(),
    )
    .ok()?;
    Some(STANDARD.encode(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 透明 PNG（8 字节 signature + IHDR chunk）。
    const TINY_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";

    /// 最小合法 PNG：1×1 RGB（IHDR + IDAT + IEND）。
    fn minimal_valid_png() -> Vec<u8> {
        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        // 1×1 white pixel
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 255, 255]));
        image::ImageEncoder::write_image(
            encoder,
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
        buf
    }

    #[test]
    fn makes_64_dim_thumbnail_for_small_png() {
        let png = minimal_valid_png();
        let t = make_thumbnail(&png, "image/png").expect("should produce a thumbnail");
        assert_eq!(t.dim, 64);
        assert!(t.b64.len() <= THUMB_B64_BUDGET, "got {} bytes", t.b64.len());
        // base64 解码后是有效 PNG（PNG signature 起头）
        let bytes = STANDARD.decode(&t.b64).expect("valid base64");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn returns_none_for_invalid_bytes() {
        assert!(make_thumbnail(b"not an image", "image/png").is_none());
        assert!(make_thumbnail(&[], "image/png").is_none());
    }

    #[test]
    fn returns_none_for_truncated_png_signature() {
        // 仅 PNG signature,无 IHDR/IDAT,解码失败。
        assert!(make_thumbnail(TINY_PNG, "image/png").is_none());
    }

    #[test]
    fn thumbnail_dimensions_match_const() {
        assert_eq!(PRIMARY_DIM, 64, "R19 锁定 64×64");
        assert_eq!(FALLBACK_DIM, 48, "R24 降级到 48×48");
        assert_eq!(THUMB_B64_BUDGET, 10 * 1024, "R24 锁定 10KB");
    }

    #[test]
    fn encode_thumbnail_round_trips() {
        // 24×24 RGB image should produce non-empty PNG output.
        let img = image::RgbImage::from_pixel(24, 24, image::Rgb([100, 150, 200]));
        let b64 = encode_thumbnail(&img.into(), 64).expect("encode should succeed");
        assert!(b64.len() > 0);
        // PNG 解码回来应当等于 64×64。
        let bytes = STANDARD.decode(&b64).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 64);
    }
}