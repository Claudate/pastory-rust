#!/usr/bin/env python3
"""App Store 截图合成。

面板本体是宽扁条（1600×450 等），直接拉伸到 2880×1800 会失真，所以按
「macOS 桌面实景 + 面板贴入」构图：先铺一层桌面壁纸，再把自测渲染出的
真实面板 PNG 按原比例贴上去，加投影。面板像素全部来自 --selftest 渲染，
不重绘、不改内容。

产物：store-assets/screenshots/{zh,en}/01..05.png，均为 2880×1800。
"""
import os
from PIL import Image, ImageDraw, ImageFilter

W, H = 2880, 1800
HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "raw")
OUT = os.path.join(HERE, "screenshots")


def desktop(lang, variant=0):
    """一块安静的桌面底：竖直渐变 + 两团柔光，避免纯色显得像 PPT。"""
    palettes = [
        ((28, 34, 52), (12, 14, 24), (72, 96, 168), (150, 92, 130)),
        ((238, 240, 245), (206, 212, 224), (168, 196, 244), (232, 196, 214)),
        ((22, 26, 30), (10, 12, 16), (54, 92, 104), (96, 74, 54)),
    ]
    top, bottom, glow_a, glow_b = palettes[variant % len(palettes)]
    img = Image.new("RGB", (W, H))
    d = ImageDraw.Draw(img)
    for y in range(H):
        t = y / (H - 1)
        d.line([(0, y), (W, y)], fill=tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)))

    glow = Image.new("RGB", (W, H), (0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse([-500, 200, 1500, 1900], fill=glow_a)
    gd.ellipse([1500, -600, 3400, 1100], fill=glow_b)
    glow = glow.filter(ImageFilter.GaussianBlur(260))
    img = Image.blend(img, Image.blend(img, glow, 0.55), 0.42)
    return img


def shadow_paste(canvas, panel, x, y, blur=46, offset=26, alpha=118):
    """把面板贴上去，先画一层模糊的黑影。"""
    mask = Image.new("L", panel.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, panel.size[0] - 1, panel.size[1] - 1], radius=22, fill=255
    )
    sh = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    sd = ImageDraw.Draw(sh)
    sd.rounded_rectangle(
        [x, y + offset, x + panel.size[0], y + offset + panel.size[1]],
        radius=22, fill=(0, 0, 0, alpha),
    )
    sh = sh.filter(ImageFilter.GaussianBlur(blur))
    canvas.paste(sh, (0, 0), sh)
    canvas.paste(panel, (x, y), mask)


def rounded(im, radius=22):
    im = im.convert("RGBA")
    mask = Image.new("L", im.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, im.size[0] - 1, im.size[1] - 1], radius=radius, fill=255
    )
    im.putalpha(mask)
    return im


def place_center(panel, scale, y_center, canvas, blur=46, offset=26, alpha=112):
    p = panel.copy()
    if scale != 1.0:
        p = p.resize((int(p.size[0] * scale), int(p.size[1] * scale)), Image.LANCZOS)
    p = rounded(p)
    x = (W - p.size[0]) // 2
    y = int(y_center - p.size[1] / 2)
    shadow_paste(canvas, p, x, y, blur=blur, offset=offset, alpha=alpha)
    return p


def build(lang, out_name, raw_name, variant, caption_bg=None, scale=1.0, y_center=None):
    panel = Image.open(os.path.join(RAW, raw_name)).convert("RGB")
    canvas = desktop(lang, variant).convert("RGB")
    if y_center is None:
        y_center = H // 2
    place_center(panel, scale, y_center, canvas)
    path = os.path.join(OUT, lang, out_name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    canvas.save(path, "PNG")
    return path, panel.size


def main():
    jobs = [
        # (文件名, 原始面板, 配色, 目标宽度px, 垂直中心)
        ("01-clipboard.png", "shelf-{L}.png", 0, 2560, 900),
        ("02-search.png", "search-{L}.png", 2, 2560, 900),
        ("03-settings.png", "settings-{L}.png", 1, 2560, 900),
        ("04-annotate.png", "annotate-{L}.png", 2, 1440, 900),
        # 第二形态（晾衣绳）：4.2 差异化的视觉证据，整屏宽横条。
        ("05-clothesline.png", "line-{L}.png", 2, 2880, 760),
    ]
    for lang in ("zh", "en"):
        for name, raw, variant, target_w, yc in jobs:
            r = raw.format(L=lang)
            src = os.path.join(RAW, r)
            if not os.path.exists(src):
                print("skip (missing)", src)
                continue
            w0 = Image.open(src).size[0]
            p, size = build(lang, name, r, variant, scale=target_w / w0, y_center=yc)
            print(f"{p}  ← {r} @ {target_w / w0:.2f}x  {size}")


if __name__ == "__main__":
    main()
