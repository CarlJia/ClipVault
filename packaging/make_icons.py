#!/usr/bin/env python3
"""Generate ClipVault clipboard-glyph icons (no external deps).

Produces:
  - a 1024x1024 RGBA master (color, for the app/Dock icon)
  - a monochrome template master (black + alpha, for the menu-bar tray)
Downstream `sips`/`iconutil` resize these into the sizes Tauri needs.
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


if __name__ == "__main__":
    import sys
    outdir = sys.argv[1] if len(sys.argv) > 1 else "."
    # Color master for the app icon (a deep slate blue clipboard).
    open(f"{outdir}/master-color.png", "wb").write(render(1024, (63, 90, 168)))
    # Template master for the menu-bar tray (pure black + alpha).
    open(f"{outdir}/master-template.png", "wb").write(render(1024, (0, 0, 0)))
    print("wrote master-color.png + master-template.png")
