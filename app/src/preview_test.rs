//! 预览栏后端的回归护栏（列表旁预览栏 plan U1 / R5 / R6 / R7）。
//!
//! 钉住四件事：
//! 1. **缩放算术**：长边恰好 ≤ 1600 且保持长宽比，原图更小则原样返回。
//! 2. **图片预览**：非加密区的图直接给 `data:image/png;base64,…`，解出来是 PNG。
//! 3. **加密区**：可解密时文本 / 图片都能还原；`is_decryptable = false` 时**只**回
//!    `undecryptable: true`，绝不下发明文 —— 这是本条最容易悄悄写坏的地方。
//! 4. **错误语义**：id 不存在 → `Err`；file / rtf / color → `kind = "none"`。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use clipvault_core::crypto::{CryptoError, KEY_LEN, KeyProvider};

use crate::crypto::{Decryptor, Encryptor, VaultCipher};
use crate::preview::{
    PREVIEW_MAX_DIM, get_entry_preview_inner, make_preview_image, preview_dimensions,
};
use crate::storage::Storage;
use crate::storage::test_helpers::open_test_db;

/// 固定 key 的 mock provider：加密 / 解密都走真实 AES-GCM，只是 key 不来自 Keychain。
struct MockKeyProvider {
    key: [u8; KEY_LEN],
}

impl KeyProvider for MockKeyProvider {
    fn data_key(&self) -> Result<[u8; KEY_LEN], CryptoError> {
        Ok(self.key)
    }
}

fn mock_cipher() -> VaultCipher<MockKeyProvider> {
    VaultCipher::new(MockKeyProvider {
        key: [7u8; KEY_LEN],
    })
}

/// 另一把 key 的「解密器」：模拟换了机器 / 密钥被重建，密文解不开。
fn wrong_key_cipher() -> VaultCipher<MockKeyProvider> {
    VaultCipher::new(MockKeyProvider {
        key: [9u8; KEY_LEN],
    })
}

/// `w × h` 的实心色块 PNG。
fn png_of(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([120, 80, 200]));
    let mut out = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut out),
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();
    out
}

/// 把 `data:image/png;base64,...` 还原成字节。
fn decode_data_url(url: &str) -> Vec<u8> {
    let b64 = url
        .strip_prefix("data:image/png;base64,")
        .expect("images must come back as a PNG data URL");
    STANDARD.decode(b64).expect("valid base64")
}

/// 断言一张 data URL 解出来的尺寸。
fn assert_png_size(url: &str, w: u32, h: u32) {
    let bytes = decode_data_url(url);
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "必须是 PNG");
    let img = image::load_from_memory(&bytes).expect("PNG must decode");
    assert_eq!((img.width(), img.height()), (w, h));
}

// ----------------------------------------------------------------
// 纯算术：preview_dimensions
// ----------------------------------------------------------------

#[test]
fn preview_dimensions_keeps_small_images_untouched() {
    assert_eq!(preview_dimensions(800, 600, 1600), (800, 600));
    // 长边正好等于上限：不动（缩放只会白花一次重编码）。
    assert_eq!(preview_dimensions(1600, 900, 1600), (1600, 900));
}

#[test]
fn preview_dimensions_pins_long_side_to_the_cap() {
    // 横图：宽钉成 1600，高按比例（1600/3200 * 1600 = 800）。
    assert_eq!(preview_dimensions(3200, 1600, 1600), (1600, 800));
    // 竖图：高钉成 1600，宽按比例（800/2400 * 1600 = 533.3 → 533）。
    assert_eq!(preview_dimensions(800, 2400, 1600), (533, 1600));
    // 极端长条：短边不能被四舍五入成 0（0 宽的 PNG 编不出来）。
    assert_eq!(preview_dimensions(10_000, 3, 1600), (1600, 1));
}

#[test]
fn preview_dimensions_handles_degenerate_input() {
    assert_eq!(preview_dimensions(0, 0, 1600), (0, 0));
    assert_eq!(preview_dimensions(0, 500, 1600), (0, 500));
}

// ----------------------------------------------------------------
// 缩放 + 重编码：make_preview_image
// ----------------------------------------------------------------

#[test]
fn make_preview_image_scales_oversized_to_cap() {
    let png = png_of(3200, 1600);
    let out = make_preview_image(&png, PREVIEW_MAX_DIM).expect("必须能缩放");
    assert_eq!((out.width, out.height), (1600, 800));
    // re-encode 出来的字节必须还是有效 PNG，且尺寸就是声明的尺寸。
    let img = image::load_from_memory(&out.png).expect("输出必须是 PNG");
    assert_eq!((img.width(), img.height()), (1600, 800));
}

#[test]
fn make_preview_image_keeps_aspect_ratio_of_tall_image() {
    let png = png_of(900, 1800);
    let out = make_preview_image(&png, PREVIEW_MAX_DIM).expect("必须能缩放");
    assert_eq!(out.height, PREVIEW_MAX_DIM);
    assert!(out.width < out.height);
    // 比例误差不超过 1px（四舍五入）。
    let expected = 900.0 * 1600.0 / 1800.0;
    assert!((f64::from(out.width) - expected).abs() <= 1.0);
}

#[test]
fn make_preview_image_does_not_upscale_small_images() {
    let png = png_of(64, 32);
    let out = make_preview_image(&png, PREVIEW_MAX_DIM).expect("必须能编码");
    assert_eq!((out.width, out.height), (64, 32));
}

#[test]
fn make_preview_image_returns_none_for_non_image_bytes() {
    assert!(make_preview_image(b"not an image at all", PREVIEW_MAX_DIM).is_none());
    assert!(make_preview_image(&[], PREVIEW_MAX_DIM).is_none());
}

// ----------------------------------------------------------------
// 明文条目
// ----------------------------------------------------------------

#[tokio::test]
async fn image_entry_preview_is_a_png_data_url_with_capped_dims() {
    let db = open_test_db();
    let id = db
        .insert_entry_binary(
            "img-1",
            "image",
            Some("image/png"),
            &png_of(3200, 1600),
            None,
            Some(r#"{"width":3200,"height":1600}"#),
        )
        .await
        .unwrap();

    let dto = get_entry_preview_inner(&db, id, &mock_cipher())
        .await
        .expect("预览应成功");
    assert_eq!(dto.kind, "image");
    assert_eq!(dto.mime.as_deref(), Some("image/png"));
    assert!(!dto.undecryptable);
    assert!(dto.content.is_none());
    // 缩到 1600 长边，比例不变。
    assert_eq!((dto.width, dto.height), (Some(1600), Some(800)));
    assert_png_size(dto.data_url.as_deref().unwrap(), 1600, 800);
}

#[tokio::test]
async fn text_entry_preview_returns_full_content_without_ipc_roundtrip() {
    let db = open_test_db();
    // 长文本：列表里只能看到前 120 字符，预览要是完整的。
    let long = "行".repeat(500);
    let id = db.insert_entry("txt-1", &long, "plain").await.unwrap();

    let dto = get_entry_preview_inner(&db, id, &mock_cipher())
        .await
        .unwrap();
    assert_eq!(dto.kind, "text");
    assert_eq!(dto.content.as_deref(), Some(long.as_str()));
    assert!(dto.data_url.is_none());
    assert!(!dto.undecryptable);
}

#[tokio::test]
async fn color_entry_preview_is_none() {
    let db = open_test_db();
    let id = db
        .insert_entry_binary(
            "color-1",
            "color",
            None,
            b"",
            None,
            Some(r#"{"rgba":"rgba(1,2,3,1)"}"#),
        )
        .await
        .unwrap();

    let dto = get_entry_preview_inner(&db, id, &mock_cipher())
        .await
        .unwrap();
    assert_eq!(dto.kind, "none");
    assert!(dto.data_url.is_none() && dto.content.is_none());
    assert!(!dto.undecryptable);
}

#[tokio::test]
async fn missing_id_is_an_error_not_a_none_kind() {
    let db = open_test_db();
    let err = get_entry_preview_inner(&db, 4242, &mock_cipher())
        .await
        .expect_err("不存在的 id 必须报错");
    assert!(err.contains("4242"), "错误里要能看见是哪个 id：{err}");
}

// ----------------------------------------------------------------
// 加密区条目（R7 / AE3）
// ----------------------------------------------------------------

#[tokio::test]
async fn vault_text_preview_decrypts_with_the_nonce_column() {
    let db = open_test_db();
    let cipher = mock_cipher();
    let id = db
        .insert_entry("vault-txt", "秘密内容 secret", "plain")
        .await
        .unwrap();
    db.move_to_vault(id, &cipher).await.expect("移入加密区");

    let dto = get_entry_preview_inner(&db, id, &cipher).await.unwrap();
    assert_eq!(dto.kind, "text");
    assert_eq!(dto.content.as_deref(), Some("秘密内容 secret"));
    assert!(!dto.undecryptable);
}

#[tokio::test]
async fn vault_image_preview_decrypts_the_payload_blob() {
    let db = open_test_db();
    let cipher = mock_cipher();
    let id = db
        .insert_entry_binary(
            "vault-img",
            "image",
            Some("image/png"),
            &png_of(120, 60),
            None,
            None,
        )
        .await
        .unwrap();
    db.move_to_vault(id, &cipher).await.expect("移入加密区");

    let dto = get_entry_preview_inner(&db, id, &cipher).await.unwrap();
    assert_eq!(dto.kind, "image");
    assert!(!dto.undecryptable);
    assert_png_size(dto.data_url.as_deref().unwrap(), 120, 60);
}

#[tokio::test]
async fn undecryptable_image_returns_no_content_at_all() {
    let db = open_test_db();
    let cipher = mock_cipher();
    let id = db
        .insert_entry_binary(
            "vault-img-locked",
            "image",
            Some("image/png"),
            &png_of(80, 40),
            None,
            None,
        )
        .await
        .unwrap();
    db.move_to_vault(id, &cipher).await.unwrap();
    // 模拟「换机器 / 密钥重建」：启动校验会把 is_decryptable 置 0。
    db.with_conn(move |conn| {
        conn.execute("UPDATE entries SET is_decryptable = 0 WHERE id = ?1", [id])?;
        Ok(())
    })
    .await
    .unwrap();

    let dto = get_entry_preview_inner(&db, id, &cipher).await.unwrap();
    assert!(dto.undecryptable, "必须报「无法解密」");
    assert_eq!(dto.kind, "image");
    assert!(dto.data_url.is_none(), "不可解密时绝不下发图片字节");
    assert!(dto.content.is_none());
}

#[tokio::test]
async fn vault_text_with_wrong_key_reports_undecryptable() {
    // is_decryptable 还是 1（启动校验没跑过），但密钥其实是另一把：
    // 这条守的是「解密失败也要走 undecryptable，而不是把错误抛给前端当 toast」。
    let db = open_test_db();
    let cipher = mock_cipher();
    let id = db
        .insert_entry("vault-txt-2", "另一把 key 解不开", "plain")
        .await
        .unwrap();
    db.move_to_vault(id, &cipher).await.unwrap();

    let dto = get_entry_preview_inner(&db, id, &wrong_key_cipher())
        .await
        .unwrap();
    assert!(dto.undecryptable);
    assert!(dto.content.is_none(), "解密失败不得回明文");
}

/// 明文条目的预览不该碰解密器 —— 碰了就说明分派写反了（并把明文当密文送进 Keychain）。
#[tokio::test]
async fn plain_entries_never_touch_the_decryptor() {
    let db = open_test_db();
    let id = db.insert_entry("plain-1", "明文", "plain").await.unwrap();

    let dto = get_entry_preview_inner(&db, id, &PanickingDecryptor)
        .await
        .unwrap();
    assert_eq!(dto.content.as_deref(), Some("明文"));
}

struct PanickingDecryptor;

impl Decryptor for PanickingDecryptor {
    fn decrypt(&self, _stored_content: &str, _nonce: &[u8]) -> Result<String, CryptoError> {
        panic!("明文条目不该调用解密器");
    }

    fn data_key(&self) -> Result<[u8; KEY_LEN], CryptoError> {
        panic!("明文条目不该查询密钥");
    }
}

/// `mock_cipher` 同时是 `Encryptor`：这里显式用一次。
#[tokio::test]
async fn mock_cipher_roundtrips_through_the_storage_columns() {
    let db = open_test_db();
    let cipher = mock_cipher();
    let id = db.insert_entry("rt", "可往返", "plain").await.unwrap();
    db.move_to_vault(id, &cipher).await.unwrap();

    let entry = db.get_by_id(id).await.unwrap().unwrap();
    assert_ne!(entry.content, "可往返", "落盘必须是密文");
    let nonce = db.get_vault_nonce(id).await.unwrap().unwrap();
    assert_eq!(cipher.decrypt(&entry.content, &nonce).unwrap(), "可往返");
}

/// `Encryptor` 的默认 `encrypt_bytes` 与 `decrypt_bytes` 互逆（图片 payload 走这条）。
#[test]
fn encrypt_bytes_roundtrips() {
    let cipher = mock_cipher();
    let enc = cipher.encrypt_bytes(b"png-ish bytes").unwrap();
    assert_ne!(enc.ciphertext, b"png-ish bytes");
    // 用 `crate::storage` 之外的路径解一次：`Decryptor` 的默认实现。
    let dec = crate::pasteback::decrypt_payload(
        &plain_entry_stub(),
        &enc.ciphertext,
        Some(&enc.nonce),
        &cipher,
    )
    .unwrap();
    assert_eq!(dec, b"png-ish bytes");
}

/// 构造一条 `is_vault = true` 的最小条目，只为驱动 `decrypt_payload` 的 vault 分支。
fn plain_entry_stub() -> clipvault_core::Entry {
    clipvault_core::Entry {
        id: 1,
        content_hash: String::new(),
        content: String::new(),
        note: String::new(),
        auto_category: "image".to_string(),
        is_pinned: false,
        pinned_at: None,
        is_vault: true,
        is_decryptable: true,
        created_at: 0,
        content_kind: "image".to_string(),
        mime_type: Some("image/png".to_string()),
        payload: None,
        payload_nonce: None,
        metadata_json: None,
    }
}

#[test]
fn test_db_is_in_memory() {
    let db: Storage = open_test_db();
    // 空库：没有任何条目 → 预览命令对任何 id 都报不存在。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(get_entry_preview_inner(&db, 1, &mock_cipher()))
        .expect_err("空库不该有 id=1");
    assert!(err.contains("不存在"), "实际错误：{err}");
}
