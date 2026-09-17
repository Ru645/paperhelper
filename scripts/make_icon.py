#!/usr/bin/env python3
"""生成 assets/ 下的图标资源（纯标准库，无需 Pillow 等第三方依赖）。

产物：
  assets/icon.ico       多尺寸图标（16/32/48/64/128/256），供 NSIS 安装包/快捷方式使用
  assets/icon.png       256x256 PNG，供文档/README 使用
  assets/icon_32.rgba   32x32 原始 RGBA（4096 字节），供桌面壳 tao::Icon::from_rgba 使用

用法：python3 scripts/make_icon.py
"""

import math
import os
import struct
import sys
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")

BASE = 256          # 逻辑尺寸
SS = 4              # 超采样倍数（先画 1024 再缩，得到抗锯齿边缘）
S = BASE * SS

INDIGO = (79, 70, 229)      # #4F46E5
VIOLET = (124, 58, 237)     # #7C3AED
WHITE = (255, 255, 255)
FOLD = (199, 210, 254)      # #C7D2FE
LINE = (99, 102, 241)       # #6366F1
LINE_LIGHT = (165, 180, 252)  # #A5B4FC
SHADOW = (49, 46, 129)      # 页面投影


def blend(dst, idx, color, alpha):
    """把 color 以 alpha(0..1) 叠加到 dst 的 idx 处（RGBA）。"""
    if alpha <= 0:
        return
    if alpha > 1:
        alpha = 1.0
    inv = 1.0 - alpha
    for k in range(3):
        dst[idx + k] = int(dst[idx + k] * inv + color[k] * alpha + 0.5)
    dst[idx + 3] = int(dst[idx + 3] * inv + 255 * alpha + 0.5)


def rounded_rect(c, x0, y0, x1, y1, r, color, alpha=1.0):
    """填充圆角矩形（坐标为首尾边界，逻辑像素；内部乘超采样倍数）。"""
    x0, y0, x1, y1, r = (v * SS for v in (x0, y0, x1, y1, r))
    for py in range(max(0, int(y0)), min(S, int(math.ceil(y1)))):
        cy = min(max(py + 0.5, y0 + r), y1 - r)
        for px in range(max(0, int(x0)), min(S, int(math.ceil(x1)))):
            cx = min(max(px + 0.5, x0 + r), x1 - r)
            dx, dy = px + 0.5 - cx, py + 0.5 - cy
            if dx * dx + dy * dy <= r * r:
                blend(c, (py * S + px) * 4, color, alpha)


def gradient_bg(c, x0, y0, x1, y1, r, c0, c1):
    """圆角矩形 + 沿对角线渐变。"""
    x0s, y0s, x1s, y1s, rs = (v * SS for v in (x0, y0, x1, y1, r))
    span = max(1.0, (x1s - x0s) + (y1s - y0s))
    for py in range(int(y0s), int(math.ceil(y1s))):
        cy = min(max(py + 0.5, y0s + rs), y1s - rs)
        for px in range(int(x0s), int(math.ceil(x1s))):
            cx = min(max(px + 0.5, x0s + rs), x1s - rs)
            dx, dy = px + 0.5 - cx, py + 0.5 - cy
            if dx * dx + dy * dy > rs * rs:
                continue
            t = ((px + 0.5 - x0s) + (py + 0.5 - y0s)) / span
            col = tuple(int(c0[k] + (c1[k] - c0[k]) * t + 0.5) for k in range(3))
            blend(c, (py * S + px) * 4, col, 1.0)


def triangle(c, p0, p1, p2, color, alpha=1.0):
    """填充三角形（逻辑坐标）。"""
    pts = [(x * SS, y * SS) for x, y in (p0, p1, p2)]
    miny = max(0, int(min(p[1] for p in pts)))
    maxy = min(S, int(math.ceil(max(p[1] for p in pts))))
    minx = max(0, int(min(p[0] for p in pts)))
    maxx = min(S, int(math.ceil(max(p[0] for p in pts))))

    def sign(ax, ay, bx, by, cx_, cy):
        return (ax - cx_) * (by - cy) - (bx - cx_) * (ay - cy)

    (ax, ay), (bx, by), (cx_, cy) = pts
    area = sign(ax, ay, bx, by, cx_, cy)
    if abs(area) < 1e-9:
        return
    for py in range(miny, maxy):
        for px in range(minx, maxx):
            x, y = px + 0.5, py + 0.5
            d1 = sign(x, y, ax, ay, bx, by)
            d2 = sign(x, y, bx, by, cx_, cy)
            d3 = sign(x, y, cx_, cy, ax, ay)
            neg = (d1 < 0) or (d2 < 0) or (d3 < 0)
            pos = (d1 > 0) or (d2 > 0) or (d3 > 0)
            if not (neg and pos):
                blend(c, (py * S + px) * 4, color, alpha)


def draw_base():
    """画超采样底图：圆角渐变底 + 白色纸张 + 折角 + 文字线 + 投影。"""
    c = bytearray(S * S * 4)
    gradient_bg(c, 0, 0, BASE, BASE, 56, INDIGO, VIOLET)

    # 纸张投影与纸面
    rounded_rect(c, 70, 52, 196, 216, 16, SHADOW, 0.22)
    rounded_rect(c, 66, 44, 190, 208, 16, WHITE, 1.0)

    # 右上折角
    triangle(c, (158, 44), (190, 44), (190, 76), FOLD)
    triangle(c, (158, 44), (158, 62), (176, 44), FOLD)  # 折痕阴影，制造层次

    # 文字线（最后一条更短更浅）
    lines = [
        (88, 96, 168, 10, LINE),
        (88, 122, 168, 10, LINE),
        (88, 148, 158, 10, LINE),
        (88, 174, 168, 10, LINE_LIGHT),
    ]
    for x, y, x1, h, col in lines:
        rounded_rect(c, x, y, x1, y + h, h / 2, col, 1.0)
    return c


def resize_area(src, sw, sh, dw, dh):
    """面积平均缩放（RGBA）。"""
    if sw == dw and sh == dh:
        return bytes(src)
    out = bytearray(dw * dh * 4)
    for dy in range(dh):
        sy0 = dy * sh / dh
        sy1 = (dy + 1) * sh / dh
        y0, y1 = int(math.floor(sy0)), int(math.ceil(sy1))
        for dx in range(dw):
            sx0 = dx * sw / dw
            sx1 = (dx + 1) * sw / dw
            x0, x1 = int(math.floor(sx0)), int(math.ceil(sx1))
            r = g = b = a = 0.0
            wsum = 0.0
            for yy in range(y0, min(y1, sh)):
                wy = min(sy1, yy + 1) - max(sy0, yy)
                if wy <= 0:
                    continue
                for xx in range(x0, min(x1, sw)):
                    wx = min(sx1, xx + 1) - max(sx0, xx)
                    if wx <= 0:
                        continue
                    w = wx * wy
                    i = (yy * sw + xx) * 4
                    r += src[i] * w
                    g += src[i + 1] * w
                    b += src[i + 2] * w
                    a += src[i + 3] * w
                    wsum += w
            o = (dy * dw + dx) * 4
            if wsum > 0:
                out[o] = int(r / wsum + 0.5)
                out[o + 1] = int(g / wsum + 0.5)
                out[o + 2] = int(b / wsum + 0.5)
                out[o + 3] = int(a / wsum + 0.5)
    return bytes(out)


def png_bytes(w, h, rgba):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + rgba[y * w * 4:(y + 1) * w * 4] for y in range(h))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def bmp_payload(w, h, rgba):
    """ICO 内嵌的 BMP（BITMAPINFOHEADER + 自下而上的 BGRA + AND 掩码）。"""
    bih = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, w * h * 4, 0, 0, 0, 0)
    xor = bytearray()
    for y in range(h - 1, -1, -1):
        row = rgba[y * w * 4:(y + 1) * w * 4]
        for x in range(w):
            i = x * 4
            xor += bytes((row[i + 2], row[i + 1], row[i], row[i + 3]))
    stride = ((w + 31) // 32) * 4
    and_mask = bytearray()
    for y in range(h - 1, -1, -1):
        row = bytearray(stride)
        for x in range(w):
            if rgba[(y * w + x) * 4 + 3] == 0:
                row[x // 8] |= 0x80 >> (x % 8)
        and_mask += row
    return bih + bytes(xor) + bytes(and_mask)


def ico_bytes(images):
    header = struct.pack("<HHH", 0, 1, len(images))
    entries = b""
    data = b""
    offset = 6 + 16 * len(images)
    for size, payload in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(payload), offset)
        data += payload
        offset += len(payload)
    return header + entries + data


def main():
    os.makedirs(ASSETS, exist_ok=True)
    base = draw_base()
    sizes = [16, 32, 48, 64, 128, 256]
    images = []
    for size in sizes:
        rgba = resize_area(base, S, S, size, size)
        images.append((size, bmp_payload(size, size, rgba)))
        if size == 32:
            with open(os.path.join(ASSETS, "icon_32.rgba"), "wb") as f:
                f.write(rgba)
        if size == 256:
            with open(os.path.join(ASSETS, "icon.png"), "wb") as f:
                f.write(png_bytes(size, size, rgba))
    ico = ico_bytes(images)
    with open(os.path.join(ASSETS, "icon.ico"), "wb") as f:
        f.write(ico)
    print(f"已生成 assets/icon.ico（{len(ico)} 字节，尺寸 {sizes}）")
    print("已生成 assets/icon.png（256x256）与 assets/icon_32.rgba（4096 字节）")


if __name__ == "__main__":
    sys.exit(main())
