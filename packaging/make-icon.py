#!/usr/bin/env python3
"""Midna's app icon, drawn in code (no image tools needed): "Twilight tiles".

Midna's own window as tiles on a dark squircle: a sidebar with three status dots (teal,
orange, lavender), the main pane holding a lavender crescent, a teal split pane with a
block cursor, and an orange status bar. The colors are the setup screen's Twilight Tiles.

Usage: packaging/make-icon.py packaging/assets/Midna.icns
       packaging/make-icon.py --invert packaging/assets/MidnaDev.icns   (Midna Dev: colors inverted)
       packaging/make-icon.py packaging/assets/Midna.ico                (Windows, any OS)
Writes a 1024px PNG with the stdlib only, then uses sips + iconutil for the .icns sizes.
A .ico is all stdlib: the body fills more of the canvas and has no shadow (Windows icons
don't), box-filtered down to the sizes Windows asks for, each stored as a PNG.
The design is on a 100-unit grid (the body is 9..91, the macOS 824px icon body).
"""
import math, os, struct, subprocess, sys, tempfile, zlib

N = 1024
U = 10.24  # px per design unit
BODY = (0x14, 0x12, 0x20)
TILE = (0x2A, 0x25, 0x40)
BAR = (0x4B, 0x44, 0x66)
LAVENDER = (0xB7, 0x9A, 0xE8)
TEAL = (0x3F, 0xC4, 0xC0)
ORANGE = (0xF2, 0x8A, 0x4B)
CURSOR = (0xE4, 0xE7, 0xEE)


def rrect_sdf(x, y, cx, cy, hw, hh, r):
    qx, qy = abs(x - cx) - hw + r, abs(y - cy) - hh + r
    return math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - r


def cov(d):  # signed distance (px) -> coverage, ~1px anti-aliasing
    return 0.0 if d >= 0.5 else 1.0 if d <= -0.5 else 0.5 - d


def blend(dst, src, a):
    return tuple(d + (s - d) * a for d, s in zip(dst, src))


def box(x, y, left, top, w, h, r):
    """Distance in px to a rounded rect given in design units."""
    return rrect_sdf(x, y, (left + w / 2) * U, (top + h / 2) * U, w / 2 * U, h / 2 * U, r * U)


def disc(x, y, cx, cy, r):
    return math.hypot(x - cx * U, y - cy * U) - r * U


# (shape, color, opacity), painted in order.
SHAPES = [
    (lambda x, y: box(x, y, 19, 19, 20, 55, 5), TILE, 1.0),  # sidebar
    (lambda x, y: disc(x, y, 25, 27, 2), TEAL, 1.0),
    (lambda x, y: disc(x, y, 25, 35, 2), ORANGE, 1.0),
    (lambda x, y: disc(x, y, 25, 43, 2), LAVENDER, 1.0),
    (lambda x, y: box(x, y, 29, 26, 6, 2, 1), BAR, 1.0),
    (lambda x, y: box(x, y, 29, 34, 6, 2, 1), BAR, 1.0),
    (lambda x, y: box(x, y, 29, 42, 6, 2, 1), BAR, 1.0),
    (lambda x, y: box(x, y, 43, 19, 38, 32, 5), TILE, 1.0),  # main pane
    (lambda x, y: max(disc(x, y, 62, 35, 9), -disc(x, y, 66.5, 31.5, 7.5)), LAVENDER, 1.0),  # crescent
    (lambda x, y: box(x, y, 43, 55, 38, 19, 5), TEAL, 0.4),  # split pane
    (lambda x, y: box(x, y, 49, 60, 4, 9, 1), CURSOR, 1.0),
    (lambda x, y: box(x, y, 19, 78, 62, 4, 2), ORANGE, 1.0),  # status bar
]


def pixel(x, y, shadow=True):
    # macOS icon grid: 824px body, centred, ~185px corners, soft shadow below
    body = rrect_sdf(x, y, 512, 512, 412, 412, 185)
    out = (0.0, 0.0, 0.0, 0.0)
    if shadow:
        under = rrect_sdf(x, y, 512, 524, 412, 412, 185)
        sa = max(0.0, 1.0 - max(under, 0) / 28.0) * 0.16 if under > -1 else 0.16
        if sa > 0 and body > -0.5:
            out = (0.0, 0.0, 0.0, sa)
    a = cov(body)
    if a <= 0:
        return out
    bg = BODY
    # inner hairline
    if -6 < body <= 0:
        bg = blend(bg, (255, 255, 255), 0.06 * cov(abs(body + 3) - 2))
    for shape, color, opacity in SHAPES:
        c = cov(shape(x, y))
        if c > 0:
            bg = blend(bg, color, c * opacity)
    # composite over the shadow
    oa = out[3]
    ra = a + oa * (1 - a)
    rgb = tuple((c * a + 0 * oa * (1 - a)) / ra for c in bg) if ra > 0 else (0, 0, 0)
    return (*rgb, ra)


def write_png(path, w, h, rows):
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


def render(px, invert):
    """N×N RGBA rows from px(x, y) -> (r, g, b, a)."""
    rows = []
    for y in range(N):
        row = bytearray()
        for x in range(N):
            r, g, b, a = px(x + 0.5, y + 0.5)
            if invert:
                r, g, b = 255 - r, 255 - g, 255 - b
            row += bytes((int(r + 0.5), int(g + 0.5), int(b + 0.5), int(a * 255 + 0.5)))
        rows.append(row)
    return rows


def png_bytes(w, h, rows):
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def spans(n, size):
    """For each of `size` output cells, the (source index, weight) pairs it covers out of n."""
    k = n / size
    out = []
    for i in range(size):
        lo, hi = i * k, (i + 1) * k
        out.append([(j, (min(hi, j + 1) - max(lo, j)) / k) for j in range(int(lo), min(n, math.ceil(hi)))])
    return out


def downsample(rows, size):
    """Area-average N×N RGBA rows to size×size, in premultiplied alpha (no dark fringes)."""
    pre = [[(r[i] * r[i + 3], r[i + 1] * r[i + 3], r[i + 2] * r[i + 3], r[i + 3]) for i in range(0, len(r), 4)] for r in rows]
    sp = spans(N, size)
    wide = [[tuple(sum(row[j][c] * w for j, w in s) for c in range(4)) for s in sp] for row in pre]
    out = []
    for s in sp:
        row = bytearray()
        for x in range(size):
            p = [sum(wide[j][x][c] * w for j, w in s) for c in range(4)]
            a = p[3]
            rgb = (p[0] / a, p[1] / a, p[2] / a) if a > 0 else (0, 0, 0)
            row += bytes(min(255, int(v + 0.5)) for v in (*rgb, a))
        out.append(row)
    return out


def write_ico(path, rows):
    """A .ico of PNG entries (Vista and later read PNG at every size)."""
    sizes = (256, 128, 64, 48, 40, 32, 24, 20, 16)
    images = [png_bytes(s, s, downsample(rows, s)) for s in sizes]
    head = struct.pack("<HHH", 0, 1, len(images))
    offset = len(head) + 16 * len(images)
    entries = b""
    for s, data in zip(sizes, images):
        entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with open(path, "wb") as f:
        f.write(head + entries + b"".join(images))


def main():
    args = sys.argv[1:]
    invert = "--invert" in args
    args = [a for a in args if a != "--invert"]
    out = args[0] if args else "Midna.icns"
    if out.endswith(".ico"):
        # Windows: the body nearly fills the canvas (482 of 512px half-width), no shadow
        k = 412 / 482
        rows = render(lambda x, y: pixel(512 + (x - 512) * k, 512 + (y - 512) * k, shadow=False), invert)
        write_ico(out, rows)
        print(f"wrote {out}")
        return
    rows = render(pixel, invert)
    with tempfile.TemporaryDirectory() as tmp:
        master = os.path.join(tmp, "icon_1024.png")
        write_png(master, N, N, rows)
        iconset = os.path.join(tmp, "Midna.iconset")
        os.mkdir(iconset)
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = size * scale
                name = f"icon_{size}x{size}{'@2x' if scale == 2 else ''}.png"
                subprocess.run(["sips", "-z", str(px), str(px), master, "--out", os.path.join(iconset, name)], check=True, capture_output=True)
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", out], check=True)
        png_out = os.path.splitext(out)[0] + ".png"
        subprocess.run(["sips", "-z", "512", "512", master, "--out", png_out], check=True, capture_output=True)
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
