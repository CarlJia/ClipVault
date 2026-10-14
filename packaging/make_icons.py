#!/usr/bin/env python3
"""从 packaging/icon-source.png 生成 ClipVault 全套图标（纯 stdlib，无外部依赖）。

源图是「白底上的圆角方块 3D 插画」。脚本自己解码 PNG、抠掉白底、按内容裁剪，
再派生两类产物：

  A. 彩色应用图标 —— 直接用源图本体
       icon.icns           macOS：Apple 网格（1024 画布里实体 824×824 居中、四周透明留白）
       icon.ico            Windows：满幅，16/32/48/64/128/256
       32x32.png           通用小图（Windows/Linux 窗口与任务栏）：满幅
       128x128.png         "
       128x128@2x.png      "
       tray-windows.png    Windows 通知区托盘：满幅彩色，深色任务栏上照样看得见（RW7）

  B. 菜单栏模板图 —— 源图是 3D 彩色插画，缩到菜单栏的 16–18pt 会糊成一团，
     所以菜单栏另画一个单色「剪贴板 + 锁」字形，走 macOS 的 template image 语义，
     随菜单栏明暗自动反色（托盘的状态可视化靠菜单标题字形，与本图无关）：
       tray-template.png      44×44（渲染成 18pt，见 TRAY_TEMPLATE_SIZE 的说明）

用法：python3 packaging/make_icons.py
"""
import os
import struct
import sys
import zlib

# ---------------------------------------------------------------- PNG 编解码

def decode_png(path):
    """读 8 位非隔行 PNG，返回 (宽, 高, RGBA bytearray)。

    只支持本仓库源图会用到的子集（8 位色深、非隔行），灰度与调色板也一并支持，
    省得换个源图就报错。隔行（Adam7）要把分块重排，用不上，直接报错。
    """
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} 不是 PNG")
    pos = 8
    idat = bytearray()
    palette = trns = None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        tag = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if tag == b"IHDR":
            width, height, depth, color_type, _, _, interlace = struct.unpack(">IIBBBBB", body)
        elif tag == b"IDAT":
            idat += body
        elif tag == b"PLTE":
            palette = body
        elif tag == b"tRNS":
            trns = body
        elif tag == b"IEND":
            break
    if depth != 8:
        raise ValueError(f"{path}: 只支持 8 位色深（实际 {depth}）")
    if interlace:
        raise ValueError(f"{path}: 不支持隔行 PNG")

    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[color_type]
    stride = width * channels
    raw = zlib.decompress(bytes(idat))
    rows = []
    prev = bytearray(stride)
    offset = 0
    for _ in range(height):
        filter_type = raw[offset]
        offset += 1
        line = bytearray(raw[offset:offset + stride])
        offset += stride
        if filter_type == 1:  # Sub
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif filter_type == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif filter_type == 3:  # Average
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif filter_type == 4:  # Paeth
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prev[i]
                c = prev[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        elif filter_type != 0:
            raise ValueError(f"{path}: 未知的行过滤器 {filter_type}")
        rows.append(line)
        prev = line

    pixels = bytearray(width * height * 4)
    for y, line in enumerate(rows):
        base = y * width * 4
        if color_type == 6:
            pixels[base:base + stride] = line
        elif color_type == 2:
            for x in range(width):
                pixels[base + x * 4:base + x * 4 + 3] = line[x * 3:x * 3 + 3]
                pixels[base + x * 4 + 3] = 255
        elif color_type == 0:
            for x in range(width):
                v = line[x]
                pixels[base + x * 4:base + x * 4 + 4] = bytes((v, v, v, 255))
        elif color_type == 4:
            for x in range(width):
                v = line[x * 2]
                pixels[base + x * 4:base + x * 4 + 4] = bytes((v, v, v, line[x * 2 + 1]))
        else:  # 3：调色板
            for x in range(width):
                idx = line[x]
                pixels[base + x * 4:base + x * 4 + 3] = palette[idx * 3:idx * 3 + 3]
                pixels[base + x * 4 + 3] = trns[idx] if trns and idx < len(trns) else 255
    return width, height, pixels


def encode_png(width, height, pixels):
    """RGBA -> PNG，逐行挑最优行过滤器。

    每行把 5 种过滤器都试一遍，取「过滤后字节绝对值之和」最小的那个 —— 这是 PNG
    规范推荐的启发式。对纯色线条图无所谓，但对本项目这种 3D 渐变插画差别很大
    （固定 filter 0 会让 icon.icns 从 ~0.2 MB 涨到 ~1.7 MB）。
    """
    stride = width * 4
    raw = bytearray()
    prev = bytearray(stride)
    for y in range(height):
        line = pixels[y * stride:(y + 1) * stride]
        best = None
        for filter_type in range(5):
            filtered = bytearray(stride)
            for i in range(stride):
                left = line[i - 4] if i >= 4 else 0
                up = prev[i]
                up_left = prev[i - 4] if i >= 4 else 0
                if filter_type == 0:
                    pred = 0
                elif filter_type == 1:
                    pred = left
                elif filter_type == 2:
                    pred = up
                elif filter_type == 3:
                    pred = (left + up) >> 1
                else:
                    p = left + up - up_left
                    pa, pb, pc = abs(p - left), abs(p - up), abs(p - up_left)
                    pred = left if (pa <= pb and pa <= pc) else (up if pb <= pc else up_left)
                filtered[i] = (line[i] - pred) & 0xFF
            score = sum(v if v < 128 else 256 - v for v in filtered)
            if best is None or score < best[0]:
                best = (score, filter_type, filtered)
        raw.append(best[1])
        raw += best[2]
        prev = line

    def chunk(tag, body):
        return (struct.pack(">I", len(body)) + tag + body
                + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))


# ---------------------------------------------------------------- 图像处理

class Bitmap:
    """行优先的 RGBA 位图。"""

    def __init__(self, width, height, pixels):
        self.w = width
        self.h = height
        self.px = pixels


# 源图背景是近白的。阈值 248：再低会把方块边缘的抗锯齿像素也判成背景、边缘被啃掉
# 一圈；再高则抠不干净（源图背景带轻微噪点，并非处处纯白）。
WHITE_CUTOFF = 248


def cut_out_background(bitmap):
    """从四边 flood fill 近白像素 → 判为「外部」，alpha 置 0。

    源图若本来就带透明（任一像素 alpha < 255），原样返回 —— 这套只针对白底插画。
    """
    w, h, px = bitmap.w, bitmap.h, bitmap.px
    if any(px[i] != 255 for i in range(3, len(px), 4)):
        return bitmap

    outside = bytearray(w * h)
    frontier = []

    def is_white(x, y):
        i = (y * w + x) * 4
        return min(px[i], px[i + 1], px[i + 2]) >= WHITE_CUTOFF

    for x in range(w):
        for y in (0, h - 1):
            if is_white(x, y):
                outside[y * w + x] = 1
                frontier.append((x, y))
    for y in range(h):
        for x in (0, w - 1):
            if is_white(x, y):
                outside[y * w + x] = 1
                frontier.append((x, y))

    # 外部是一整片连通的白，逐层向里推进直到撞上方块边缘。
    while frontier:
        nxt = []
        for x, y in frontier:
            for nx, ny in ((x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)):
                if 0 <= nx < w and 0 <= ny < h:
                    i = ny * w + nx
                    if not outside[i] and is_white(nx, ny):
                        outside[i] = 1
                        nxt.append((nx, ny))
        frontier = nxt

    out = Bitmap(w, h, bytearray(px))
    for i in range(w * h):
        if outside[i]:
            out.px[i * 4 + 3] = 0
    return out


def content_box(bitmap):
    """不透明像素的包围盒，再按内容中心扩成正方形。

    源图的方块本身未必严格正方形（底部带一点极淡的投影），按较长边扩成正方形、
    中心不变，缩小时才不会被压扁。扩出来的部分在图像边界外时整体平移回来。
    """
    w, h, px = bitmap.w, bitmap.h, bitmap.px
    min_x, min_y, max_x, max_y = w, h, -1, -1
    for y in range(h):
        row = y * w * 4
        if not any(px[row + x * 4 + 3] for x in range(w)):
            continue
        if y < min_y:
            min_y = y
        if y > max_y:
            max_y = y
        for x in range(w):
            if px[row + x * 4 + 3]:
                if x < min_x:
                    min_x = x
                if x > max_x:
                    max_x = x
    if max_x < 0:
        raise ValueError("源图抠完背景后没有任何不透明像素")

    side = max(max_x - min_x, max_y - min_y) + 1
    side = min(side, w, h)
    x0 = round((min_x + max_x + 1) / 2 - side / 2)
    y0 = round((min_y + max_y + 1) / 2 - side / 2)
    x0 = min(max(x0, 0), w - side)
    y0 = min(max(y0, 0), h - side)
    return x0, y0, side


def crop(bitmap, x0, y0, size):
    px = bytearray(size * size * 4)
    for y in range(size):
        src = ((y + y0) * bitmap.w + x0) * 4
        px[y * size * 4:(y + 1) * size * 4] = bitmap.px[src:src + size * 4]
    return Bitmap(size, size, px)


def resize(bitmap, nw, nh):
    """面积平均（box filter）缩放：目标像素 = 覆盖到的源像素按重叠面积加权平均。

    颜色与 alpha 走**非预乘平均**的话，缩到 16px 时边缘半透明的目标像素会把透明区
    的白色一起平均进来，图标边上一圈白晕。所以这里对 RGB 按 alpha 预乘后再平均，
    最后除掉平均 alpha —— 透明像素无论 RGB 是什么都不参与颜色，边缘干净。
    """
    sw, sh, src = bitmap.w, bitmap.h, bitmap.px
    out = bytearray(nw * nh * 4)
    for dy in range(nh):
        sy0, sy1 = dy * sh / nh, (dy + 1) * sh / nh
        for dx in range(nw):
            sx0, sx1 = dx * sw / nw, (dx + 1) * sw / nw
            r = g = b = 0.0
            alpha_sum = 0.0
            total = 0.0
            for y in range(int(sy0), min(int(sy1) + 1, sh)):
                wy = min(y + 1, sy1) - max(y, sy0)
                if wy <= 0:
                    continue
                base = y * sw
                for x in range(int(sx0), min(int(sx1) + 1, sw)):
                    wx = min(x + 1, sx1) - max(x, sx0)
                    if wx <= 0:
                        continue
                    weight = wx * wy * (src[(base + x) * 4 + 3] / 255.0)
                    i = (base + x) * 4
                    r += src[i] * weight
                    g += src[i + 1] * weight
                    b += src[i + 2] * weight
                    alpha_sum += weight
                    total += wx * wy
            o = (dy * nw + dx) * 4
            if alpha_sum > 0:
                out[o] = min(255, int(r / alpha_sum + 0.5))
                out[o + 1] = min(255, int(g / alpha_sum + 0.5))
                out[o + 2] = min(255, int(b / alpha_sum + 0.5))
            out[o + 3] = min(255, int(alpha_sum / total * 255 + 0.5))
    return Bitmap(nw, nh, out)


# macOS 应用图标规范：1024 画布里实体占 824，四周留白给系统画投影与选中效果。
# 不留白的话图标在 Dock 里会比原生 App 明显大一截。Windows 侧相反，惯例是铺满画布。
MACOS_BODY_RATIO = 824 / 1024


def place(bitmap, size, body_ratio):
    """把方块缩到 size*body_ratio，居中放进 size×size 的透明画布。"""
    body = max(1, round(size * body_ratio))
    art = resize(bitmap, body, body)
    canvas = bytearray(size * size * 4)
    offset = (size - body) // 2
    for y in range(body):
        dst = ((y + offset) * size + offset) * 4
        canvas[dst:dst + body * 4] = art.px[y * body * 4:(y + 1) * body * 4]
    return Bitmap(size, size, canvas)


# ---------------------------------------------------------------- 菜单栏字形

def _rounded_rect(x, y, x0, y0, x1, y1, radius):
    cx = min(max(x, x0 + radius), x1 - radius)
    cy = min(max(y, y0 + radius), y1 - radius)
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2


def _clipboard_glyph(x, y):
    """单色「剪贴板 + 锁」字形，坐标空间 22×22。

    轮廓剪贴板（板身描边 + 顶部夹子）里放一把实心锁：语义和源图的「文档 + 锁」
    一致，而 16pt 下仍认得出是个剪贴板。整块在渲染时超采样，边缘自带抗锯齿。
    """
    body_x0, body_y0, body_x1, body_y1, body_r = 3.8, 3.8, 18.2, 20.5, 2.4
    stroke = 1.50
    if _rounded_rect(x, y, body_x0, body_y0, body_x1, body_y1, body_r) and not _rounded_rect(
            x, y, body_x0 + stroke, body_y0 + stroke,
            body_x1 - stroke, body_y1 - stroke, body_r - stroke * 0.7):
        return True

    # 顶部夹子：圆角矩形挖一个椭圆孔
    if _rounded_rect(x, y, 7.5, 1.2, 14.5, 5.1, 1.45):
        return not (((x - 11.0) / 1.10) ** 2 + ((y - 3.2) / 0.88) ** 2 <= 1.0)

    # 锁：竖直居中在板身内部。shackle 取上半环，body 取圆角矩形。
    cx, scale = 11.0, 1.70
    top = 12.15 - 0.35 * scale
    half_width = 2.35 * scale
    if _rounded_rect(x, y, cx - half_width, top + 0.55 * scale,
                     cx + half_width, top + 0.55 * scale + 1.95 * scale, 0.55 * scale):
        return True
    if y <= top:
        distance = ((x - cx) ** 2 + (y - top) ** 2) ** 0.5
        return 1.05 * scale <= distance <= 1.72 * scale
    return False


def render_glyph(size):
    """超采样渲染字形，返回 Bitmap（黑色 + alpha）。"""
    supersample = 8
    px = bytearray(size * size * 4)
    for py in range(size):
        for pxi in range(size):
            hits = 0
            for sy in range(supersample):
                for sx in range(supersample):
                    x = (pxi + (sx + 0.5) / supersample) / size * 22.0
                    y = (py + (sy + 0.5) / supersample) / size * 22.0
                    if _clipboard_glyph(x, y):
                        hits += 1
            px[(py * size + pxi) * 4 + 3] = hits * 255 // (supersample * supersample)
    return Bitmap(size, size, px)


# ---------------------------------------------------------------- ICNS / ICO

# macOS 认的 ICNS 条目类型 → 像素尺寸。这 10 条覆盖 16–1024 的每档 1x/2x，
# PNG 载荷自 macOS 10.7 起原生支持。
#
# 注意：这里的 icp4/icp5（16/32 用 PNG）与 `iconutil` 自己吐出来的形态**不完全一样** ——
# 它给 16/32 写的是 ic04/ic05（ARGB + 私有 RLE，apple 自家格式，编解码都未公开）。
# 本脚本刻意不走那条路：ImageIO（Finder / Dock / Launchpad / NSImage）和 Quick Look
# 都能正确读出 icp4/icp5 的 PNG，实测 16/32 两档渲染无误。
# 唯一的副作用是 `iconutil -c iconset` 反向解包 icp4/icp5 时会吐出乱码（它自己的 bug，
# 与渲染无关）。要肉眼验证请改用：`qlmanage -t -s 256 -o <dir> app/icons/icon.icns`。
ICNS_ENTRIES = (
    (b"icp4", 16), (b"icp5", 32), (b"ic11", 32), (b"ic12", 64),
    (b"ic07", 128), (b"ic13", 256), (b"ic08", 256),
    (b"ic09", 512), (b"ic14", 512), (b"ic10", 1024),
)


def build_icns(source, body_ratio):
    """手写 ICNS 容器：'icns' + 总长，后面跟「类型 + 长度 + PNG 载荷」。

    比调 sips/iconutil 多几十行，换来的是：一条命令可复现，且不依赖 macOS ——
    在 Windows 开发机上也能重新生成。
    """
    bodies = {}
    entries = bytearray()
    for tag, size in ICNS_ENTRIES:
        if size not in bodies:
            bodies[size] = encode_png(size, size, place(source, size, body_ratio).px)
        payload = bodies[size]
        entries += tag + struct.pack(">I", len(payload) + 8) + payload
    return b"icns" + struct.pack(">I", len(entries) + 8) + bytes(entries)


# Windows 需要一整套尺寸：16/32 出现在任务栏与资源管理器列表，48/64 是 exe 属性页
# 与 Alt+Tab，128/256 是「大图标」视图。
ICO_SIZES = (16, 32, 48, 64, 128, 256)


def _ico_entry(size, pixels):
    """一个 32bpp BMP 条目：DIB 头 + 自下而上的 BGRA 行 + AND 掩码。"""
    stride = size * 4
    mask_stride = ((size + 31) // 32) * 4
    # BITMAPINFOHEADER。ICO 里的高度是 XOR 位图 + AND 掩码的总高，所以是 size*2。
    header = struct.pack(
        "<IiiHHIIiiII",
        40,             # biSize
        size,           # biWidth
        size * 2,       # biHeight
        1,              # biPlanes
        32,             # biBitCount
        0,              # biCompression = BI_RGB
        stride * size,  # biSizeImage
        0, 0,           # 像素/米（用不到）
        0, 0,           # biClrUsed / biClrImportant
    )
    xor = bytearray()
    for y in range(size - 1, -1, -1):  # 自下而上
        row = y * size * 4
        for x in range(size):
            i = row + x * 4
            xor += bytes((pixels[i + 2], pixels[i + 1], pixels[i], pixels[i + 3]))
    # 32bpp 下 AND 掩码全 0 = 完全交给 XOR 位图（含 alpha 通道）。
    return header + bytes(xor) + b"\x00" * (mask_stride * size)


def build_ico(source, body_ratio):
    """多尺寸 ICO。

    用 BMP(DIB) 条目而不是 PNG-in-ICO：后者要 Vista+ 才认，且部分资源编译器与
    打包器对 PNG 条目支持不全 —— 这个图标要同时喂给 exe 资源嵌入、NSIS 安装包与
    任务栏，走传统条目最稳。
    """
    entries = [(size, _ico_entry(size, place(source, size, body_ratio).px)) for size in ICO_SIZES]
    out = bytearray(struct.pack("<HHH", 0, 1, len(entries)))  # reserved / type=icon / count
    offset = 6 + 16 * len(entries)
    for size, data in entries:
        out += struct.pack(
            "<BBBBHHII",
            size % 256,  # 宽（256 记作 0）
            size % 256,  # 高
            0,           # 调色板颜色数（真彩色为 0）
            0,           # reserved
            1,           # planes
            32,          # bitCount
            len(data),
            offset,
        )
        offset += len(data)
    for _, data in entries:
        out += data
    return bytes(out)


# ---------------------------------------------------------------- 入口

# Windows 托盘图标尺寸：通知区在 100% DPI 下是 16×16、200% 下 32×32，由 shell 自行缩放。
# 取 32（缩到 16 是干净的 2:1）；留一点边距，免得方块顶到通知区的格子上。
TRAY_WINDOWS_SIZE = 32
TRAY_MARGIN_RATIO = 0.94

# 菜单栏模板图的**像素**尺寸 —— 不是显示尺寸。tray-icon 在 macOS 上会把图标统一
# 设成 18pt 高（`nsimage.setSize(NSSize::new(icon_width, 18.0))`）再按像素数等比缩放，
# 所以给多少像素只影响清晰度、不影响大小：22px 在 Retina 上要被放大到 36 设备像素
# （糊），44px 是缩下来用（清晰）。字形坐标空间是 22 单位，44px 正好 2 倍。
TRAY_TEMPLATE_SIZE = 44


def main():
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    icons_dir = os.path.join(repo_root, "app", "icons")
    source_path = os.path.join(repo_root, "packaging", "icon-source.png")

    print(f"读取源图 {os.path.relpath(source_path, repo_root)}")
    width, height, pixels = decode_png(source_path)
    cut_out = cut_out_background(Bitmap(width, height, pixels))
    artwork = crop(cut_out, *content_box(cut_out))
    print(f"  抠白底 + 裁剪 → {artwork.w}×{artwork.h}")

    written = []

    def write(name, blob):
        with open(os.path.join(icons_dir, name), "wb") as fh:
            fh.write(blob)
        written.append((name, len(blob)))

    # macOS：Apple 网格留白；Windows / Linux：满幅。
    write("icon.icns", build_icns(artwork, MACOS_BODY_RATIO))
    write("icon.ico", build_ico(artwork, 1.0))
    for name, size in (("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256)):
        write(name, encode_png(size, size, place(artwork, size, 1.0).px))

    # macOS 菜单栏：单色模板图（黑色 + alpha，系统按菜单栏明暗自动反色）。
    write("tray-template.png",
          encode_png(TRAY_TEMPLATE_SIZE, TRAY_TEMPLATE_SIZE,
                     render_glyph(TRAY_TEMPLATE_SIZE).px))

    # Windows 通知区：彩色。模板图语义在 Windows 上没有意义，而黑字形在深色任务栏上
    # 根本看不见（RW7），所以两边各用各的。
    write("tray-windows.png",
          encode_png(TRAY_WINDOWS_SIZE, TRAY_WINDOWS_SIZE,
                     place(artwork, TRAY_WINDOWS_SIZE, TRAY_MARGIN_RATIO).px))

    for name, size in written:
        print(f"  app/icons/{name}  ({size / 1024:.1f} KiB)")


if __name__ == "__main__":
    sys.exit(main())
