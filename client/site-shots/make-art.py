"""Made-up artwork for the website's screenshots (site-shots/): game covers for fictional games
and one stream picture. Everything is drawn here, so no real game, cover or person appears in
the screenshots. Run: python3 site-shots/make-art.py (needs Pillow)."""

import math
import random
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

OUT = Path(__file__).parent / "art"
OUT.mkdir(exist_ok=True)

# Fictional games: (id, title, two colours, motif).
GAMES = [
    ("PPSA90001", "Neon Drift", ("#1b0f3b", "#ff3d81"), "road"),
    ("PPSA90002", "Skyward Isles", ("#0b3c5d", "#7fd8ff"), "islands"),
    ("PPSA90003", "Iron Harbor", ("#1f2a33", "#f2a541"), "towers"),
    ("PPSA90004", "Lumen Trail", ("#0d2b1f", "#9bff8a"), "trail"),
    ("PPSA90005", "Pocket Galaxy", ("#120a2e", "#b58cff"), "planets"),
    ("PPSA90006", "Frostline", ("#0e1f33", "#e6f6ff"), "peaks"),
    ("CUSA90007", "Ember Knights", ("#2b0d0d", "#ff7a3d"), "sun"),
    ("CUSA90008", "Tidal Run", ("#06283d", "#47b5ff"), "waves"),
]


def font(size: int) -> ImageFont.FreeTypeFont:
    for name in (
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return ImageFont.load_default()


def hex_rgb(h: str) -> tuple[int, int, int]:
    h = h.lstrip("#")
    return tuple(int(h[i : i + 2], 16) for i in (0, 2, 4))  # type: ignore[return-value]


def gradient(w: int, h: int, a: str, b: str) -> Image.Image:
    ca, cb = hex_rgb(a), hex_rgb(b)
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(1, h - 1)
        for x in range(w):
            u = min(1.0, t * 0.85 + (x / w) * 0.25)
            px[x, y] = tuple(int(ca[i] + (cb[i] - ca[i]) * u * 0.55) for i in range(3))
    return img


def motif(d: ImageDraw.ImageDraw, w: int, h: int, kind: str, accent: tuple[int, int, int], rnd: random.Random) -> None:
    a = accent
    soft = tuple(min(255, c + 40) for c in a)
    if kind == "road":
        for i in range(14):
            y = h * 0.55 + i * i * 2.2
            d.line([(w * 0.5 - i * 26, y), (w * 0.5 + i * 26, y)], fill=a, width=2)
        d.polygon([(w * 0.5, h * 0.55), (w * 0.05, h), (w * 0.95, h)], outline=soft, width=4)
        d.ellipse([w * 0.38, h * 0.18, w * 0.62, h * 0.42], fill=a)
    elif kind == "islands":
        for _ in range(5):
            x, y, r = rnd.uniform(0.1, 0.9) * w, rnd.uniform(0.35, 0.75) * h, rnd.uniform(28, 70)
            d.ellipse([x - r, y - r * 0.45, x + r, y + r * 0.45], fill=soft)
            d.polygon([(x - r * 0.7, y), (x + r * 0.7, y), (x, y + r * 1.4)], fill=a)
    elif kind == "towers":
        for i in range(9):
            x = w * (0.05 + i * 0.11)
            t = h * rnd.uniform(0.25, 0.6)
            d.rectangle([x, t, x + w * 0.08, h], fill=tuple(int(c * 0.55) for c in a))
            for j in range(int((h - t) / 22)):
                if rnd.random() > 0.4:
                    d.rectangle([x + 8, t + 10 + j * 22, x + 16, t + 18 + j * 22], fill=soft)
    elif kind == "trail":
        pts = [(w * 0.1 + i * w * 0.08, h * 0.8 - math.sin(i * 0.8) * h * 0.12 - i * h * 0.04) for i in range(11)]
        d.line(pts, fill=a, width=10, joint="curve")
        for _ in range(40):
            x, y = rnd.uniform(0, w), rnd.uniform(0, h * 0.6)
            d.ellipse([x, y, x + 3, y + 3], fill=soft)
    elif kind == "planets":
        for _ in range(4):
            x, y, r = rnd.uniform(0.15, 0.85) * w, rnd.uniform(0.15, 0.7) * h, rnd.uniform(18, 80)
            d.ellipse([x - r, y - r, x + r, y + r], fill=a)
            d.ellipse([x - r * 1.6, y - r * 0.25, x + r * 1.6, y + r * 0.25], outline=soft, width=3)
    elif kind == "peaks":
        for i, s in enumerate((0.75, 0.6, 0.5)):
            d.polygon([(0, h), (w * 0.3, h * s - i * 30), (w * 0.55, h), (w * 0.75, h * (s + 0.05)), (w, h)],
                      fill=tuple(int(c * (0.5 + i * 0.2)) for c in a))
    elif kind == "sun":
        d.ellipse([w * 0.3, h * 0.2, w * 0.7, h * 0.6], fill=a)
        for i in range(8):
            y = h * 0.42 + i * 12
            d.rectangle([w * 0.3, y, w * 0.7, y + 4 + i], fill=hex_rgb("#2b0d0d"))
        d.polygon([(w * 0.42, h), (w * 0.5, h * 0.55), (w * 0.58, h)], fill=soft)
    elif kind == "waves":
        for i in range(7):
            y = h * 0.45 + i * 30
            pts = [(x, y + math.sin(x / 40 + i) * 12) for x in range(0, w + 10, 10)]
            d.line(pts, fill=soft if i % 2 else a, width=6)


def cover(gid: str, title: str, colors: tuple[str, str], kind: str, size: int = 512) -> None:
    rnd = random.Random(gid)
    img = gradient(size, size, colors[0], colors[1])
    d = ImageDraw.Draw(img)
    motif(d, size, size, kind, hex_rgb(colors[1]), rnd)
    img = img.filter(ImageFilter.SMOOTH)
    # The title on a band at the bottom (the app puts its own tags on the top corners).
    band = Image.new("RGBA", (size, int(size * 0.22)), (0, 0, 0, 150))
    img = img.convert("RGBA")
    img.alpha_composite(band, (0, size - band.height))
    img = img.convert("RGB")
    d = ImageDraw.Draw(img)
    f = font(int(size * 0.1))
    tw = d.textlength(title, font=f)
    x, y = (size - tw) / 2, size - band.height + (band.height - size * 0.1) / 2 - 6
    d.text((x, y), title, font=f, fill=(255, 255, 255))
    img.save(OUT / f"{gid}.png", optimize=True)


def stream_frame(w: int = 1920, h: int = 1080) -> None:
    """A made-up game moment: a neon city road at dusk with a simple HUD."""
    rnd = random.Random(7)
    img = gradient(w, h, "#140a32", "#ff5fa2")
    d = ImageDraw.Draw(img)
    d.ellipse([w * 0.4, h * 0.12, w * 0.6, h * 0.48], fill=(255, 196, 110))
    for i in range(10):
        y = h * 0.3 + i * 9
        d.rectangle([w * 0.4, y, w * 0.6, y + 3 + i], fill=(20, 10, 50))
    for i in range(26):
        x = i * w / 26
        t = h * rnd.uniform(0.32, 0.5)
        d.rectangle([x, t, x + w / 30, h * 0.62], fill=(28, 18, 70))
        for j in range(int((h * 0.62 - t) / 26)):
            if rnd.random() > 0.55:
                d.rectangle([x + 10, t + 12 + j * 26, x + 20, t + 20 + j * 26], fill=(255, 220, 140))
    d.rectangle([0, h * 0.62, w, h], fill=(18, 8, 40))
    d.polygon([(w * 0.5, h * 0.62), (w * 0.08, h), (w * 0.92, h)], fill=(40, 24, 80))
    for i in range(12):
        y = h * 0.64 + i * i * 2.6
        d.line([(w * 0.5 - i * 40, y), (w * 0.5 + i * 40, y)], fill=(0, 230, 255), width=3)
    d.polygon([(w * 0.5, h * 0.62), (w * 0.08, h), (w * 0.12, h)], fill=(255, 61, 129))
    d.polygon([(w * 0.5, h * 0.62), (w * 0.92, h), (w * 0.88, h)], fill=(255, 61, 129))
    # The car.
    cx, cy = w * 0.5, h * 0.86
    d.rounded_rectangle([cx - 150, cy - 40, cx + 150, cy + 50], radius=24, fill=(30, 220, 255))
    d.rounded_rectangle([cx - 95, cy - 90, cx + 95, cy - 30], radius=20, fill=(20, 140, 200))
    d.rectangle([cx - 140, cy + 10, cx - 90, cy + 26], fill=(255, 240, 160))
    d.rectangle([cx + 90, cy + 10, cx + 140, cy + 26], fill=(255, 240, 160))
    img = img.filter(ImageFilter.SMOOTH_MORE)
    d = ImageDraw.Draw(img)
    # HUD.
    f, fs = font(56), font(30)
    d.text((70, 190), "LAP 2/3", font=f, fill=(255, 255, 255))
    d.text((70, 258), "POS 1st", font=fs, fill=(0, 230, 255))
    d.text((w - 330, h - 150), "248", font=font(96), fill=(255, 255, 255))
    d.text((w - 150, h - 112), "km/h", font=fs, fill=(255, 255, 255))
    img.save(OUT / "stream.png", optimize=True)


if __name__ == "__main__":
    for g in GAMES:
        cover(*g)
    stream_frame()
    print("art:", sorted(p.name for p in OUT.iterdir()))
