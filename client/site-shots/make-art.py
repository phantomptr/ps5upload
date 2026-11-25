"""Made-up artwork for the website's screenshots (site-shots/): box art for fictional games and
one stream picture. Everything is drawn here with shapes, gradients and lettering, so no real
game, cover, logo or person appears in the screenshots, and nothing is downloaded.
Run: python3 site-shots/make-art.py (needs Pillow)."""

import math
import random
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

OUT = Path(__file__).parent / "art"
OUT.mkdir(exist_ok=True)

S = 1024  # covers are drawn at twice their saved size, then scaled down for smooth edges
RGB = tuple[int, int, int]

FONT_DIRS = ("/System/Library/Fonts/Supplemental/", "/System/Library/Fonts/", "/Library/Fonts/")
FALLBACK = (
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
)


def font(name: str, size: int, index: int = 0) -> ImageFont.FreeTypeFont:
    """A system font by file name; any bold sans where that font is missing."""
    for d in FONT_DIRS:
        try:
            return ImageFont.truetype(d + name, size, index=index)
        except OSError:
            continue
    for f in FALLBACK:
        try:
            return ImageFont.truetype(f, size)
        except OSError:
            continue
    return ImageFont.load_default(size)


def rgb(h: str) -> RGB:
    h = h.lstrip("#")
    return (int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16))


def mix(a: RGB, b: RGB, t: float) -> RGB:
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))  # type: ignore[return-value]


def sky(w: int, h: int, stops: list[tuple[float, str]]) -> Image.Image:
    """A top-to-bottom gradient through colour stops at 0..1."""
    col = Image.new("RGB", (1, h))
    px = col.load()
    pts = [(p, rgb(c)) for p, c in stops]
    for y in range(h):
        t = y / (h - 1)
        for (p0, c0), (p1, c1) in zip(pts, pts[1:]):
            if p0 <= t <= p1:
                px[0, y] = mix(c0, c1, (t - p0) / max(1e-6, p1 - p0))
                break
        else:
            px[0, y] = pts[-1][1]
    return col.resize((w, h))


def layer(img: Image.Image) -> tuple[Image.Image, ImageDraw.ImageDraw]:
    lay = Image.new("RGBA", img.size, (0, 0, 0, 0))
    return lay, ImageDraw.Draw(lay)


def put(img: Image.Image, lay: Image.Image, blur: float = 0) -> Image.Image:
    if blur:
        lay = lay.filter(ImageFilter.GaussianBlur(blur))
    out = img.convert("RGBA")
    out.alpha_composite(lay)
    return out.convert("RGB")


def glow(img: Image.Image, xy: tuple[float, float], r: float, color: RGB, alpha: int = 170, blur: float | None = None) -> Image.Image:
    lay, d = layer(img)
    x, y = xy
    d.ellipse([x - r, y - r, x + r, y + r], fill=(*color, alpha))
    return put(img, lay, blur if blur is not None else r * 0.6)


def ridge(w: int, base: float, amp: float, rnd: random.Random, step: int = 8, sharp: float = 0.0) -> list[tuple[float, float]]:
    """A skyline of hills: a few sine waves added up (sharp > 0 makes peaks)."""
    waves = [(rnd.uniform(0.004, 0.02), rnd.uniform(0, 6.3), rnd.uniform(0.3, 1.0)) for _ in range(4)]
    norm = sum(a for _, _, a in waves)
    pts = []
    for x in range(0, w + step, step):
        v = sum(math.sin(x * f + p) * a for f, p, a in waves) / norm
        if sharp:
            v = 1 - abs(v) ** (1 - sharp * 0.5) * 2
        pts.append((x, base - v * amp))
    return pts


def hills(img: Image.Image, pts: list[tuple[float, float]], color: RGB, bottom: int | None = None) -> None:
    w, h = img.size
    ImageDraw.Draw(img).polygon([(0, bottom or h), *pts, (w, bottom or h)], fill=color)


def stars(img: Image.Image, rnd: random.Random, n: int, below: float = 0.6, color: RGB = (255, 255, 255)) -> Image.Image:
    lay, d = layer(img)
    w, h = img.size
    for _ in range(n):
        x, y, r = rnd.uniform(0, w), rnd.uniform(0, h * below), rnd.choice((1.5, 1.5, 2, 2.5, 3.5))
        d.ellipse([x - r, y - r, x + r, y + r], fill=(*color, rnd.randint(110, 255)))
    return put(img, lay)


def clouds(img: Image.Image, rnd: random.Random, n: int, band: tuple[float, float], color: RGB, alpha: int = 200) -> Image.Image:
    lay, d = layer(img)
    w, h = img.size
    for _ in range(n):
        cx, cy, s = rnd.uniform(0, w), rnd.uniform(band[0], band[1]) * h, rnd.uniform(60, 150)
        for k in range(6):
            x, y, r = cx + (k - 2.5) * s * 0.45, cy + rnd.uniform(-0.12, 0.12) * s, s * rnd.uniform(0.35, 0.6)
            d.ellipse([x - r, y - r * 0.7, x + r, y + r * 0.7], fill=(*color, alpha))
        d.rounded_rectangle([cx - s * 1.5, cy, cx + s * 1.5, cy + s * 0.32], radius=s * 0.16, fill=(*color, alpha))
    return put(img, lay, 5)


def finish(img: Image.Image, rnd: random.Random, shade: float = 0.5) -> Image.Image:
    """What print does to a cover: darker corners and a little grain."""
    w, h = img.size
    v = Image.radial_gradient("L").resize((w, h))
    dark = Image.new("RGB", (w, h), (0, 0, 0))
    img = Image.composite(dark, img, v.point(lambda p: int(max(0, p - 110) * shade)))
    noise = Image.effect_noise((w // 2, h // 2), 26).resize((w, h)).convert("RGB")
    return Image.blend(img, ImageChops.overlay(img, noise), 0.1)


def lettering(
    img: Image.Image,
    lines: list[str],
    face: tuple[str, int],
    size: int,
    y: float,
    fill: RGB = (255, 255, 255),
    track: float = 0.0,
    shadow: RGB | None = (0, 0, 0),
    stroke: tuple[int, RGB] | None = None,
    halo: RGB | None = None,
    gap: float = 1.02,
    fit: float = 0.86,
) -> Image.Image:
    """The title, set like a logo: its own face, letter-spacing, a soft shadow or a neon halo."""
    w, _ = img.size
    f = font(face[0], size, face[1])

    def width(text: str, f: ImageFont.FreeTypeFont, sp: float) -> float:
        return sum(f.getlength(c) for c in text) + sp * (len(text) - 1)

    widest = max(lines, key=lambda t: width(t, f, track * size))
    while width(widest, f, track * size) > w * fit and size > 20:
        size -= 4
        f = font(face[0], size, face[1])
    sp = track * size

    def draw(d: ImageDraw.ImageDraw, color: tuple[int, ...], dx: float = 0, dy: float = 0, sw: int = 0, sc: tuple[int, ...] | None = None) -> None:
        for i, text in enumerate(lines):
            x = (w - width(text, f, sp)) / 2 + dx
            for c in text:
                d.text((x, y + i * size * gap + dy), c, font=f, fill=color, stroke_width=sw, stroke_fill=sc)
                x += f.getlength(c) + sp

    if halo:
        lay, d = layer(img)
        draw(d, (*halo, 255), sw=6, sc=(*halo, 255))
        img = put(img, lay, 14)
    if shadow is not None:
        lay, d = layer(img)
        draw(d, (*shadow, 190), dy=size * 0.06, sw=2, sc=(*shadow, 190))
        img = put(img, lay, size * 0.07)
    lay, d = layer(img)
    if stroke:
        draw(d, (*fill, 255), sw=stroke[0], sc=(*stroke[1], 255))
    else:
        draw(d, (*fill, 255))
    return put(img, lay)


def small_caps(img: Image.Image, text: str, y: float, fill: RGB = (255, 255, 255), size: int = 30, alpha: int = 220) -> Image.Image:
    """A line under the title: a tagline, in spaced capitals."""
    return lettering(img, [text], ("Avenir Next Condensed.ttc", 2), size, y, fill=mix((0, 0, 0), fill, alpha / 255), track=0.32, shadow=(0, 0, 0))


# ── The fourteen covers ─────────────────────────────────────────────────────────────────────


def neon_drift(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#12063a"), (0.38, "#5b1a7a"), (0.56, "#ff4f8b"), (0.6, "#1a0b3a"), (1, "#0b0420")])
    img = stars(img, rnd, 90, 0.4)
    hz = S * 0.58
    img = glow(img, (S / 2, hz - 60), 300, (255, 120, 90), 150)
    sun, d = layer(img)
    d.ellipse([S / 2 - 210, hz - 330, S / 2 + 210, hz + 90], fill=(255, 205, 110, 255))
    for i in range(9):
        y = hz - 150 + i * 30
        d.rectangle([0, y, S, y + 5 + i * 2.2], fill=(0, 0, 0, 0))
    d.rectangle([0, hz, S, S], fill=(0, 0, 0, 0))
    img = put(img, sun)
    hills(img, ridge(S, hz - 10, 70, rnd, sharp=0.9), rgb("#2a0f55"), int(hz) + 2)
    d = ImageDraw.Draw(img)
    for i in range(1, 16):
        y = hz + (i / 15) ** 2.1 * (S - hz)
        d.line([(0, y), (S, y)], fill=(255, 70, 170), width=3)
    for k in range(-12, 13):
        d.line([(S / 2 + k * 14, hz), (S / 2 + k * 190, S)], fill=(255, 70, 170), width=3)
    d.polygon([(S / 2 - 26, hz), (S / 2 + 26, hz), (S / 2 + 330, S), (S / 2 - 330, S)], fill=rgb("#150a30"))
    for i in range(7):
        t0, t1 = (i / 7) ** 2, ((i + 0.45) / 7) ** 2
        y0, y1 = hz + t0 * (S - hz), hz + t1 * (S - hz)
        d.polygon([(S / 2 - 3 - t0 * 14, y0), (S / 2 + 3 + t0 * 14, y0), (S / 2 + 3 + t1 * 14, y1), (S / 2 - 3 - t1 * 14, y1)], fill=(120, 240, 255))
    # The car, from behind.
    cx, cy = S / 2 + 40, S * 0.83
    img = glow(img, (cx, cy + 46), 150, (0, 230, 255), 110, 30)
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([cx - 150, cy - 30, cx + 150, cy + 50], radius=22, fill=rgb("#1b1238"))
    d.polygon([(cx - 112, cy - 28), (cx - 78, cy - 86), (cx + 78, cy - 86), (cx + 112, cy - 28)], fill=rgb("#2b1c58"))
    d.polygon([(cx - 92, cy - 34), (cx - 68, cy - 76), (cx + 68, cy - 76), (cx + 92, cy - 34)], fill=rgb("#7ee8ff"))
    d.rounded_rectangle([cx - 140, cy - 6, cx - 62, cy + 12], radius=8, fill=(255, 60, 120))
    d.rounded_rectangle([cx + 62, cy - 6, cx + 140, cy + 12], radius=8, fill=(255, 60, 120))
    d.rectangle([cx - 150, cy + 44, cx - 104, cy + 70], fill=(10, 6, 22))
    d.rectangle([cx + 104, cy + 44, cx + 150, cy + 70], fill=(10, 6, 22))
    img = finish(img, rnd)
    img = lettering(img, ["NEON", "DRIFT"], ("Futura.ttc", 4), 172, 104, fill=(255, 255, 255), track=0.04, halo=(255, 60, 170), shadow=None, gap=0.9)
    return img


def skyward_isles(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#2f7fd6"), (0.55, "#9fd8ff"), (0.85, "#fff0d0"), (1, "#ffd9a8")])
    img = glow(img, (S * 0.78, S * 0.2), 190, (255, 250, 220), 200)
    img = clouds(img, rnd, 5, (0.12, 0.45), (255, 255, 255), 150)

    def isle(cx: float, cy: float, r: float, tone: float) -> None:
        nonlocal img
        d = ImageDraw.Draw(img)
        rock, rock2 = mix(rgb("#6b4a3a"), rgb("#b9cfe8"), tone), mix(rgb("#4a3228"), rgb("#9fbbe0"), tone)
        grass, grass2 = mix(rgb("#4fae4a"), rgb("#bfe3ef"), tone), mix(rgb("#2f8a3c"), rgb("#a6d3ea"), tone)
        pts = [(cx - r, cy)]
        n = 9
        for i in range(1, n):
            t = i / n
            depth = math.sin(t * math.pi) ** 0.7 * r * rnd.uniform(0.9, 1.5)
            pts.append((cx - r + 2 * r * t, cy + depth * (0.6 + 0.5 * (i % 2))))
        pts.append((cx + r, cy))
        d.polygon(pts, fill=rock)
        d.polygon([p for p in pts if p[0] >= cx - r * 0.1] + [(cx, cy)], fill=rock2)
        d.ellipse([cx - r, cy - r * 0.26, cx + r, cy + r * 0.26], fill=grass2)
        d.ellipse([cx - r, cy - r * 0.32, cx + r, cy + r * 0.18], fill=grass)
        for _ in range(int(r / 22)):
            x, th = cx + rnd.uniform(-0.7, 0.7) * r, rnd.uniform(0.28, 0.5) * r
            d.rectangle([x - 3, cy - r * 0.12 - th * 0.4, x + 3, cy - r * 0.08], fill=mix(rgb("#5a3b22"), rgb("#b9cfe8"), tone))
            d.ellipse([x - th * 0.38, cy - r * 0.12 - th, x + th * 0.38, cy - r * 0.12 - th * 0.3], fill=mix(rgb("#237a3a"), rgb("#a6d3ea"), tone))
        if tone < 0.2:
            fall, fd = layer(img)
            fd.rectangle([cx + r * 0.36, cy + r * 0.1, cx + r * 0.41, cy + r * 1.5], fill=(240, 252, 255, 170))
            img = put(img, fall, 3)

    isle(S * 0.18, S * 0.54, 80, 0.72)
    isle(S * 0.86, S * 0.58, 110, 0.55)
    isle(S * 0.42, S * 0.68, 250, 0.0)
    img = clouds(img, rnd, 4, (0.86, 0.99), (255, 255, 255), 235)
    d = ImageDraw.Draw(img)
    for _ in range(6):
        x, y, s = rnd.uniform(0.1, 0.7) * S, rnd.uniform(0.3, 0.45) * S, rnd.uniform(8, 16)
        d.line([(x - s, y - s * 0.5), (x, y), (x + s, y - s * 0.5)], fill=(40, 60, 100), width=4)
    img = finish(img, rnd, 0.3)
    img = lettering(img, ["Skyward", "Isles"], ("Baskerville.ttc", 3), 150, 96, fill=(255, 255, 255), shadow=(20, 60, 130), gap=0.92)
    return img


def iron_harbor(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#16222e"), (0.4, "#3c4a55"), (0.6, "#f2a541"), (0.64, "#ffd58a"), (0.66, "#1b2a36"), (1, "#0d151c")])
    hz = int(S * 0.65)
    img = glow(img, (S * 0.3, hz - 10), 170, (255, 214, 140), 190)
    d = ImageDraw.Draw(img)
    for depth, tone in ((0, "#41505b"), (1, "#27333d")):
        x = -20
        while x < S:
            bw = rnd.randint(40, 110)
            bh = rnd.randint(90, 330) * (0.7 if depth == 0 else 1)
            d.rectangle([x, hz - bh, x + bw, hz], fill=rgb(tone))
            if depth:
                for wy in range(int(hz - bh) + 14, hz - 12, 24):
                    for wx in range(x + 10, x + bw - 12, 20):
                        if rnd.random() > 0.55:
                            d.rectangle([wx, wy, wx + 9, wy + 12], fill=(255, 220, 140))
            x += bw + rnd.randint(2, 18)
    # Two cranes over the quay.
    for cx, hgt, arm in ((S * 0.66, 400, 250), (S * 0.86, 330, -200)):
        steel = rgb("#10181f")
        d.polygon([(cx - 26, hz), (cx - 8, hz - hgt), (cx + 8, hz - hgt), (cx + 26, hz)], outline=steel, width=9)
        for k in range(6):
            y0, y1 = hz - k * hgt / 6, hz - (k + 1) * hgt / 6
            d.line([(cx - 26 + k * 3, y0), (cx + 8, y1)], fill=steel, width=5)
        d.line([(cx - arm * 0.3, hz - hgt), (cx + arm, hz - hgt - 26)], fill=steel, width=12)
        d.line([(cx, hz - hgt - 70), (cx + arm, hz - hgt - 26)], fill=steel, width=4)
        d.line([(cx, hz - hgt - 70), (cx, hz - hgt)], fill=steel, width=8)
        d.line([(cx + arm * 0.8, hz - hgt - 22), (cx + arm * 0.8, hz - hgt + 150)], fill=steel, width=3)
        d.rectangle([cx + arm * 0.8 - 26, hz - hgt + 150, cx + arm * 0.8 + 26, hz - hgt + 184], fill=rgb("#c1492e"))
    # A freighter and its reflection.
    sx, sy = S * 0.36, hz + 96
    d.polygon([(sx - 250, sy - 40), (sx + 270, sy - 40), (sx + 220, sy + 34), (sx - 210, sy + 34)], fill=rgb("#0c1319"))
    d.rectangle([sx - 250, sy - 46, sx + 270, sy - 38], fill=rgb("#c1492e"))
    for i, c in enumerate(("#c1492e", "#2d6f8e", "#e0a13a", "#5b6b76", "#2d6f8e", "#c1492e")):
        d.rectangle([sx - 180 + i * 62, sy - 84 - (i % 2) * 38, sx - 122 + i * 62, sy - 46], fill=rgb(c))
    d.rectangle([sx + 190, sy - 150, sx + 250, sy - 46], fill=rgb("#dfe6ea"))
    d.rectangle([sx + 204, sy - 190, sx + 226, sy - 150], fill=rgb("#c1492e"))
    ref, rd = layer(img)
    for i in range(46):
        y = hz + 8 + i * 8
        x0 = S * 0.3 + rnd.uniform(-90, 90) * (1 + i * 0.05)
        rd.line([(x0 - rnd.uniform(20, 120), y), (x0 + rnd.uniform(20, 120), y)], fill=(255, 205, 120, max(20, 190 - i * 4)), width=3)
    img = put(img, ref, 1.5)
    img = finish(img, rnd)
    img = lettering(img, ["IRON", "HARBOR"], ("DIN Condensed Bold.ttf", 0), 214, 96, fill=rgb("#f6efe2"), track=0.06, gap=0.86)
    return img


def lumen_trail(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#04140f"), (0.5, "#0d3b2a"), (0.78, "#1c6a45"), (1, "#06180f")])
    img = glow(img, (S * 0.5, S * 0.5), 260, (150, 255, 170), 120)
    for depth in range(3):
        d = ImageDraw.Draw(img)
        tone = mix(rgb("#1f6b47"), rgb("#020a07"), 0.35 + depth * 0.3)
        for _ in range(7 - depth):
            x, tw = rnd.uniform(-40, S), rnd.uniform(26, 44) * (1 + depth * 0.7)
            if depth == 2 and S * 0.3 < x < S * 0.62:
                continue
            d.polygon([(x, 0), (x + tw, 0), (x + tw * 1.25, S), (x - tw * 0.25, S)], fill=tone)
            for _ in range(3):
                by, bl, side = rnd.uniform(0.1, 0.55) * S, rnd.uniform(60, 170), rnd.choice((-1, 1))
                d.line([(x + tw / 2, by), (x + tw / 2 + side * bl, by - bl * 0.6)], fill=tone, width=int(8 + depth * 5))
        hills(img, ridge(S, S * (0.8 + depth * 0.06), 30, rnd), mix(rgb("#124a31"), rgb("#020a07"), 0.3 + depth * 0.3))
    path, pd = layer(img)
    pts = [(S * 0.5 + math.sin(t * 5.2) * (30 + t * 240), S * 0.58 + t * S * 0.44) for t in [i / 40 for i in range(41)]]
    for i in range(len(pts) - 1):
        pd.line([pts[i], pts[i + 1]], fill=(190, 255, 170, 255), width=int(6 + i * 1.5))
    img = put(img, path, 16)
    img = put(img, path.filter(ImageFilter.GaussianBlur(3)))
    lay, d = layer(img)
    for _ in range(70):
        x, y, r = rnd.uniform(0, S), rnd.uniform(0.3, 0.98) * S, rnd.uniform(2, 6)
        d.ellipse([x - r, y - r, x + r, y + r], fill=(220, 255, 170, rnd.randint(120, 255)))
    img = put(put(img, lay, 6), lay)
    img = finish(img, rnd, 0.7)
    img = lettering(img, ["LUMEN", "TRAIL"], ("Avenir Next.ttc", 10), 120, 116, fill=rgb("#e9ffd6"), track=0.34, halo=(120, 255, 150), gap=1.15)
    return img


def pocket_galaxy(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#0b0626"), (0.6, "#2a1560"), (1, "#5a2a8c")])
    for x, y, r, c in ((0.25, 0.3, 260, (255, 100, 200)), (0.8, 0.75, 300, (90, 160, 255)), (0.6, 0.2, 180, (140, 255, 230))):
        img = glow(img, (S * x, S * y), r, c, 70)
    img = stars(img, rnd, 260, 1.0)
    # The big planet, shaded, with a ring that passes behind it.
    cx, cy, r = S * 0.44, S * 0.62, 215
    back, bd = layer(img)
    bd.ellipse([cx - r * 1.9, cy - r * 0.42, cx + r * 1.9, cy + r * 0.42], outline=(255, 214, 150, 255), width=26)
    bd.ellipse([cx - r * 1.55, cy - r * 0.3, cx + r * 1.55, cy + r * 0.3], outline=(255, 170, 190, 220), width=10)
    back = back.rotate(-18, center=(cx, cy), resample=Image.BICUBIC)
    img = put(img, back)
    ball, d = layer(img)
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 150, 90, 255))
    for i, c in enumerate(("#ffb36b", "#f0704f", "#ffcf8f", "#e85d6a", "#ffb36b")):
        y = cy - r + 50 + i * 78
        d.rounded_rectangle([cx - r, y, cx + r, y + 34], radius=17, fill=(*rgb(c), 255))
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).ellipse([cx - r, cy - r, cx + r, cy + r], fill=255)
    shade, sd = layer(img)
    sd.ellipse([cx - r * 0.5, cy - r * 0.6, cx + r * 2.0, cy + r * 2.0], fill=(30, 10, 60, 150))
    ball = Image.alpha_composite(ball, shade.filter(ImageFilter.GaussianBlur(26)))
    ball.putalpha(mask)
    img = put(img, ball)
    front = Image.new("RGBA", img.size, (0, 0, 0, 0))
    front.paste(back, (0, 0), Image.new("L", img.size, 0))
    cut = Image.new("L", img.size, 0)
    ImageDraw.Draw(cut).rectangle([0, cy + 6, S, S], fill=255)
    cut = cut.rotate(-18, center=(cx, cy), resample=Image.BICUBIC)
    img = put(img, Image.composite(back, Image.new("RGBA", img.size, (0, 0, 0, 0)), cut))
    d = ImageDraw.Draw(img)
    for x, y, rr, c in ((0.83, 0.42, 46, "#7fe0d0"), (0.14, 0.86, 30, "#ffe08a"), (0.9, 0.9, 20, "#ff9ad0")):
        d.ellipse([S * x - rr, S * y - rr, S * x + rr, S * y + rr], fill=rgb(c))
        d.ellipse([S * x - rr * 0.2, S * y - rr * 0.9, S * x + rr, S * y + rr * 0.6], fill=mix(rgb(c), (30, 10, 60), 0.3))
    # A small rocket and its trail.
    rk, rd = layer(img)
    rd.polygon([(0, -70), (34, -14), (34, 46), (-34, 46), (-34, -14)], fill=(250, 250, 255, 255))
    rk2 = Image.new("RGBA", (200, 260), (0, 0, 0, 0))
    q = ImageDraw.Draw(rk2)
    q.polygon([(100, 250), (70, 170), (130, 170)], fill=(255, 190, 80, 255))
    q.polygon([(52, 176), (62, 120), (62, 176)], fill=(255, 90, 120, 255))
    q.polygon([(148, 176), (138, 120), (138, 176)], fill=(255, 90, 120, 255))
    q.rounded_rectangle([62, 60, 138, 178], radius=22, fill=(246, 246, 255, 255))
    q.pieslice([62, 10, 138, 120], 180, 360, fill=(255, 90, 120, 255))
    q.ellipse([82, 84, 118, 120], fill=(90, 190, 255, 255), outline=(60, 60, 110, 255), width=5)
    rk2 = rk2.rotate(-38, resample=Image.BICUBIC, expand=True)
    trail, td = layer(img)
    for i in range(14):
        x, y, rr = S * 0.74 - i * 21, S * 0.26 + i * 27, 9 + i * 2.2
        td.ellipse([x - rr, y - rr, x + rr, y + rr], fill=(255, 235, 200, max(0, 190 - i * 14)))
    img = put(img, trail, 4)
    out = img.convert("RGBA")
    out.alpha_composite(rk2, (int(S * 0.69), int(S * 0.07)))
    img = finish(out.convert("RGB"), rnd)
    img = lettering(img, ["Pocket", "Galaxy"], ("Arial Rounded Bold.ttf", 0), 138, S * 0.1, fill=rgb("#fff3c4"), stroke=(8, rgb("#3a1a7a")), shadow=(20, 6, 50), gap=0.98, fit=0.62)
    return img


def frostline(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#071426"), (0.5, "#17406b"), (0.8, "#7fb7d8"), (1, "#e6f6ff")])
    img = stars(img, rnd, 120, 0.5)
    aur, d = layer(img)
    for k, c in enumerate(((90, 255, 190), (120, 200, 255), (190, 140, 255))):
        pts = [(x, S * (0.2 + k * 0.07) + math.sin(x / 130 + k * 2) * 46) for x in range(0, S + 20, 20)]
        for x, y in pts:
            d.line([(x, y), (x + 14, y + 150 - k * 22)], fill=(*c, 105), width=22)
    img = put(img, aur, 16)
    img = glow(img, (S * 0.78, S * 0.2), 60, (255, 255, 240), 255, 5)
    for i, (base, amp, c) in enumerate(((0.62, 150, "#3f6f9a"), (0.74, 210, "#5f93bd"))):
        hills(img, ridge(S, S * base, amp, rnd, sharp=1.0), rgb(c))
    # The near peak, with a lit and a shaded face and a snow cap.
    d = ImageDraw.Draw(img)
    px, py = S * 0.46, S * 0.3
    d.polygon([(px, py), (S * 0.02, S), (S * 0.98, S)], fill=rgb("#27527c"))
    d.polygon([(px, py), (px + 30, S * 0.62), (px - 40, S), (S * 0.02, S)], fill=rgb("#b9dcf2"))
    d.polygon([(px, py), (px - 96, py + 180), (px - 50, py + 150), (px - 20, py + 220), (px + 12, py + 160), (px + 60, py + 230), (px + 92, py + 150), (px + 140, py + 230)], fill=rgb("#ffffff"))
    d.polygon([(px, py), (px + 12, py + 160), (px + 60, py + 230), (px + 92, py + 150), (px + 140, py + 230)], fill=rgb("#cfe6f5"))
    snow = ridge(S, S * 0.9, 26, rnd)
    hills(img, snow, rgb("#f4fbff"))
    for _ in range(16):
        x = rnd.uniform(0, S)
        y = S * 0.9 + rnd.uniform(-6, 40)
        h = rnd.uniform(60, 130)
        for k in range(3):
            d.polygon([(x, y - h + k * h * 0.22), (x - h * (0.16 + k * 0.07), y - h * 0.45 + k * h * 0.22), (x + h * (0.16 + k * 0.07), y - h * 0.45 + k * h * 0.22)], fill=rgb("#0f2f44"))
    img = finish(img, rnd, 0.4)
    img = lettering(img, ["FROSTLINE"], ("Avenir Next.ttc", 8), 124, 110, fill=(255, 255, 255), track=0.16, shadow=(8, 30, 60))
    img = small_caps(img, "THE LAST ASCENT", 250, size=32)
    return img


def ember_knights(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#120404"), (0.55, "#5a1208"), (0.85, "#d8481a"), (1, "#ffb347")])
    img = glow(img, (S / 2, S * 0.62), 330, (255, 140, 50), 170)
    hills(img, ridge(S, S * 0.82, 60, rnd, sharp=0.8), rgb("#2a0806"))
    d = ImageDraw.Draw(img)
    # A castle on the ridge.
    for x, wd, h in ((0.1, 60, 200), (0.2, 90, 150), (0.28, 50, 240), (0.72, 70, 190), (0.82, 100, 140), (0.9, 54, 220)):
        d.rectangle([S * x, S * 0.84 - h, S * x + wd, S * 0.9], fill=rgb("#1c0504"))
        for k in range(3):
            d.rectangle([S * x + k * wd / 3, S * 0.84 - h - 18, S * x + k * wd / 3 + wd / 5, S * 0.84 - h], fill=rgb("#1c0504"))
    # The shield, with two crossed swords behind it.
    cx, cy = S / 2, S * 0.64
    for ang in (-34, 34):
        sw = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        q = ImageDraw.Draw(sw)
        q.polygon([(cx - 15, cy + 250), (cx - 15, cy - 290), (cx, cy - 340), (cx + 15, cy - 290), (cx + 15, cy + 250)], fill=(236, 226, 214, 255))
        q.polygon([(cx, cy + 250), (cx, cy - 340), (cx + 15, cy - 290), (cx + 15, cy + 250)], fill=(176, 160, 150, 255))
        q.rounded_rectangle([cx - 80, cy + 240, cx + 80, cy + 266], radius=10, fill=(255, 196, 80, 255))
        q.rectangle([cx - 12, cy + 266, cx + 12, cy + 346], fill=(92, 40, 22, 255))
        q.ellipse([cx - 22, cy + 338, cx + 22, cy + 382], fill=(255, 196, 80, 255))
        sw = sw.rotate(ang, center=(cx, cy), resample=Image.BICUBIC)
        img = put(img, sw)
    d = ImageDraw.Draw(img)

    def shield(scale: float, fill: RGB) -> None:
        w, h = 190 * scale, 250 * scale
        d.polygon([(cx - w, cy - h * 0.72), (cx, cy - h * 0.9), (cx + w, cy - h * 0.72), (cx + w * 0.92, cy + h * 0.2), (cx, cy + h), (cx - w * 0.92, cy + h * 0.2)], fill=fill)

    shield(1.0, rgb("#ffc450"))
    shield(0.9, rgb("#7a140c"))
    d.polygon([(cx, cy - 202), (cx + 171, cy - 162), (cx + 157, cy + 45), (cx, cy + 225)], fill=rgb("#5a0d08"))
    # A flame on the shield.
    fl = [(cx, cy - 130), (cx + 64, cy - 24), (cx + 44, cy - 34), (cx + 80, cy + 60), (cx, cy + 130), (cx - 80, cy + 60), (cx - 40, cy - 30), (cx - 26, cy + 2)]
    d.polygon(fl, fill=rgb("#ffb347"))
    d.polygon([(cx, cy - 40), (cx + 36, cy + 56), (cx, cy + 104), (cx - 36, cy + 56)], fill=rgb("#fff0b8"))
    lay, d2 = layer(img)
    for _ in range(90):
        x, y, r = rnd.uniform(0, S), rnd.uniform(0.2, 1) * S, rnd.uniform(1.5, 5)
        d2.ellipse([x - r, y - r, x + r, y + r], fill=(255, rnd.randint(150, 230), 80, rnd.randint(120, 255)))
    img = put(put(img, lay, 5), lay)
    img = finish(img, rnd, 0.7)
    img = lettering(img, ["EMBER", "KNIGHTS"], ("Copperplate.ttc", 1), 132, 96, fill=rgb("#ffe2a8"), track=0.05, stroke=(3, rgb("#3a0a06")), gap=0.95)
    return img


def tidal_run(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#0aa3c8"), (0.42, "#8fe3ea"), (0.5, "#fff3c9"), (0.52, "#0f7fb0"), (1, "#052a55")])
    img = glow(img, (S * 0.24, S * 0.24), 110, (255, 250, 210), 255, 18)
    img = clouds(img, rnd, 3, (0.1, 0.3), (255, 255, 255), 190)
    d = ImageDraw.Draw(img)
    # A sail boat on the horizon line.
    bx, by = S * 0.7, S * 0.5
    d.polygon([(bx - 110, by), (bx + 120, by), (bx + 84, by + 40), (bx - 80, by + 40)], fill=rgb("#f6f1e6"))
    d.rectangle([bx - 110, by, bx + 120, by + 10], fill=rgb("#e2572d"))
    d.rectangle([bx - 4, by - 280, bx + 4, by], fill=rgb("#3a2a20"))
    d.polygon([(bx + 8, by - 270), (bx + 8, by - 20), (bx + 150, by - 20)], fill=rgb("#ffffff"))
    d.polygon([(bx - 8, by - 220), (bx - 8, by - 20), (bx - 110, by - 20)], fill=rgb("#ffb23e"))
    for i in range(8):
        y = S * (0.54 + i * 0.064)
        tone = mix(rgb("#19a0c9"), rgb("#053465"), i / 7)
        pts = [(x, y + math.sin(x / (60 + i * 9) + i * 1.9) * (10 + i * 3)) for x in range(0, S + 12, 12)]
        d.polygon([(0, S), *pts, (S, S)], fill=tone)
        d.line(pts, fill=mix(tone, (255, 255, 255), 0.75 - i * 0.05), width=5)
    # The big wave, curling in from the left.
    wave, wd = layer(img)
    wd.pieslice([-S * 0.42, S * 0.3, S * 0.5, S * 1.22], 215, 60, fill=(7, 92, 150, 255))
    wd.pieslice([-S * 0.3, S * 0.42, S * 0.38, S * 1.1], 200, 80, fill=(16, 140, 190, 255))
    wd.pieslice([-S * 0.2, S * 0.52, S * 0.28, S * 1.0], 190, 100, fill=(90, 205, 225, 255))
    img = put(img, wave)
    foam, fd = layer(img)
    for a in range(215, 420, 4):
        t = math.radians(a)
        x, y = S * 0.04 + math.cos(t) * S * 0.46, S * 0.76 + math.sin(t) * S * 0.46
        r = rnd.uniform(10, 26)
        fd.ellipse([x - r, y - r, x + r, y + r], fill=(255, 255, 255, 235))
    img = put(img, foam, 2)
    img = finish(img, rnd, 0.35)
    img = lettering(img, ["TIDAL RUN"], ("Futura.ttc", 2), 150, S * 0.83, fill=(255, 255, 255), track=0.02, shadow=(3, 30, 70))
    return img


def clockwork_garden(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#f4e7c8"), (0.6, "#e9d3a0"), (1, "#c9a56a")])
    img = glow(img, (S * 0.5, S * 0.45), 330, (255, 250, 225), 180)

    def gear(cx: float, cy: float, r: float, teeth: int, color: str, phase: float = 0) -> None:
        d = ImageDraw.Draw(img)
        c = rgb(color)
        for k in range(teeth):
            a = phase + k * 2 * math.pi / teeth
            w = math.pi / teeth * 0.55
            d.polygon(
                [(cx + math.cos(a - w) * r * 0.9, cy + math.sin(a - w) * r * 0.9), (cx + math.cos(a - w * 0.6) * r * 1.16, cy + math.sin(a - w * 0.6) * r * 1.16),
                 (cx + math.cos(a + w * 0.6) * r * 1.16, cy + math.sin(a + w * 0.6) * r * 1.16), (cx + math.cos(a + w) * r * 0.9, cy + math.sin(a + w) * r * 0.9)],
                fill=c,
            )
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=c)
        d.ellipse([cx - r * 0.72, cy - r * 0.72, cx + r * 0.72, cy + r * 0.72], fill=mix(c, (0, 0, 0), 0.22))
        for k in range(5):
            a = phase + k * 2 * math.pi / 5
            d.line([(cx, cy), (cx + math.cos(a) * r * 0.74, cy + math.sin(a) * r * 0.74)], fill=c, width=int(r * 0.14))
        d.ellipse([cx - r * 0.2, cy - r * 0.2, cx + r * 0.2, cy + r * 0.2], fill=mix(c, (255, 255, 255), 0.35))

    gear(S * 0.24, S * 0.86, 210, 16, "#b0763a", 0.1)
    gear(S * 0.72, S * 0.92, 170, 14, "#8a5a2b", 0.3)
    gear(S * 0.52, S * 0.7, 96, 10, "#d6a04a", 0.5)
    gear(S * 0.9, S * 0.62, 74, 9, "#b0763a", 0.2)
    d = ImageDraw.Draw(img)
    # Flowers growing out of the works: stems, leaves, petals.
    for fx, fy, top, petal, n in ((0.2, 0.7, 0.5, "#d9534f", 8), (0.5, 0.64, 0.46, "#2a9d8f", 10), (0.74, 0.74, 0.52, "#e9a23b", 7), (0.36, 0.76, 0.6, "#7a5cc0", 6), (0.88, 0.58, 0.48, "#d9534f", 6)):
        x0, y0, y1 = S * fx, S * fy, S * top
        sway = rnd.uniform(-50, 50)
        pts = [(x0 + math.sin(t * 3) * sway * t, y0 + (y1 - y0) * t) for t in [i / 20 for i in range(21)]]
        d.line(pts, fill=rgb("#2f6b4a"), width=9, joint="curve")
        for t in (0.35, 0.6):
            lx, ly = pts[int(t * 20)]
            side = 1 if t < 0.5 else -1
            d.polygon([(lx, ly), (lx + side * 62, ly - 30), (lx + side * 78, ly - 4), (lx + side * 40, ly + 14)], fill=rgb("#3f8a5f"))
        hx, hy = pts[-1]
        pr = 40 + n * 2
        for k in range(n):
            a = k * 2 * math.pi / n
            px, py = hx + math.cos(a) * pr * 0.8, hy + math.sin(a) * pr * 0.8
            d.ellipse([px - pr * 0.46, py - pr * 0.46, px + pr * 0.46, py + pr * 0.46], fill=rgb(petal))
        d.ellipse([hx - pr * 0.38, hy - pr * 0.38, hx + pr * 0.38, hy + pr * 0.38], fill=rgb("#ffe9a8"))
        d.ellipse([hx - pr * 0.16, hy - pr * 0.16, hx + pr * 0.16, hy + pr * 0.16], fill=rgb("#8a5a2b"))
    img = finish(img, rnd, 0.35)
    img = lettering(img, ["Clockwork", "Garden"], ("Didot.ttc", 2), 128, 100, fill=rgb("#3a2a18"), shadow=None, gap=0.98)
    return img


def lantern_marsh(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#04121a"), (0.5, "#0d2f3a"), (0.62, "#1d4a4f"), (0.64, "#071a20"), (1, "#020a0d")])
    img = stars(img, rnd, 70, 0.45)
    mx, my = S * 0.8, S * 0.4
    img = glow(img, (mx, my), 130, (210, 235, 225), 90)
    d = ImageDraw.Draw(img)
    d.ellipse([mx - 60, my - 60, mx + 60, my + 60], fill=rgb("#e8f2e4"))
    d.ellipse([mx - 28, my - 72, mx + 86, my + 42], fill=rgb("#0c2c38"))
    hz = S * 0.63
    hills(img, ridge(S, hz, 50, rnd), rgb("#06181e"), int(hz) + 4)
    # Bare trees on the far bank.
    for _ in range(9):
        x, h = rnd.uniform(0, S), rnd.uniform(90, 220)
        d.line([(x, hz), (x + rnd.uniform(-10, 10), hz - h)], fill=rgb("#06181e"), width=8)
        for _ in range(4):
            y, bl, side = hz - h * rnd.uniform(0.4, 0.95), rnd.uniform(26, 70), rnd.choice((-1, 1))
            d.line([(x, y), (x + side * bl, y - bl * 0.8)], fill=rgb("#06181e"), width=4)
    # A lantern on a post, a jetty, and the light on the water.
    lx, ly = S * 0.36, S * 0.56
    img = glow(img, (lx, ly), 230, (255, 190, 90), 130)
    img = glow(img, (lx, ly), 70, (255, 230, 160), 230, 20)
    ref, rd = layer(img)
    for i in range(34):
        y = hz + 30 + i * 11
        half = 24 + i * 5 + rnd.uniform(-10, 10)
        rd.line([(lx - half, y), (lx + half, y)], fill=(255, 196, 100, max(18, 210 - i * 6)), width=4)
    img = put(img, ref, 2)
    d = ImageDraw.Draw(img)
    d.rectangle([lx + 36, ly - 60, lx + 46, hz + 80], fill=rgb("#03090b"))
    d.line([(lx + 40, ly - 56), (lx, ly - 56), (lx, ly - 30)], fill=rgb("#03090b"), width=7)
    d.rounded_rectangle([lx - 20, ly - 30, lx + 20, ly + 26], radius=6, fill=rgb("#ffe6a6"), outline=rgb("#03090b"), width=6)
    d.polygon([(lx - 26, ly - 30), (lx, ly - 48), (lx + 26, ly - 30)], fill=rgb("#03090b"))
    d.polygon([(lx - 60, hz + 70), (lx + 260, hz + 40), (lx + 300, hz + 62), (lx - 30, hz + 100)], fill=rgb("#03090b"))
    for k in range(4):
        d.rectangle([lx - 30 + k * 96, hz + 74 - k * 9, lx - 20 + k * 96, hz + 150 - k * 9], fill=rgb("#03090b"))
    fog, fd = layer(img)
    for _ in range(9):
        x, y, w = rnd.uniform(-100, S), hz + rnd.uniform(-40, 30), rnd.uniform(240, 520)
        fd.ellipse([x, y, x + w, y + 36], fill=(190, 220, 215, 70))
    img = put(img, fog, 22)
    d = ImageDraw.Draw(img)
    for _ in range(46):
        x, h = rnd.uniform(0, S), rnd.uniform(120, 330)
        lean = rnd.uniform(-28, 28)
        d.line([(x, S), (x + lean, S - h)], fill=rgb("#020709"), width=5)
        if rnd.random() > 0.5:
            d.rounded_rectangle([x + lean - 7, S - h - 56, x + lean + 7, S - h], radius=7, fill=rgb("#020709"))
    img = finish(img, rnd, 0.8)
    img = lettering(img, ["Lantern", "Marsh"], ("Baskerville.ttc", 1), 126, 100, fill=rgb("#ffe2a6"), halo=(255, 170, 70), gap=0.94, fit=0.62)
    return img


def copper_canyon(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#f6c36a"), (0.4, "#f28f4b"), (0.7, "#d9553a"), (1, "#8f2f2a")])
    d = ImageDraw.Draw(img)
    d.ellipse([S * 0.5 - 200, S * 0.3 - 200, S * 0.5 + 200, S * 0.3 + 200], fill=rgb("#fff0c2"))
    img = glow(img, (S * 0.5, S * 0.3), 300, (255, 236, 180), 110)

    def mesa(x0: float, x1: float, top: float, tone: str, strata: bool = True) -> None:
        d = ImageDraw.Draw(img)
        c = rgb(tone)
        pts = [(x0 - 60, S), (x0 - 20, top + 120), (x0, top + 110), (x0 + 16, top + 6), (x0 + 40, top)]
        x = x0 + 40
        y = top
        while x < x1 - 60:
            x += rnd.uniform(30, 90)
            y = top + rnd.choice((0, 0, 18, 34))
            pts += [(x, pts[-1][1]), (x, y)]
        pts += [(x1 - 30, pts[-1][1]), (x1 - 12, top + 100), (x1 + 20, top + 126), (x1 + 70, S)]
        d.polygon(pts, fill=c)
        if strata:
            for k in range(1, 12):
                y = top + k * 44
                d.line([(x0 - 20 - k * 3, y), (x1 + k * 4, y)], fill=mix(c, (60, 14, 10) if k % 2 else (255, 214, 150), 0.22), width=9)
            d.polygon(pts[:2] + [(x0 + 60, S)], fill=mix(c, (40, 10, 10), 0.25))

    mesa(S * 0.55, S * 0.82, S * 0.44, "#c2603f", False)
    mesa(-S * 0.05, S * 0.3, S * 0.38, "#b04a30")
    mesa(S * 0.7, S * 1.04, S * 0.5, "#a23f2a")
    hills(img, ridge(S, S * 0.84, 22, rnd), rgb("#6b2320"))
    d = ImageDraw.Draw(img)
    # A trestle bridge with a short train, across the gap.
    by = S * 0.7
    d.rectangle([S * 0.24, by, S * 0.76, by + 10], fill=rgb("#3a1512"))
    for k in range(9):
        x = S * 0.26 + k * S * 0.06
        d.line([(x, by + 10), (x - 12, S * 0.86)], fill=rgb("#3a1512"), width=5)
        d.line([(x, by + 10), (x + 12, S * 0.86)], fill=rgb("#3a1512"), width=5)
    for k in range(4):
        x = S * 0.36 + k * 74
        d.rounded_rectangle([x, by - 40, x + 66, by - 2], radius=6, fill=rgb("#2a0f0e") if k else rgb("#4a1a16"))
    d.rectangle([S * 0.36 + 10, by - 62, S * 0.36 + 26, by - 40], fill=rgb("#2a0f0e"))
    smoke, sd = layer(img)
    for i in range(7):
        sd.ellipse([S * 0.36 - i * 34 - 10, by - 96 - i * 16, S * 0.36 - i * 34 + 40 + i * 8, by - 56 - i * 14], fill=(255, 240, 220, 150 - i * 16))
    img = put(img, smoke, 6)
    d = ImageDraw.Draw(img)
    for cx, ch in ((0.1, 190), (0.88, 150), (0.62, 96)):
        x, y = S * cx, S * 0.97
        c = rgb("#1f3a2a")
        d.rounded_rectangle([x - 16, y - ch, x + 16, y], radius=16, fill=c)
        d.rounded_rectangle([x - 56, y - ch * 0.62, x - 34, y - ch * 0.3], radius=11, fill=c)
        d.rectangle([x - 56, y - ch * 0.4, x, y - ch * 0.3], fill=c)
        d.rounded_rectangle([x + 34, y - ch * 0.82, x + 56, y - ch * 0.46], radius=11, fill=c)
        d.rectangle([x, y - ch * 0.56, x + 56, y - ch * 0.46], fill=c)
    img = finish(img, rnd, 0.45)
    img = lettering(img, ["COPPER", "CANYON"], ("Rockwell.ttc", 1), 136, 92, fill=rgb("#fff3d6"), stroke=(5, rgb("#5a1a14")), shadow=(60, 14, 10), gap=0.96)
    return img


def kite_season(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#7cc6f2"), (0.6, "#cdeeff"), (0.8, "#fdf6e3"), (1, "#fdf6e3")])
    img = clouds(img, rnd, 6, (0.2, 0.62), (255, 255, 255), 230)
    for i, (base, c) in enumerate(((0.8, "#9bd67a"), (0.87, "#6fbf5f"), (0.94, "#4aa24c"))):
        hills(img, ridge(S, S * base, 46, rnd), rgb(c))
    d = ImageDraw.Draw(img)
    # Two children on the hill (plain shapes), each holding a string.
    hands = []
    for px, shirt in ((0.3, "#e4572e"), (0.42, "#3d5a80")):
        x, y = S * px, S * 0.86
        d.rounded_rectangle([x - 15, y - 62, x + 15, y - 8], radius=12, fill=rgb(shirt))
        d.ellipse([x - 16, y - 96, x + 16, y - 64], fill=rgb("#3a2a20"))
        d.line([(x - 8, y - 8), (x - 12, y + 30)], fill=rgb("#2a2a3a"), width=9)
        d.line([(x + 8, y - 8), (x + 14, y + 30)], fill=rgb("#2a2a3a"), width=9)
        d.line([(x + 10, y - 50), (x + 38, y - 86)], fill=rgb(shirt), width=9)
        hands.append((x + 38, y - 86))
    kites = [(0.66, 0.42, 104, "#e4572e", "#ffd166", -20, hands[0]), (0.84, 0.6, 76, "#3d5a80", "#98c1d9", 14, hands[1]), (0.2, 0.5, 64, "#7a5cc0", "#f4a6c0", 22, None)]
    for kx, ky, ks, c1, c2, tilt, hand in kites:
        x, y = S * kx, S * ky
        a = math.radians(tilt)

        def rot(dx: float, dy: float) -> tuple[float, float]:
            return (x + dx * math.cos(a) - dy * math.sin(a), y + dx * math.sin(a) + dy * math.cos(a))

        top, right, bottom, left = rot(0, -ks), rot(ks * 0.68, -ks * 0.12), rot(0, ks * 1.25), rot(-ks * 0.68, -ks * 0.12)
        if hand:
            d.line([bottom, ((bottom[0] + hand[0]) / 2 + 40, (bottom[1] + hand[1]) / 2 + 60), hand], fill=(90, 90, 110), width=2, joint="curve")
        d.polygon([top, right, rot(0, -ks * 0.12)], fill=rgb(c1))
        d.polygon([top, left, rot(0, -ks * 0.12)], fill=rgb(c2))
        d.polygon([bottom, right, rot(0, -ks * 0.12)], fill=rgb(c2))
        d.polygon([bottom, left, rot(0, -ks * 0.12)], fill=rgb(c1))
        d.line([top, bottom], fill=(60, 50, 60), width=3)
        d.line([left, right], fill=(60, 50, 60), width=3)
        tail = [(bottom[0] + math.sin(t * 6) * 20 + t * 30 * (1 if tilt < 0 else -1), bottom[1] + t * ks * 1.7) for t in [i / 16 for i in range(17)]]
        d.line(tail, fill=rgb(c1), width=4, joint="curve")
        for i in (4, 8, 12):
            bx, bw = tail[i]
            d.polygon([(bx - 15, bw - 9), (bx + 15, bw + 9), (bx + 15, bw - 9), (bx - 15, bw + 9)], fill=rgb(c2))
    img = finish(img, rnd, 0.25)
    img = lettering(img, ["Kite Season"], ("MarkerFelt.ttc", 1), 150, 104, fill=rgb("#24476b"), shadow=None, stroke=(7, (255, 255, 255)))
    return img


def mosswood_tales(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#f7d488"), (0.4, "#f2b866"), (0.75, "#8fb86a"), (1, "#3f7a4a")])
    img = glow(img, (S * 0.6, S * 0.4), 280, (255, 244, 200), 170)
    d = ImageDraw.Draw(img)

    def tree(x: float, y: float, h: float, tone: float) -> None:
        trunk = mix(rgb("#5a3a22"), rgb("#f2b866"), tone)
        d.polygon([(x - h * 0.05, y), (x - h * 0.03, y - h * 0.6), (x + h * 0.03, y - h * 0.6), (x + h * 0.05, y)], fill=trunk)
        for dx, dy, r, c in ((0, -0.72, 0.3, "#2f7d4a"), (-0.2, -0.56, 0.22, "#276b40"), (0.2, -0.58, 0.24, "#3a8f55"), (0.04, -0.9, 0.2, "#49a362")):
            cx, cy, rr = x + dx * h, y + dy * h, r * h
            d.ellipse([cx - rr, cy - rr, cx + rr, cy + rr], fill=mix(rgb(c), rgb("#f2b866"), tone))

    for _ in range(9):
        tree(rnd.uniform(0, S), S * 0.72, rnd.uniform(230, 330), 0.55)
    hills(img, ridge(S, S * 0.72, 22, rnd), rgb("#6da257"))
    for x in (0.08, 0.24, 0.9, 0.76):
        tree(S * x, S * 0.86, rnd.uniform(420, 560), 0.0)
    hills(img, ridge(S, S * 0.86, 26, rnd), rgb("#3f7a4a"))
    d = ImageDraw.Draw(img)
    # A cabin with a lit window and a path to its door.
    cx, cy = S * 0.52, S * 0.84
    d.polygon([(cx - 22, cy), (cx + 22, cy), (cx + 130, S), (cx - 150, S)], fill=rgb("#e6cf9a"))
    d.rectangle([cx - 110, cy - 110, cx + 110, cy], fill=rgb("#8a5a34"))
    for k in range(5):
        d.line([(cx - 110, cy - 22 * k - 11), (cx + 110, cy - 22 * k - 11)], fill=rgb("#734826"), width=4)
    d.polygon([(cx - 136, cy - 104), (cx, cy - 212), (cx + 136, cy - 104)], fill=rgb("#b5432e"))
    d.polygon([(cx, cy - 212), (cx + 136, cy - 104), (cx + 20, cy - 104)], fill=rgb("#953524"))
    d.rectangle([cx + 56, cy - 206, cx + 84, cy - 150], fill=rgb("#6b4a3a"))
    d.rounded_rectangle([cx - 22, cy - 70, cx + 22, cy], radius=20, fill=rgb("#4a2f1c"))
    img = glow(img, (cx - 66, cy - 58), 44, (255, 220, 120), 190, 14)
    d = ImageDraw.Draw(img)
    for wx in (cx - 66, cx + 66):
        d.rectangle([wx - 22, cy - 80, wx + 22, cy - 36], fill=rgb("#ffe08a"))
        d.line([(wx, cy - 80), (wx, cy - 36)], fill=rgb("#4a2f1c"), width=4)
        d.line([(wx - 22, cy - 58), (wx + 22, cy - 58)], fill=rgb("#4a2f1c"), width=4)
    smoke, sd = layer(img)
    for i in range(6):
        sd.ellipse([cx + 54 + i * 16, cy - 240 - i * 34, cx + 100 + i * 22, cy - 200 - i * 30], fill=(255, 255, 255, 150 - i * 20))
    img = put(img, smoke, 7)
    d = ImageDraw.Draw(img)
    for mx, ms in ((0.16, 44), (0.22, 28), (0.84, 38)):
        x, y = S * mx, S * 0.97
        d.rounded_rectangle([x - ms * 0.22, y - ms, x + ms * 0.22, y], radius=6, fill=rgb("#f6ecd6"))
        d.pieslice([x - ms, y - ms * 1.9, x + ms, y - ms * 0.3], 180, 360, fill=rgb("#d9412f"))
        for dx, dy in ((-0.45, -1.2), (0.3, -1.35), (0, -1.05)):
            d.ellipse([x + dx * ms - 6, y + dy * ms - 6, x + dx * ms + 6, y + dy * ms + 6], fill=(255, 255, 255))
    img = finish(img, rnd, 0.35)
    img = lettering(img, ["Mosswood", "Tales"], ("Georgia Bold Italic.ttf", 0), 124, 94, fill=rgb("#fff7e0"), stroke=(5, rgb("#2f5a36")), shadow=(30, 60, 30), gap=0.98)
    return img


def midnight_tram(rnd: random.Random) -> Image.Image:
    img = sky(S, S, [(0, "#070b1f"), (0.5, "#16204a"), (0.72, "#3a2f6a"), (0.74, "#0c1026"), (1, "#05060f")])
    img = stars(img, rnd, 50, 0.3)
    d = ImageDraw.Draw(img)
    hz = S * 0.74
    for side in (0, 1):
        for k in range(5):
            w = 150 - k * 22
            x = (k * 104) if side == 0 else (S - k * 104 - w)
            top = S * 0.16 + k * 84 + rnd.uniform(-20, 20)
            tone = mix(rgb("#0c1230"), rgb("#2a2a5a"), k / 5)
            d.rectangle([x, top, x + w, hz], fill=tone)
            for wy in range(int(top) + 18, int(hz) - 20, 34):
                for wx in range(int(x) + 12, int(x + w) - 16, 28):
                    if rnd.random() > 0.5:
                        d.rectangle([wx, wy, wx + 12, wy + 18], fill=rnd.choice(((255, 214, 130), (255, 170, 190), (150, 220, 255))))
    # Wet street, rails to a vanishing point, wires overhead.
    vx = S * 0.5
    d.polygon([(vx - 30, hz), (vx + 30, hz), (S * 0.98, S), (S * 0.02, S)], fill=rgb("#0e1330"))
    for off in (-1, 1):
        d.line([(vx + off * 10, hz), (vx + off * 200, S)], fill=rgb("#8fa3d8"), width=5)
    for off in (-36, 36):
        d.line([(vx + off, 0), (vx + off * 0.2, hz - 250)], fill=rgb("#05060f"), width=3)
    # The tram, head-on.
    tx, ty, tw, th = vx, hz + 40, 250, 380
    img = glow(img, (tx, ty - 60), 280, (255, 210, 120), 90)
    d = ImageDraw.Draw(img)
    d.line([(tx - 50, ty - th), (tx, ty - th - 90), (tx + 50, ty - th)], fill=rgb("#05060f"), width=6)
    d.rounded_rectangle([tx - tw / 2, ty - th, tx + tw / 2, ty], radius=40, fill=rgb("#c9432e"))
    d.rounded_rectangle([tx - tw / 2, ty - th * 0.44, tx + tw / 2, ty], radius=26, fill=rgb("#f1e3c4"))
    d.rounded_rectangle([tx - tw / 2 + 22, ty - th + 84, tx + tw / 2 - 22, ty - th * 0.5], radius=16, fill=rgb("#ffe6a0"))
    d.line([(tx, ty - th + 84), (tx, ty - th * 0.5)], fill=rgb("#c9432e"), width=8)
    d.rounded_rectangle([tx - 76, ty - th + 26, tx + 76, ty - th + 68], radius=10, fill=rgb("#10142e"))
    d.text((tx - 50, ty - th + 28), "LINE 7", font=font("DIN Condensed Bold.ttf", 40), fill=rgb("#ffd166"))
    d.rectangle([tx - tw / 2 - 6, ty - 26, tx + tw / 2 + 6, ty], fill=rgb("#10142e"))
    for off in (-1, 1):
        img = glow(img, (tx + off * 78, ty - 90), 66, (255, 244, 200), 230, 16)
        ImageDraw.Draw(img).ellipse([tx + off * 78 - 24, ty - 114, tx + off * 78 + 24, ty - 66], fill=(255, 252, 230))
    ref, rd = layer(img)
    for i in range(28):
        y = ty + 8 + i * 9
        for off in (-78, 78):
            half = 16 + i * 2.6
            rd.line([(tx + off * (1 + i * 0.03) - half, y), (tx + off * (1 + i * 0.03) + half, y)], fill=(255, 226, 150, max(12, 200 - i * 7)), width=4)
    img = put(img, ref, 2)
    rain, rd = layer(img)
    for _ in range(170):
        x, y, ln = rnd.uniform(-60, S), rnd.uniform(0, S), rnd.uniform(26, 60)
        rd.line([(x, y), (x + ln * 0.22, y + ln)], fill=(200, 220, 255, rnd.randint(50, 130)), width=2)
    img = put(img, rain)
    img = finish(img, rnd, 0.6)
    img = lettering(img, ["MIDNIGHT", "TRAM"], ("Futura.ttc", 4), 128, 92, fill=rgb("#ffd166"), track=0.05, shadow=(0, 0, 0), gap=0.9)
    return img


COVERS = {
    "PPSA90001": neon_drift,
    "PPSA90002": skyward_isles,
    "PPSA90003": iron_harbor,
    "PPSA90004": lumen_trail,
    "PPSA90005": pocket_galaxy,
    "PPSA90006": frostline,
    "CUSA90007": ember_knights,
    "CUSA90008": tidal_run,
    "PPSA90009": clockwork_garden,
    "PPSA90010": lantern_marsh,
    "PPSA90011": copper_canyon,
    "PPSA90012": kite_season,
    "CUSA90013": mosswood_tales,
    "CUSA90014": midnight_tram,
}


def stream_frame(w: int = 1920, h: int = 1080) -> None:
    """A made-up game moment (Neon Drift): a neon road at dusk, the car, and a simple HUD."""
    rnd = random.Random(7)
    img = sky(w, h, [(0, "#10062e"), (0.36, "#4a1670"), (0.58, "#ff4f8b"), (0.6, "#170a36"), (1, "#0a041c")])
    img = stars(img, rnd, 60, 0.34)
    hz = h * 0.6
    img = glow(img, (w / 2, hz - 40), 380, (255, 120, 90), 150)
    sun, d = layer(img)
    d.ellipse([w / 2 - 230, hz - 380, w / 2 + 230, hz + 80], fill=(255, 205, 110, 255))
    for i in range(9):
        y = hz - 170 + i * 30
        d.rectangle([0, y, w, y + 5 + i * 2.2], fill=(0, 0, 0, 0))
    d.rectangle([0, hz, w, h], fill=(0, 0, 0, 0))
    img = put(img, sun)
    hills(img, ridge(w, hz - 6, 46, rnd), rgb("#2a0f55"), int(hz) + 2)
    d = ImageDraw.Draw(img)
    x = 0
    while x < w:
        bw = rnd.randint(34, 86)
        if not (w * 0.36 < x < w * 0.6):
            t = hz - rnd.uniform(40, 200)
            d.rectangle([x, t, x + bw, hz], fill=(24, 14, 62))
            for wy in range(int(t) + 12, int(hz) - 8, 26):
                for wx in range(x + 10, x + bw - 12, 22):
                    if rnd.random() > 0.62:
                        d.rectangle([wx, wy, wx + 9, wy + 12], fill=(255, 220, 140))
        x += bw + rnd.randint(0, 12)
    for i in range(1, 13):
        y = hz + (i / 12) ** 2.1 * (h - hz)
        d.line([(0, y), (w, y)], fill=(255, 70, 170), width=3)
    for k in range(-8, 9):
        d.line([(w / 2 + k * 36, hz), (w / 2 + k * 600, h)], fill=(255, 70, 170), width=3)
    d.polygon([(w / 2 - 34, hz), (w / 2 + 34, hz), (w * 0.86, h), (w * 0.14, h)], fill=rgb("#150a30"))
    d.line([(w / 2 - 34, hz), (w * 0.14, h)], fill=(0, 230, 255), width=7)
    d.line([(w / 2 + 34, hz), (w * 0.86, h)], fill=(0, 230, 255), width=7)
    for i in range(8):
        t0, t1 = (i / 8) ** 2, ((i + 0.45) / 8) ** 2
        y0, y1 = hz + t0 * (h - hz), hz + t1 * (h - hz)
        d.polygon([(w / 2 - 3 - t0 * 16, y0), (w / 2 + 3 + t0 * 16, y0), (w / 2 + 3 + t1 * 16, y1), (w / 2 - 3 - t1 * 16, y1)], fill=(255, 255, 255))
    # A rival ahead, and the player's car.
    rx, ry = w * 0.455, hz + 70
    d.rounded_rectangle([rx - 42, ry - 16, rx + 42, ry + 16], radius=8, fill=(255, 190, 70))
    d.rounded_rectangle([rx - 28, ry - 34, rx + 28, ry - 12], radius=8, fill=(120, 70, 20))
    cx, cy = w * 0.54, h * 0.84
    img = glow(img, (cx, cy + 60), 220, (0, 230, 255), 110, 40)
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([cx - 200, cy - 40, cx + 200, cy + 66], radius=28, fill=rgb("#1b1238"))
    d.polygon([(cx - 150, cy - 38), (cx - 104, cy - 116), (cx + 104, cy - 116), (cx + 150, cy - 38)], fill=rgb("#2b1c58"))
    d.polygon([(cx - 124, cy - 46), (cx - 92, cy - 102), (cx + 92, cy - 102), (cx + 124, cy - 46)], fill=rgb("#7ee8ff"))
    d.rounded_rectangle([cx - 188, cy - 8, cx - 84, cy + 16], radius=10, fill=(255, 60, 120))
    d.rounded_rectangle([cx + 84, cy - 8, cx + 188, cy + 16], radius=10, fill=(255, 60, 120))
    d.rectangle([cx - 200, cy + 58, cx - 138, cy + 94], fill=(10, 6, 22))
    d.rectangle([cx + 138, cy + 58, cx + 200, cy + 94], fill=(10, 6, 22))
    img = img.filter(ImageFilter.SMOOTH)
    # HUD: lap and place, a time, a round map with dots, the speed.
    hud, d = layer(img)
    big, mid, small = font("DIN Condensed Bold.ttf", 84), font("DIN Condensed Bold.ttf", 48), font("Avenir Next Condensed.ttc", 30, 2)
    d.rounded_rectangle([56, 170, 372, 360], radius=22, fill=(10, 6, 30, 150))
    d.text((84, 184), "LAP 2/3", font=big, fill=(255, 255, 255, 255))
    d.text((84, 280), "1st", font=mid, fill=(0, 230, 255, 255))
    d.text((176, 290), "01:42.318", font=font("DIN Condensed Bold.ttf", 40), fill=(255, 255, 255, 230))
    mx, my, mr = w - 190, 290, 110
    d.ellipse([mx - mr, my - mr, mx + mr, my + mr], fill=(10, 6, 30, 150), outline=(255, 255, 255, 200), width=4)
    track = [(mx + math.cos(a) * mr * (0.62 + 0.14 * math.sin(a * 3)), my + math.sin(a) * mr * (0.6 + 0.12 * math.cos(a * 2))) for a in [i * math.pi / 30 for i in range(61)]]
    d.line(track, fill=(255, 70, 170, 255), width=7, joint="curve")
    for i, c in ((8, (0, 230, 255, 255)), (13, (255, 190, 70, 255)), (40, (255, 255, 255, 255))):
        px, py = track[i]
        d.ellipse([px - 9, py - 9, px + 9, py + 9], fill=c)
    d.rounded_rectangle([w - 420, h - 230, w - 60, h - 70], radius=22, fill=(10, 6, 30, 150))
    d.text((w - 390, h - 226), "248", font=font("DIN Condensed Bold.ttf", 150), fill=(255, 255, 255, 255))
    d.text((w - 188, h - 150), "km/h", font=mid, fill=(0, 230, 255, 255))
    d.rounded_rectangle([w - 392, h - 96, w - 92, h - 86], radius=5, fill=(255, 255, 255, 70))
    d.rounded_rectangle([w - 392, h - 96, w - 170, h - 86], radius=5, fill=(255, 70, 170, 255))
    d.text((84, h - 130), "BOOST", font=small, fill=(255, 255, 255, 230))
    for i in range(5):
        d.rounded_rectangle([84 + i * 54, h - 90, 128 + i * 54, h - 72], radius=6, fill=(0, 230, 255, 255) if i < 3 else (255, 255, 255, 70))
    img = put(img, hud)
    img.save(OUT / "stream.png", optimize=True)


if __name__ == "__main__":
    for gid, draw in COVERS.items():
        cover = draw(random.Random(gid)).resize((512, 512), Image.LANCZOS)
        # 256 colours keep the file small; the app shows the cover smaller than this anyway.
        cover.quantize(256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.FLOYDSTEINBERG).save(OUT / f"{gid}.png", optimize=True)
    stream_frame()
    print("art:", sorted(p.name for p in OUT.iterdir()))
