#!/usr/bin/env python3
"""Generate ClipVault clipboard-glyph icons (no external deps).

Produces:
  - a 1024x1024 RGBA master (color, for the app/Dock icon)
  - a monochrome template master (black + alpha, for the menu-bar tray)
Downstream `sips`/`iconutil` resize these into the sizes Tauri needs.

Also writes `app/icons/icon.ico` (multi-size, 32bpp BMP entries) -- Windows 打包与
exe 图标资源都要求 bundle.icon 里有一个 .ico，而 `sips`/`iconutil` 都产不出它。
输出路径固定为 app/icons/icon.ico（无论 outdir 传的是什么），因为那是 Tauri 要找的位置。
"""
import struct
import zlib


def _png(width, height, pixels):
    """pixels: list of (r,g,b,a) rows-major. Returns PNG bytes."""
    raw = bytearray()
    for y in range(height):
        raw.append(0)  # filter type 0
        for x in range(width):
            r, g, b, a = pixels[y * width + x]
            raw += bytes((r, g, b, a))

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)  # 8-bit RGBA
    idat = zlib.compress(bytes(raw), 9)
    return sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def _rounded_rect(px, py, w, h, x0, y0, x1, y1, radius, inside):
    """Mark pixels inside a rounded rectangle via the `inside` set."""
    for y in range(y0, y1):
        for x in range(x0, x1):
            cx = min(max(x, x0 + radius), x1 - radius)
            cy = min(max(y, y0 + radius), y1 - radius)
            if (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2:
                inside.add((x, y))


def clipboard_mask(size):
    """Return a set of (x,y) pixels that form a clipboard glyph, plus an
    'inner' set (the paper cut-out) so the body reads as an outline + clip."""
    s = size
    body = set()
    # clipboard body
    bx0, by0 = int(s * 0.22), int(s * 0.16)
    bx1, by1 = int(s * 0.78), int(s * 0.90)
    r = int(s * 0.10)
    _rounded_rect(None, None, None, None, bx0, by0, bx1, by1, r, body)

    # hollow interior (leave an outline frame)
    inner = set()
    frame = int(s * 0.075)
    _rounded_rect(None, None, None, None, bx0 + frame, by0 + frame,
                  bx1 - frame, by1 - frame, max(1, r - frame), inner)

    # clip tab at top center
    clip = set()
    cw = int(s * 0.30)
    cx0 = (s - cw) // 2
    cx1 = cx0 + cw
    cy0 = int(s * 0.08)
    cy1 = int(s * 0.22)
    _rounded_rect(None, None, None, None, cx0, cy0, cx1, cy1,
                  int(s * 0.05), clip)

    # two "text lines" inside the paper
    lines = set()
    lx0, lx1 = int(s * 0.34), int(s * 0.66)
    for ly in (int(s * 0.42), int(s * 0.54), int(s * 0.66)):
        for x in range(lx0, lx1):
            for y in range(ly, ly + max(2, int(s * 0.035))):
                lines.add((x, y))

    # final glyph = (body outline) + clip + lines, minus interior (except lines)
    glyph = (body - inner) | clip | lines
    return glyph


def render(size, color):
    glyph = clipboard_mask(size)
    r, g, b = color
    pixels = []
    for y in range(size):
        for x in range(size):
            if (x, y) in glyph:
                pixels.append((r, g, b, 255))
            else:
                pixels.append((0, 0, 0, 0))
    return _png(size, size, pixels)


# Color master for the app icon (a deep slate blue clipboard).
APP_COLOR = (63, 90, 168)
# Windows 需要一套 16–256 的尺寸：16/32 出现在任务栏与资源管理器列表，
# 48/64 是 exe 属性页与 Alt+Tab，128/256 是「大图标」视图。
ICO_SIZES = (16, 32, 48, 64, 128, 256)
# Windows 托盘图标尺寸：通知区在 100% DPI 下是 16×16、200% 下 32×32，由 shell 自行缩放。
# 取 32 —— 与 icon.ico 的 32 档同源同色，缩到 16 是干净的 2:1。
TRAY_WINDOWS_SIZE = 32


def ico_pixels(size, color):
    """和 `render` 同样的字形，但返回行优先的 RGBA 列表（ICO 条目要裸像素）。"""
    glyph = clipboard_mask(size)
    r, g, b = color
    return [
        (r, g, b, 255) if (x, y) in glyph else (0, 0, 0, 0)
        for y in range(size)
        for x in range(size)
    ]


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
        for x in range(size):
            r, g, b, a = pixels[y * size + x]
            xor += bytes((b, g, r, a))
    # 32bpp 下 AND 掩码全 0 = 完全交给 XOR 位图（含 alpha 通道）。
    and_mask = b"\x00" * (mask_stride * size)
    return header + bytes(xor) + and_mask


def ico(sizes, color):
    """多尺寸 ICO。

    用 BMP(DIB) 条目而不是 PNG-in-ICO：后者要 Vista+ 才认，且部分资源编译器与
    打包器对 PNG 条目支持不全 —— 这个图标要同时喂给 exe 资源嵌入、NSIS 安装包与
    任务栏，走传统条目最稳。
    """
    entries = [_ico_entry(size, ico_pixels(size, color)) for size in sizes]
    out = bytearray(struct.pack("<HHH", 0, 1, len(entries)))  # reserved / type=icon / count
    offset = 6 + 16 * len(entries)
    for size, data in zip(sizes, entries):
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
    for data in entries:
        out += data
    return bytes(out)


if __name__ == "__main__":
    import os
    import sys
    outdir = sys.argv[1] if len(sys.argv) > 1 else "."
    open(f"{outdir}/master-color.png", "wb").write(render(1024, APP_COLOR))
    # Template master for the menu-bar tray (pure black + alpha).
    open(f"{outdir}/master-template.png", "wb").write(render(1024, (0, 0, 0)))

    # icon.ico 固定写到 app/icons/：那是 Tauri 的 bundle.icon 要找的位置。
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    ico_path = os.path.join(repo_root, "app", "icons", "icon.ico")
    with open(ico_path, "wb") as fh:
        fh.write(ico(ICO_SIZES, APP_COLOR))

    # Windows 的彩色托盘图标（U6 / RW7）。必须彩色且**不能**当模板图用：
    # 模板标志在 Windows 上没有意义，而黑色模板图在深色任务栏上根本看不见。
    # 与 icon.ico 同色同字形，只是不带 16–256 那一套尺寸。
    tray_path = os.path.join(repo_root, "app", "icons", "tray-windows.png")
    with open(tray_path, "wb") as fh:
        fh.write(render(TRAY_WINDOWS_SIZE, APP_COLOR))

    print(f"wrote master-color.png + master-template.png to {outdir}")
    print(f"wrote {ico_path} ({', '.join(str(s) for s in ICO_SIZES)})")
    print(f"wrote {tray_path} ({TRAY_WINDOWS_SIZE}x{TRAY_WINDOWS_SIZE})")
