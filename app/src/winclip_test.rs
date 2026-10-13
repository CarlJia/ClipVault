//! `winclip` 的 DIB 解析与路径前缀转换单测(U1)。
//!
//! 这些测试用**合成的 DIB 字节**驱动,不依赖 Windows 也不依赖真实剪贴板 —— 这正是把
//! 纯转换逻辑单独拆成 `winclip` 模块的目的:Windows 侧的 FFI 只能在真机验收,而这里
//! 的解析边界(位深、行对齐、自下而上、alpha 语义、畸形输入不 panic)可以在任意平台
//! 由 CI 覆盖。
//!
//! 合成 DIB 的构造规则:`CF_DIB` 字节流 = `BITMAPINFOHEADER`(40 字节)或
//! `BITMAPV4HEADER`(108 字节)+ 可选调色板 + 位数据;位数据每行按 4 字节对齐,
//! 默认自下而上(`biHeight > 0`),`biHeight < 0` 表示自上而下。

use super::winclip::{
    BI_BITFIELDS, BI_RGB, build_dropfiles, dib_to_png, dib_to_rgba, image_to_dib,
    strip_verbatim_prefix, utf16_nul,
};

/// 构造 40 字节 `BITMAPINFOHEADER`。
fn header(width: i32, height: i32, bit_count: u16, compression: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(40);
    h.extend(40u32.to_le_bytes()); // biSize
    h.extend(width.to_le_bytes()); // biWidth
    h.extend(height.to_le_bytes()); // biHeight
    h.extend(1u16.to_le_bytes()); // biPlanes
    h.extend(bit_count.to_le_bytes()); // biBitCount
    h.extend(compression.to_le_bytes()); // biCompression
    h.extend(0u32.to_le_bytes()); // biSizeImage
    h.extend(0u32.to_le_bytes()); // biXPelsPerMeter
    h.extend(0u32.to_le_bytes()); // biYPelsPerMeter
    h.extend(0u32.to_le_bytes()); // biClrUsed
    h.extend(0u32.to_le_bytes()); // biClrImportant
    h
}

/// 构造 108 字节 `BITMAPV4HEADER`,带显式通道掩码(含 alpha)。
fn header_v4(width: i32, height: i32, alpha_mask: u32) -> Vec<u8> {
    let mut h = header(width, height, 32, BI_RGB);
    h[0..4].copy_from_slice(&108u32.to_le_bytes()); // bV4Size
    h.extend(0x00ff_0000u32.to_le_bytes()); // bV4RedMask
    h.extend(0x0000_ff00u32.to_le_bytes()); // bV4GreenMask
    h.extend(0x0000_00ffu32.to_le_bytes()); // bV4BlueMask
    h.extend(alpha_mask.to_le_bytes()); // bV4AlphaMask
    h.resize(108, 0); // 其余字段(CSType / 端点 / gamma)与本解析无关
    h
}

/// 每行字节数(含 4 字节对齐填充)。
fn stride(bit_count: u16, width: usize) -> usize {
    (bit_count as usize * width).div_ceil(32) * 4
}

#[test]
fn dib_24bpp_bottom_up_decodes_bgr_to_rgba() {
    // 2×2,自上而下应为:红、绿 / 蓝、白。CF_DIB 默认自下而上存储,故字节里
    // 先出现的(蓝、白)才是图像的下一行。
    let rows: [[u8; 8]; 2] = [
        [255, 0, 0, 255, 255, 255, 0, 0], // 蓝、白 + 2 字节对齐填充
        [0, 0, 255, 0, 255, 0, 0, 0],     // 红、绿 + 填充
    ];
    let mut dib = header(2, 2, 24, BI_RGB);
    dib.extend(rows[0]);
    dib.extend(rows[1]);

    let (rgba, w, h) = dib_to_rgba(&dib).expect("24bpp 应可解码");
    assert_eq!((w, h), (2, 2));
    assert_eq!(
        rgba,
        vec![
            255, 0, 0, 255, // 左上:红
            0, 255, 0, 255, // 右上:绿
            0, 0, 255, 255, // 左下:蓝
            255, 255, 255, 255, // 右下:白
        ]
    );
}

#[test]
fn dib_24bpp_row_padding_is_not_read_as_pixels() {
    // 宽 1 的 24bpp 行是 3 字节,必须补齐到 4 字节。若把填充当像素读,第二行会整体偏移。
    let mut dib = header(1, 2, 24, BI_RGB);
    dib.extend([255, 0, 0, 0xaa]); // 下行:蓝(末尾是填充)
    dib.extend([0, 0, 255, 0xbb]); // 上行:红

    let (rgba, w, h) = dib_to_rgba(&dib).expect("宽 1 的 24bpp 应可解码");
    assert_eq!((w, h), (1, 2));
    assert_eq!(rgba, vec![255, 0, 0, 255, 0, 0, 255, 255]);
}

#[test]
fn dib_32bpp_bgrx_is_treated_as_opaque() {
    // BI_RGB 的 32bpp 没有 alpha 通道,应用常把高字节留成 0。若照抄成 alpha,
    // 整张图会变成全透明。
    let mut dib = header(2, 1, 32, BI_RGB);
    dib.extend([0, 0, 255, 0, 0, 255, 0, 0]); // 红、绿,两像素的高字节都是 0

    let (rgba, _, _) = dib_to_rgba(&dib).expect("32bpp BI_RGB 应可解码");
    assert_eq!(rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
}

#[test]
fn dib_v4_alpha_mask_is_honored() {
    let mut dib = header_v4(2, 1, 0xff00_0000);
    dib.extend([0x00, 0x00, 0xff, 0x80]); // 红,alpha = 0x80
    dib.extend([0x00, 0xff, 0x00, 0xff]); // 绿,alpha = 0xff

    let (rgba, w, h) = dib_to_rgba(&dib).expect("V4 带 alpha 掩码应可解码");
    assert_eq!((w, h), (2, 1));
    assert_eq!(rgba, vec![255, 0, 0, 0x80, 0, 255, 0, 0xff]);
}

#[test]
fn dib_negative_height_means_top_down() {
    // biHeight = -2:首行就是图像顶行,不做翻转。
    let mut dib = header(1, -2, 24, BI_RGB);
    dib.extend([0, 0, 255, 0]); // 顶行:红
    dib.extend([255, 0, 0, 0]); // 底行:蓝

    let (rgba, _, _) = dib_to_rgba(&dib).expect("自上而下 DIB 应可解码");
    assert_eq!(rgba, vec![255, 0, 0, 255, 0, 0, 255, 255]);
}

#[test]
fn dib_8bpp_uses_palette_in_rgbquad_order() {
    // RGBQUAD 的字节顺序是 B、G、R、保留;取错会导致红蓝互换。
    let mut dib = header(2, 1, 8, BI_RGB);
    dib[32..36].copy_from_slice(&2u32.to_le_bytes()); // biClrUsed = 2
    dib.extend([0, 0, 255, 0]); // 索引 0:红
    dib.extend([0, 255, 0, 0]); // 索引 1:绿
    dib.extend([1, 0, 0, 0]); // 像素:索引 1、0 → 绿、红

    let (rgba, w, h) = dib_to_rgba(&dib).expect("8bpp 调色板应可解码");
    assert_eq!((w, h), (2, 1));
    assert_eq!(rgba, vec![0, 255, 0, 255, 255, 0, 0, 255]);
}

#[test]
fn dib_40byte_header_bitfields_masks_follow_header() {
    // 40 字节头 + BI_BITFIELDS:3 个掩码 DWORD 紧跟头之后,位数据从第 52 字节开始。
    let mut dib = header(1, 1, 32, BI_BITFIELDS);
    dib.extend(0x00ff_0000u32.to_le_bytes()); // 红掩码
    dib.extend(0x0000_ff00u32.to_le_bytes()); // 绿掩码
    dib.extend(0x0000_00ffu32.to_le_bytes()); // 蓝掩码
    dib.extend([0x33, 0x22, 0x11, 0xff]); // B=0x33 G=0x22 R=0x11

    let (rgba, _, _) = dib_to_rgba(&dib).expect("40 字节头 + 掩码应可解码");
    assert_eq!(rgba, vec![0x11, 0x22, 0x33, 255]);
}

#[test]
fn dib_to_png_emits_png_with_same_dimensions() {
    let mut dib = header(2, 3, 24, BI_RGB);
    dib.extend(vec![0u8; stride(24, 2) * 3]);

    let (png, w, h) = dib_to_png(&dib).expect("应能转出 PNG");
    assert_eq!((w, h), (2, 3));
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // IHDR 紧跟 8 字节签名 + 4 字节长度 + 4 字节类型,宽高是大端 u32。
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 2);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 3);
}

#[test]
fn dib_unsupported_bit_count_is_rejected() {
    // 16bpp(RGB555/565)不在支持范围内:退回上一种剪贴板格式,而不是猜错颜色。
    let mut dib = header(2, 2, 16, BI_RGB);
    dib.extend(vec![0u8; 8]);
    assert!(dib_to_rgba(&dib).is_none());
}

#[test]
fn dib_compressed_payload_is_rejected() {
    // RLE8(compression = 1)需要真正的解码器,不能当未压缩数据读。
    let mut dib = header(2, 2, 8, 1);
    dib.extend(vec![0u8; 16]);
    assert!(dib_to_rgba(&dib).is_none());
}

#[test]
fn dib_truncated_pixel_data_is_rejected() {
    let mut dib = header(4, 4, 24, BI_RGB);
    dib.extend(vec![0u8; 8]); // 远少于 4 行 × 12 字节
    assert!(dib_to_rgba(&dib).is_none());
}

#[test]
fn dib_missing_header_tail_is_rejected() {
    // 声称 biSize = 40,但实际只有 20 字节:必须判不可用而不是越界读。
    let mut dib = header(2, 2, 24, BI_RGB);
    dib.truncate(20);
    assert!(dib_to_rgba(&dib).is_none());
}

#[test]
fn dib_core_header_is_rejected() {
    // 12 字节 BITMAPCOREHEADER(OS/2 1.x):字段偏移与含义都不同,不支持。
    let mut dib = vec![12u8, 0, 0, 0];
    dib.extend(vec![0u8; 40]);
    assert!(dib_to_rgba(&dib).is_none());
}

#[test]
fn dib_oversized_dimensions_are_rejected() {
    // 单维越界。
    let mut tall = header(64, 70_000, 24, BI_RGB);
    tall.extend(vec![0u8; 64]);
    assert!(dib_to_rgba(&tall).is_none());

    // 像素总数越界(6000×6000 = 3600 万 > 2500 万上限)。
    let mut big = header(6000, 6000, 24, BI_RGB);
    big.extend(vec![0u8; 64]);
    assert!(dib_to_rgba(&big).is_none());
}

#[test]
fn dib_non_positive_dimensions_are_rejected() {
    assert!(dib_to_rgba(&header(0, 2, 24, BI_RGB)).is_none());
    assert!(dib_to_rgba(&header(-1, 2, 24, BI_RGB)).is_none());
    assert!(dib_to_rgba(&header(2, 0, 24, BI_RGB)).is_none());
}

#[test]
fn dib_palette_count_beyond_full_size_is_rejected() {
    // biClrUsed 声称 300 项,但 8bpp 满编只有 256:头字段不可信,整条判不可用。
    let mut dib = header(2, 1, 8, BI_RGB);
    dib[32..36].copy_from_slice(&300u32.to_le_bytes());
    dib.extend(vec![0u8; 300 * 4]);
    dib.extend(vec![0u8; 4]);
    assert!(dib_to_rgba(&dib).is_none());
}

#[test]
fn strip_verbatim_prefix_unwraps_local_and_unc_paths() {
    assert_eq!(
        strip_verbatim_prefix(r"\\?\C:\Users\me\a.txt"),
        r"C:\Users\me\a.txt"
    );
    assert_eq!(
        strip_verbatim_prefix(r"\\?\UNC\server\share\a.txt"),
        r"\\server\share\a.txt"
    );
    // 没有前缀的路径原样返回,不引入额外变化。
    assert_eq!(
        strip_verbatim_prefix(r"C:\Users\me\a.txt"),
        r"C:\Users\me\a.txt"
    );
    assert_eq!(strip_verbatim_prefix("/Users/me/a.txt"), "/Users/me/a.txt");
}

// ---------------- 写侧:PNG → CF_DIB(U3) ----------------

/// 造一张不透明的 PNG,像素按给定顺序(行优先,从上到下)。
fn make_png(width: u32, height: u32, pixels: &[[u8; 3]]) -> Vec<u8> {
    let mut buf = Vec::new();
    let img = image::RgbImage::from_fn(width, height, |x, y| {
        let p = pixels[(y * width + x) as usize];
        image::Rgb([p[0], p[1], p[2]])
    });
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("编码 PNG");
    buf
}

#[test]
fn image_to_dib_round_trips_through_the_reader() {
    // 拿自己的解析器当裁判:写出去的 DIB 必须能被读回来,且像素与方向都对。
    // 这条测试同时覆盖「写侧自下而上」与「读侧翻转」两半 —— 两边都错会互相抵消,
    // 所以下面另有一条直接检查字节序的测试来钉住方向。
    let png = make_png(
        2,
        2,
        &[[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]],
    );
    let dib = image_to_dib(&png).expect("应能转成 DIB");

    let (rgba, w, h) = dib_to_rgba(&dib).expect("自己写的 DIB 自己应能读回");
    assert_eq!((w, h), (2, 2));
    assert_eq!(
        rgba,
        vec![
            255, 0, 0, 255, // 左上:红
            0, 255, 0, 255, // 右上:绿
            0, 0, 255, 255, // 左下:蓝
            255, 255, 255, 255, // 右下:白
        ]
    );
}

#[test]
fn image_to_dib_stores_rows_bottom_up_in_bgra() {
    // 直接看字节:第一行数据必须是图像的**底**行(BI_RGB 的约定),通道序是 BGRA。
    let png = make_png(1, 2, &[[10, 20, 30], [40, 50, 60]]);
    let dib = image_to_dib(&png).expect("应能转成 DIB");

    assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40); // biSize
    assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2); // biHeight 为正 = 自下而上
    // 头之后先是底行(40,50,60)→ BGR = 60,50,40;再是顶行(10,20,30)→ 30,20,10。
    assert_eq!(dib[40..48], [60, 50, 40, 0xff, 30, 20, 10, 0xff]);
}

#[test]
fn image_to_dib_writes_opaque_alpha() {
    // 半透明像素:BI_RGB 没有 alpha 通道,第 4 字节必须写成不透明,否则按 alpha
    // 解释它的传统应用会把整张图渲染成全透明。
    let mut png = Vec::new();
    let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 0x40]));
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();

    let dib = image_to_dib(&png).expect("应能转成 DIB");
    assert_eq!(dib[40..44], [3, 2, 1, 0xff]);
}

#[test]
fn image_to_dib_rejects_non_image_bytes() {
    assert!(image_to_dib(b"not a png at all").is_none());
    assert!(image_to_dib(&[]).is_none());
}

// ---------------- 写侧:CF_HDROP(U3) ----------------

#[test]
fn build_dropfiles_lays_out_header_and_wide_paths() {
    let paths = vec![r"C:\a b\一.txt".to_string(), r"D:\two.pdf".to_string()];
    let blob = build_dropfiles(&paths);

    // 20 字节头:偏移 20 / 落点 0,0 / fNC = FALSE / fWide = TRUE。
    assert_eq!(u32::from_le_bytes(blob[0..4].try_into().unwrap()), 20);
    assert_eq!(u32::from_le_bytes(blob[4..8].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(blob[8..12].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(blob[12..16].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(blob[16..20].try_into().unwrap()), 1);

    // 路径表:每条各自 NUL 结尾,整表再补一个 NUL。
    let mut expected: Vec<u16> = Vec::new();
    for p in &paths {
        expected.extend(p.encode_utf16());
        expected.push(0);
    }
    expected.push(0);
    let expected_bytes: Vec<u8> = expected.iter().flat_map(|u| u.to_le_bytes()).collect();
    assert_eq!(&blob[20..], expected_bytes.as_slice());

    // 结尾是双 NUL(最后 4 字节 = 两个 UTF-16 的 0)。
    assert_eq!(&blob[blob.len() - 4..], [0, 0, 0, 0]);
}

#[test]
fn build_dropfiles_handles_empty_path_list() {
    let blob = build_dropfiles(&[]);
    assert_eq!(blob.len(), 22); // 20 字节头 + 一个收尾 NUL
    assert_eq!(&blob[20..], [0, 0]);
}

#[test]
fn utf16_nul_appends_single_terminator() {
    assert_eq!(utf16_nul("ab"), vec![0x61, 0x62, 0x00]);
    assert_eq!(utf16_nul(""), vec![0x00]);
    // 非 BMP 字符按 UTF-16 代理对编码(两个 u16)。
    assert_eq!(utf16_nul("😀"), vec![0xd83d, 0xde00, 0x00]);
}
