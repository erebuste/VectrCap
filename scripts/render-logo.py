"""Render the Vectr / VectrCap marks to SVG, PNG and ICO (stdlib only).

The geometry below is the single source for both logos, on a 64-unit grid.
Run: python scripts/render-logo.py  -> writes assets/logo/
"""
import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "assets" / "logo"
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]

TILE, TILE_EDGE = "#17121F", "#3A2F4E"
LEFT, RIGHT, MARK = "#8B6CF0", "#C4A8FF", "#5B4A78"
LEFT_POLY = [(14, 16), (24, 16), (32, 38), (32, 50)]
RIGHT_POLY = [(50, 16), (40, 16), (32, 38), (32, 50)]
# Small marks that tell the two apart: crosshair ticks (capture) vs corner marks (server).
TICKS = {
    "vectrcap": [(31, 8, 33, 12), (31, 52, 33, 56), (8, 31, 12, 33), (52, 31, 56, 33)],
    "vectr": [(9, 9, 15, 11), (9, 9, 11, 15), (49, 9, 55, 11), (53, 9, 55, 15),
              (9, 53, 15, 55), (9, 49, 11, 55), (49, 53, 55, 55), (53, 49, 55, 55)],
}


def rgb(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))


def rounded_rect_dist(x, y, half, r):
    qx, qy = abs(x - 32) - (half - r), abs(y - 32) - (half - r)
    out = (max(qx, 0) ** 2 + max(qy, 0) ** 2) ** 0.5
    return out + min(max(qx, qy), 0) - r


def in_poly(x, y, poly):
    inside = False
    for (x1, y1), (x2, y2) in zip(poly, poly[1:] + poly[:1]):
        if (y1 > y) != (y2 > y) and x < x1 + (y - y1) * (x2 - x1) / (y2 - y1):
            inside = not inside
    return inside


def color_at(x, y, ticks):
    d = rounded_rect_dist(x, y, 30, 14)  # tile: x 2..62, radius 14, stroke 1.5 centred
    if d > 0.75:
        return None
    if in_poly(x, y, LEFT_POLY):
        return LEFT
    if in_poly(x, y, RIGHT_POLY):
        return RIGHT
    if any(a <= x <= c and b <= y <= e for a, b, c, e in ticks):
        return MARK
    return TILE_EDGE if d > -0.75 else TILE


def render(name, size):
    ss = 8 if size <= 64 else 3
    ticks, step = TICKS[name], 64 / (size * ss)
    rows = []
    for py in range(size):
        row = bytearray([0])
        for px in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(ss):
                for sx in range(ss):
                    c = color_at((px * ss + sx + 0.5) * step, (py * ss + sy + 0.5) * step, ticks)
                    if c:
                        r, g, b = rgb(c)
                        acc[0] += r; acc[1] += g; acc[2] += b; acc[3] += 1
            n = acc[3]
            row += bytes([acc[0] // n, acc[1] // n, acc[2] // n, n * 255 // (ss * ss)] if n else [0, 0, 0, 0])
        rows.append(bytes(row))
    return png(size, b"".join(rows))


def png(size, raw):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(pngs):
    head = struct.pack("<HHH", 0, 1, len(pngs))
    offset, entries, blobs = 6 + 16 * len(pngs), b"", b""
    for size, data in pngs:
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    return head + entries + blobs


def svg(name):
    poly = lambda p: " ".join(f"{x},{y}" for x, y in p)
    marks = "".join(f'<rect x="{a}" y="{b}" width="{c - a}" height="{e - b}" fill="{MARK}"/>' for a, b, c, e in TICKS[name])
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">'
            f'<rect x="2" y="2" width="60" height="60" rx="14" fill="{TILE}" stroke="{TILE_EDGE}" stroke-width="1.5"/>'
            f'<polygon points="{poly(LEFT_POLY)}" fill="{LEFT}"/><polygon points="{poly(RIGHT_POLY)}" fill="{RIGHT}"/>'
            f"{marks}</svg>\n")


if __name__ == "__main__":
    assert in_poly(20, 20, LEFT_POLY) and not in_poly(32, 20, LEFT_POLY)
    assert color_at(0, 0, []) is None and color_at(32, 4, []) == TILE
    OUT.mkdir(parents=True, exist_ok=True)
    for name in TICKS:
        (OUT / f"{name}.svg").write_text(svg(name), encoding="utf-8")
        pngs = {s: render(name, s) for s in SIZES}
        for s, data in pngs.items():
            (OUT / f"{name}-{s}.png").write_bytes(data)
        (OUT / f"{name}.ico").write_bytes(ico([(s, pngs[s]) for s in ICO_SIZES]))
        print("wrote", name)
