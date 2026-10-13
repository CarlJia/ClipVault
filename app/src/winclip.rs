//! Windows 剪贴板**字节格式**的纯转换逻辑(U1/U3)。
//!
//! 本模块**不含任何 Win32 调用**,只做字节层面的格式转换(DIB → RGBA/PNG、
//! 扩展长度路径前缀剥离),因此可以在**任意平台**编译并单测。真正的剪贴板读写分别在
//! `capture.rs` 的 `mod win` 与 `pasteback.rs` 的 `mod imp_win` 里 —— 那些是 FFI,
//! 只能在 Windows 上编译、也就只能在 Windows 真机上验收;而这里是能被 CI 的
//! macOS job 覆盖的部分。
//!
//! ## 为什么要单独一层
//!
//! Win32 剪贴板给到应用的图片是 `CF_DIB`(裸 `BITMAPINFOHEADER` + 位数据),不是任何
//! 能直接用的图片格式。而 ClipVault 的下游链路(缩略图 `thumbnail.rs`、前端预览、
//! `payload` BLOB)全部按 `image/png` 消费,所以采集侧必须把 DIB 转成 PNG。这段转换
//! 有大量易错细节(行 4 字节对齐、自下而上存储、`biHeight` 为负表示自上而下、32bpp 的
//! alpha 语义、掩码通道顺序),全部集中在这里并由 `winclip_test.rs` 用合成 DIB 逐项覆盖。
//!
//! ## 32bpp 的 alpha 语义(容易踩)
//!
//! `BI_RGB` 的 32bpp 按 Win32 文档「每个 DWORD 的高字节未被使用」,即**没有** alpha 通道。
//! 实践中大量应用会把这个字节留成 0,若照抄成 alpha 就会得到一张全透明的 PNG。因此
//! `BI_RGB` + 32bpp 一律按不透明处理;只有显式给出 alpha 掩码(`BI_BITFIELDS` 或
//! V4/V5 头里的 `bV4AlphaMask`)时才使用真实 alpha。
//!
//! ## 输入是不可信字节
//!
//! DIB 字节来自别的进程,头字段(位深、尺寸、偏移)都可能任意。这里的每条路径都以
//! `get`/`checked_*` 做越界与溢出检查,**任何一种畸形输入都只能导致 `None`,不得 panic**。

use std::io::Cursor;

use image::{ImageFormat, RgbaImage};

/// DIB 解码后的像素数上限。`CF_DIB` 是我们从剪贴板直接读到的字节,尺寸不受控;
/// 一张 7680×2160 的双 4K 截图约 1660 万像素,25M 留出余量,同时把最坏情况下的
/// RGBA 缓冲限制在 100MB 以内。超限判为不可用(采集侧会退到下一种格式)。
const MAX_DIB_PIXELS: u64 = 25_000_000;

/// 单个维度的上限(像素),挡住极端长条图造成的行/列算术溢出。
const MAX_DIB_DIMENSION: u32 = 65_535;

/// `BI_RGB`:未压缩,像素按颜色深度直接排布。
pub(crate) const BI_RGB: u32 = 0;
/// `BI_BITFIELDS`:通道掩码显式给出(40 字节头时掩码紧跟头之后)。
pub(crate) const BI_BITFIELDS: u32 = 3;

/// `BITMAPINFOHEADER` 的大小(读写两侧都只产出这一种头)。
const HEADER_40: u32 = 40;

/// 解析后的 DIB 头信息(已归一化为「首行 = 图像顶部」的语义)。
struct DibHeader {
    width: u32,
    height: u32,
    bit_count: u16,
    /// 归一化后的通道掩码。8bpp 走调色板,三者均为 0。
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
    /// `0` 表示该 DIB 没有 alpha 通道(按不透明处理)。
    alpha_mask: u32,
    /// 调色板起始偏移(仅 `bit_count <= 8` 时有意义)。
    palette_offset: usize,
    /// 像素数据在 DIB 字节流中的起始偏移。
    pixels_offset: usize,
    /// 每行字节数(已含 4 字节对齐填充)。
    stride: usize,
    /// 首行是否位于图像底部(`CF_DIB` 的传统排布是自下而上)。
    bottom_up: bool,
}

/// 把剪贴板里的 `CF_DIB` / `CF_DIBV5` 字节转成 PNG,返回 `(png_bytes, width, height)`。
///
/// 返回 `None` 表示该 DIB 不受支持(头不完整、位深/压缩方式不支持、尺寸越界、
/// 像素数据被截断)。调用方应把它当作「这种格式读不出来」而继续尝试下一种剪贴板
/// 格式,不要当成错误上报。
pub fn dib_to_png(dib: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, width, height) = dib_to_rgba(dib)?;
    let img = RgbaImage::from_raw(width, height, rgba)?;
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .ok()?;
    Some((png, width, height))
}

/// 把 `CF_DIB` / `CF_DIBV5` 字节解码为 RGBA8 像素,返回 `(rgba, width, height)`。
pub fn dib_to_rgba(dib: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let header = parse_dib_header(dib)?;
    let width = header.width as usize;
    let height = header.height as usize;

    // 调色板(仅 8bpp)。`parse_dib_header` 已保证位深是 8/24/32。
    let palette = if header.bit_count == 8 {
        build_palette(dib, &header)?
    } else {
        Vec::new()
    };

    let row_bytes_total = header.stride.checked_mul(height)?;
    let pixels = dib.get(header.pixels_offset..)?.get(..row_bytes_total)?;

    let mut rgba = vec![0u8; width.checked_mul(height)?.checked_mul(4)?];
    for row in 0..height {
        // DIB 常规是自下而上存储;`biHeight` 为负时才自上而下。
        let src_row = if header.bottom_up {
            height - 1 - row
        } else {
            row
        };
        let row_bytes = pixels.get(src_row * header.stride..(src_row + 1) * header.stride)?;
        let dst = rgba.get_mut(row * width * 4..(row + 1) * width * 4)?;

        for x in 0..width {
            let (r, g, b, a) = match header.bit_count {
                32 => {
                    let px = read_u32(row_bytes, x * 4)?;
                    (
                        scale_channel(header.red_mask, px),
                        scale_channel(header.green_mask, px),
                        scale_channel(header.blue_mask, px),
                        scale_alpha(header.alpha_mask, px),
                    )
                }
                24 => {
                    let off = x * 3;
                    let b = *row_bytes.get(off)?;
                    let g = *row_bytes.get(off + 1)?;
                    let r = *row_bytes.get(off + 2)?;
                    (r, g, b, 255)
                }
                8 => {
                    let idx = *row_bytes.get(x)? as usize;
                    let (r, g, b) = *palette.get(idx)?;
                    (r, g, b, 255)
                }
                _ => return None,
            };
            let off = x * 4;
            dst[off] = r;
            dst[off + 1] = g;
            dst[off + 2] = b;
            dst[off + 3] = a;
        }
    }
    Some((rgba, header.width, header.height))
}

/// 解析 DIB 头,统一处理 40 字节 `BITMAPINFOHEADER` 与 V4/V5 头(108/124 字节)。
fn parse_dib_header(dib: &[u8]) -> Option<DibHeader> {
    let header_size = read_u32(dib, 0)?;
    // 12 字节的 BITMAPCOREHEADER(OS/2 1.x)等更早的变体不支持:真实剪贴板数据里
    // 几乎不出现,且位宽字段的偏移与含义都不同,单独分支不划算。
    if header_size < 40 || dib.len() < header_size as usize {
        return None;
    }

    let width_raw = read_u32(dib, 4)? as i32;
    let height_raw = read_u32(dib, 8)? as i32;
    let planes = read_u16(dib, 12)?;
    let bit_count = read_u16(dib, 14)?;
    let compression = read_u32(dib, 16)?;

    // 先校验位深与压缩方式,再碰任何依赖它们的位移/取模运算。
    // 顺序在这里是有意的:`1u32 << bit_count` 在 bit_count >= 32 时会 panic,
    // 而 bit_count 是别的进程给的。
    if bit_count != 8 && bit_count != 24 && bit_count != 32 {
        return None;
    }
    if compression != BI_RGB && compression != BI_BITFIELDS {
        return None;
    }
    if width_raw <= 0 || height_raw == 0 || planes != 1 {
        return None;
    }
    let top_down = height_raw < 0;
    let width = width_raw as u32;
    let height = height_raw.unsigned_abs();
    if width > MAX_DIB_DIMENSION || height > MAX_DIB_DIMENSION {
        return None;
    }
    if u64::from(width) * u64::from(height) > MAX_DIB_PIXELS {
        return None;
    }

    // 通道掩码:V4/V5 头把它们放在头内固定偏移;40 字节头配 BI_BITFIELDS 时掩码
    // 紧跟头之后(3 个 DWORD,无 alpha)。其余情况没有显式掩码。
    let mut cursor = header_size as usize;
    let (red_mask, green_mask, blue_mask, alpha_mask) = if header_size >= 52 {
        (
            read_u32(dib, 40)?,
            read_u32(dib, 44)?,
            read_u32(dib, 48)?,
            read_u32(dib, 52)?,
        )
    } else if compression == BI_BITFIELDS {
        let (r, g, b) = (read_u32(dib, 40)?, read_u32(dib, 44)?, read_u32(dib, 48)?);
        cursor += 12;
        (r, g, b, 0)
    } else {
        (0, 0, 0, 0)
    };

    // 调色板紧随头(以及掩码)之后,条目数按 `biClrUsed`(0 表示满编)。
    let palette_offset = cursor;
    if bit_count == 8 {
        let entries = palette_entries(dib, bit_count)?;
        cursor = cursor.checked_add(entries.checked_mul(4)?)?;
    }

    // `BI_RGB` 32bpp 没有 alpha(见模块文档);掩码缺失时按 BGRA 通道序兜底,
    // 让下面的通道提取走同一条路径。
    let (red_mask, green_mask, blue_mask) = if bit_count == 24 {
        (0, 0, 0) // 24bpp 固定 BGR 布局,不用掩码
    } else if red_mask == 0 || green_mask == 0 || blue_mask == 0 {
        (0x00ff_0000, 0x0000_ff00, 0x0000_00ff)
    } else {
        (red_mask, green_mask, blue_mask)
    };

    let stride = (bit_count as usize * width as usize).div_ceil(32) * 4;
    if cursor.checked_add(stride.checked_mul(height as usize)?)? > dib.len() {
        return None;
    }

    Some(DibHeader {
        width,
        height,
        bit_count,
        red_mask,
        green_mask,
        blue_mask,
        alpha_mask,
        palette_offset,
        pixels_offset: cursor,
        stride,
        bottom_up: !top_down,
    })
}

/// 调色板条目数:`biClrUsed` 为 0 时按位深满编(8bpp → 256)。
fn palette_entries(dib: &[u8], bit_count: u16) -> Option<usize> {
    let clr_used = read_u32(dib, 32)?;
    if clr_used != 0 {
        // 条目数不可能超过满编,超过说明头字段不可信。
        let max = 1usize.checked_shl(bit_count as u32)?;
        if clr_used as usize > max {
            return None;
        }
        Some(clr_used as usize)
    } else {
        1usize.checked_shl(bit_count as u32)
    }
}

/// 读取 8bpp DIB 的调色板,返回 `(r, g, b)` 表。调色板项是 `RGBQUAD`(B, G, R, 保留)。
fn build_palette(dib: &[u8], header: &DibHeader) -> Option<Vec<(u8, u8, u8)>> {
    let entries = palette_entries(dib, header.bit_count)?;
    let start = header.palette_offset;
    let end = start.checked_add(entries.checked_mul(4)?)?;
    let bytes = dib.get(start..end)?;
    // 条目数可少于满编(部分 8bpp DIB 只带前 N 项),缺失的下标由 `get` 判 None。
    // 用 `as_chunks` 而非 `chunks_exact`:后者在 clippy 1.99 起会报
    // chunks_exact_to_as_chunks;两者都会丢掉末尾不足一项的字节。
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|quad| (quad[2], quad[1], quad[0]))
            .collect(),
    )
}

/// 把某个通道掩码位段里的值线性拉伸到 0..=255。
///
/// `mask = 0` 表示该通道不存在,返回 0。
fn scale_channel(mask: u32, pixel: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = (mask >> shift) as u64;
    let raw = u64::from((pixel & mask) >> shift);
    // 四舍五入到最近整数,避免 5/6 位通道在浅色区系统性偏暗。
    ((raw * 255 + max / 2) / max) as u8
}

/// alpha 通道:没有显式 alpha 掩码时按不透明处理(见模块文档)。
fn scale_alpha(mask: u32, pixel: u32) -> u8 {
    if mask == 0 {
        255
    } else {
        scale_channel(mask, pixel)
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

/// 把 PNG 字节转成 `CF_DIB`(32bpp `BI_RGB`,自下而上,行 4 字节对齐)。
///
/// 写剪贴板时用:现代应用认注册格式 `PNG`,传统应用(画图、部分邮件客户端)只认
/// `CF_DIB`,所以两种都挂(见 `pasteback` 的 `imp_win`)。
///
/// **alpha 一律写成不透明(`0xff`)**,这是刻意的:`BI_RGB` 的 32bpp 按规范没有 alpha
/// 通道,把真实 alpha 抄进去会让一部分按第 4 字节解释透明度的传统应用把整张图渲染成
/// 全透明(Windows 上一个老坑)。真正的透明度由同时挂上去的注册格式 `PNG` 承载,
/// 支持它的应用优先走那条路。
pub fn image_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 || width > MAX_DIB_DIMENSION || height > MAX_DIB_DIMENSION {
        return None;
    }
    if u64::from(width) * u64::from(height) > MAX_DIB_PIXELS {
        return None;
    }

    // 32bpp 时每行天然是 4 字节的整数倍,不需要额外填充。
    let stride = width as usize * 4;
    let mut out = Vec::with_capacity(HEADER_40 as usize + stride * height as usize);
    out.extend(HEADER_40.to_le_bytes()); // biSize
    out.extend((width as i32).to_le_bytes()); // biWidth
    // 正数 biHeight = 自下而上存储(下面按行倒着写)。
    out.extend((height as i32).to_le_bytes()); // biHeight
    out.extend(1u16.to_le_bytes()); // biPlanes
    out.extend(32u16.to_le_bytes()); // biBitCount
    out.extend(BI_RGB.to_le_bytes()); // biCompression
    out.extend(((stride * height as usize) as u32).to_le_bytes()); // biSizeImage
    out.extend(0u32.to_le_bytes()); // biXPelsPerMeter
    out.extend(0u32.to_le_bytes()); // biYPelsPerMeter
    out.extend(0u32.to_le_bytes()); // biClrUsed
    out.extend(0u32.to_le_bytes()); // biClrImportant
    debug_assert_eq!(out.len(), HEADER_40 as usize);

    for row in (0..height).rev() {
        for col in 0..width {
            let px = img.get_pixel(col, row).0;
            out.extend([px[2], px[1], px[0], 0xff]); // BGRA
        }
    }
    Some(out)
}

/// 构造 `CF_HDROP` 的载荷:`DROPFILES` 头 + 双 NUL 结尾的 UTF-16LE 路径表。
///
/// 手写这个 20 字节头而不是用 `windows` crate 的 `DROPFILES`,是为了让它留在
/// 这个**平台无关**的模块里(依赖 trees 里没有 `windows` crate 的平台上也要能编译、
/// 能被单测)。布局(全部小端):
///
/// ```text
/// 偏移 0  DWORD pFiles      路径表起点相对本结构起点的偏移 = 20
/// 偏移 4  POINT pt          拖放落点,粘贴近场景不用 → 0,0
/// 偏移 12 BOOL  fNC         非客户区拖放 → FALSE
/// 偏移 16 BOOL  fWide       路径是 UTF-16(否则是 ANSI)→ TRUE
/// ```
pub fn build_dropfiles(paths: &[String]) -> Vec<u8> {
    /// `DROPFILES` 的结构体大小:1 个 DWORD + 1 个 POINT(2 个 LONG)+ 2 个 BOOL。
    const DROPFILES_HEADER: u32 = 20;

    let mut out = Vec::new();
    out.extend(DROPFILES_HEADER.to_le_bytes()); // pFiles
    out.extend(0u32.to_le_bytes()); // pt.x
    out.extend(0u32.to_le_bytes()); // pt.y
    out.extend(0u32.to_le_bytes()); // fNC = FALSE
    out.extend(1u32.to_le_bytes()); // fWide = TRUE
    debug_assert_eq!(out.len(), DROPFILES_HEADER as usize);

    for path in paths {
        out.extend(path.encode_utf16().flat_map(u16::to_le_bytes));
        out.extend(0u16.to_le_bytes()); // 每条路径各自以 NUL 结尾
    }
    // 整个列表再补一个 NUL 结尾 —— 这是 `CF_HDROP` 的硬性要求(双 NUL 收尾)。
    out.extend(0u16.to_le_bytes());
    out
}

/// 把字符串编成以 NUL 结尾的 UTF-16(剪贴板文本格式的要求)。
pub fn utf16_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 剥离 Windows `std::fs::canonicalize` 结果的扩展长度前缀。
///
/// Windows 的 `canonicalize` 返回 `\\?\C:\dir\file` 形式(`\\?\UNC\server\share` 对应
/// 网络路径)。这个前缀对文件系统 API 是必需的,但显示在面板里、写进 FTS 索引、
/// 或回贴给别的应用都很难看且可能不被接受,所以在入库前剥掉:
/// `\\?\UNC\server\share` → `\\server\share`,`\\?\C:\dir` → `C:\dir`。
pub fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}
