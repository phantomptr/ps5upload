#!/usr/bin/env python3
"""Generate the Android launcher icons from the app's icon source.

Writes client/src-tauri/icons/android/ (committed). android-postinit-patch.sh copies
that set into the generated Android project on every build, so the launcher shows the
PS5Upload logo on every device:

- adaptive icon (Android 8+, what nearly every phone uses): the logo on a transparent
  foreground layer, sized to sit inside the launcher's safe zone, over a white background;
- legacy square and round icons (older launchers): the logo on a full-bleed white tile/disc;
- the Android TV banner (drawable-xhdpi/tv_banner.png, 320x180): the horizontal lockup on white,
  which the TV home screen shows in place of the launcher icon.

Run it again only when the logo changes:  python3 scripts/release/android-icons.py
"""
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "client/src-tauri/icons/sources/macos_squircle_1024.png"
LOCKUP = ROOT / "branding/png/lockup-horizontal-black.png"
OUT = ROOT / "client/src-tauri/icons/android"

# Launcher icon sizes per density (legacy icon, 48dp) and the adaptive layer (108dp).
DENSITIES = {"mdpi": 1.0, "hdpi": 1.5, "xhdpi": 2.0, "xxhdpi": 3.0, "xxxhdpi": 4.0}
# The safe zone is a 66dp circle in a 108dp layer; keep the logo's diagonal inside it.
FOREGROUND_FILL = 0.56
LEGACY_FILL = 0.74


def logo() -> Image.Image:
    """The logo alone: everything on the squircle that isn't its white fill, cropped."""
    im = Image.open(SRC).convert("RGBA")
    white = Image.new("RGBA", im.size, (255, 255, 255, 255))
    diff = ImageChops.difference(im, white).convert("L").point(lambda v: 255 if v > 24 else 0)
    # Drop the squircle's own edge (transparent outside it also differs from white).
    alpha = im.getchannel("A").point(lambda v: 255 if v > 250 else 0)
    inner = ImageChops.multiply(diff, alpha)
    box = inner.getbbox()
    assert box, "no logo found in the icon source"
    crop = im.crop(box)
    # Keep only the logo's pixels: the white fill around it becomes transparent.
    mask = ImageChops.difference(crop, Image.new("RGBA", crop.size, (255, 255, 255, 255))).convert("L")
    mask = mask.point(lambda v: min(255, v * 6))
    out = crop.copy()
    out.putalpha(ImageChops.multiply(mask, crop.getchannel("A")))
    return out


def fit(img: Image.Image, side: int, fill: float) -> Image.Image:
    w, h = img.size
    scale = side * fill / max(w, h)
    return img.resize((max(1, round(w * scale)), max(1, round(h * scale))), Image.LANCZOS)


def centred(canvas: Image.Image, img: Image.Image) -> Image.Image:
    canvas.alpha_composite(img, ((canvas.width - img.width) // 2, (canvas.height - img.height) // 2))
    return canvas


def tv_banner() -> None:
    """320x180 at xhdpi (Android TV's banner size): the lockup inside a 10% margin, on white."""
    w, h = 320, 180
    lockup = Image.open(LOCKUP).convert("RGBA")
    box = lockup.getchannel("A").getbbox()
    assert box, "the lockup is empty"
    lockup = lockup.crop(box)
    scale = min(w * 0.8 / lockup.width, h * 0.8 / lockup.height)
    lockup = lockup.resize((round(lockup.width * scale), round(lockup.height * scale)), Image.LANCZOS)
    d = OUT / "drawable-xhdpi"
    d.mkdir(parents=True, exist_ok=True)
    centred(Image.new("RGBA", (w, h), (255, 255, 255, 255)), lockup).save(d / "tv_banner.png")


def main() -> None:
    tv_banner()
    mark = logo()
    for name, k in DENSITIES.items():
        d = OUT / f"mipmap-{name}"
        d.mkdir(parents=True, exist_ok=True)
        fg = round(108 * k)
        centred(Image.new("RGBA", (fg, fg), (0, 0, 0, 0)), fit(mark, fg, FOREGROUND_FILL)).save(d / "ic_launcher_foreground.png")
        side = round(48 * k)
        # Legacy square: a white rounded tile edge to edge.
        tile = Image.new("RGBA", (side, side), (0, 0, 0, 0))
        ImageDraw.Draw(tile).rounded_rectangle((0, 0, side - 1, side - 1), radius=round(side * 0.18), fill=(255, 255, 255, 255))
        centred(tile, fit(mark, side, LEGACY_FILL)).save(d / "ic_launcher.png")
        # Legacy round: a white disc.
        disc = Image.new("RGBA", (side, side), (0, 0, 0, 0))
        ImageDraw.Draw(disc).ellipse((0, 0, side - 1, side - 1), fill=(255, 255, 255, 255))
        centred(disc, fit(mark, side, LEGACY_FILL * 0.92)).save(d / "ic_launcher_round.png")
    any_dpi = OUT / "mipmap-anydpi-v26"
    any_dpi.mkdir(parents=True, exist_ok=True)
    xml = (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
        '  <background android:drawable="@color/ic_launcher_background"/>\n'
        '  <foreground android:drawable="@mipmap/ic_launcher_foreground"/>\n'
        '  <monochrome android:drawable="@mipmap/ic_launcher_foreground"/>\n'
        "</adaptive-icon>\n"
    )
    (any_dpi / "ic_launcher.xml").write_text(xml)
    (any_dpi / "ic_launcher_round.xml").write_text(xml)
    values = OUT / "values"
    values.mkdir(parents=True, exist_ok=True)
    (values / "ic_launcher_background.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n<resources>\n  <color name="ic_launcher_background">#FFFFFF</color>\n</resources>\n'
    )
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
