#!/usr/bin/env python3
"""Render every PS5Upload logo image from the masters in branding/.

The masters are SVG (branding/*.svg): the app icon, the bare mark and the name lockups. The
name is set in Titan One (SIL Open Font License, branding/fonts/TitanOne-OFL.txt) and kept as
outlines, so nothing here needs the font installed. This writes:

  client/src-tauri/icons/sources/macos_squircle_1024.png  the app icon source; then run
        `npx tauri icon` on it (desktop sizes, .icns, .ico) and the Android and iOS scripts
  client/public/logo-square.png        favicon, sidebar, About, the console page's top bar
  payload/assets/webui_tile_icon0.png  the PS5 home-screen tile (512x512, no transparency)
  site/assets/logo.png, favicon.png    the website
  engine/.../src/app_icon/, site/app-icon/  a phone's Home Screen icons for the web pages the
        engine serves (/app-icon/*) and the website's /join and /pad: apple-touch-icon.png
        (180, opaque), icon-192.png, icon-512.png, icon-maskable-512.png. Only these:
        python3 scripts/release/brand-assets.py --home-icons
  logo.png                             the README logo (light); branding/png/ has the dark one
  branding/png/                        lockups, icon and mark at full size, the GitHub social
                                       preview and the Discord server icon and banner

Needs rsvg-convert (brew install librsvg) and Pillow.
Run it again whenever a master changes:  python3 scripts/release/brand-assets.py
"""
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
B = ROOT / "branding"
PNG = B / "png"
BLACK = (11, 11, 12, 255)


def render(svg: Path | str, out: Path, *, w: int | None = None, h: int | None = None) -> Path:
    """Render an SVG file (or SVG text) to a PNG at the given width or height."""
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / "in.svg"
        if isinstance(svg, Path):
            src = svg
        else:
            src.write_text(svg)
        args = ["rsvg-convert", str(src), "-o", str(out)]
        if w:
            args += ["-w", str(w)]
        if h:
            args += ["-h", str(h)]
        subprocess.run(args, check=True)
    return out


def on(bg: tuple, size: tuple[int, int], art: Image.Image, scale: float) -> Image.Image:
    """`art` centred on a plain `bg` canvas, fitted to `scale` of the smaller side."""
    canvas = Image.new("RGBA", size, bg)
    k = min(size) * scale / max(art.size)
    fitted = art.resize((round(art.width * k), round(art.height * k)), Image.LANCZOS)
    canvas.alpha_composite(fitted, ((size[0] - fitted.width) // 2, (size[1] - fitted.height) // 2))
    return canvas


def optimise(p: Path) -> None:
    """Re-save as an optimised PNG: the console page embeds logo-square.png in its budget."""
    im = Image.open(p)
    im.save(p, optimize=True)


HOME_DIRS = (ROOT / "engine/crates/ps5upload-engine/src/app_icon", ROOT / "site/app-icon")
WHITE = (255, 255, 255, 255)


def home_icons() -> None:
    """A phone's Home Screen icons. iOS wants an opaque square (it rounds it itself); Android's
    maskable icon keeps the mark inside the middle 80% circle its masks may cut to."""
    icon = B / "icon.svg"
    square = re.sub(r'rx="58"', 'rx="0"', icon.read_text())
    a, *rest = HOME_DIRS
    a.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        big = Image.open(render(square, Path(tmp) / "sq.png", w=1024)).convert("RGBA")
        art = Image.open(render(B / "mark.svg", Path(tmp) / "mark.png", h=1024)).convert("RGBA")
    big.resize((180, 180), Image.LANCZOS).convert("RGB").save(a / "apple-touch-icon.png", optimize=True)
    for n in (192, 512):
        big.resize((n, n), Image.LANCZOS).convert("RGB").save(a / f"icon-{n}.png", optimize=True)
    on(WHITE, (512, 512), art, 0.56).convert("RGB").save(a / "icon-maskable-512.png", optimize=True)
    for d in rest:
        d.mkdir(parents=True, exist_ok=True)
        for f in a.iterdir():
            shutil.copyfile(f, d / f.name)


def main() -> None:
    if "--home-icons" in sys.argv:
        home_icons()
        print("home screen icons written")
        return
    PNG.mkdir(parents=True, exist_ok=True)
    icon = B / "icon.svg"

    # The app icon source (a white squircle; transparent outside it).
    render(icon, ROOT / "client/src-tauri/icons/sources/macos_squircle_1024.png", w=1024)
    # In the app and the console page.
    out = render(icon, ROOT / "client/public/logo-square.png", w=256)
    optimise(out)
    # The PS5 tile: square, opaque (the console rounds it itself).
    square = re.sub(r'rx="58"', 'rx="0"', icon.read_text())
    with tempfile.TemporaryDirectory() as tmp:
        t = render(square, Path(tmp) / "tile.png", w=512)
        Image.open(t).convert("RGB").save(ROOT / "payload/assets/webui_tile_icon0.png", optimize=True)
    # The website.
    optimise(render(icon, ROOT / "site/assets/logo.png", w=256))
    optimise(render(icon, ROOT / "site/assets/favicon.png", w=64))

    # Full-size masters as PNG.
    for name in ("icon", "icon-dark"):
        render(B / f"{name}.svg", PNG / f"{name}-1024.png", w=1024)
    for name in ("mark", "mark-white"):
        render(B / f"{name}.svg", PNG / f"{name}-1024.png", h=1024)
    for shape in ("horizontal", "stacked"):
        for colour in ("black", "white"):
            render(B / f"lockup-{shape}-{colour}.svg", PNG / f"lockup-{shape}-{colour}.png", h=600)
    # The README logo (light pages); the dark one is branding/png/lockup-horizontal-white.png.
    optimise(render(B / "lockup-horizontal-black.svg", ROOT / "logo.png", h=280))

    # GitHub social preview (1280x640) and Discord (server icon 512, banner 960x540):
    # white on black, which reads on both services' dark themes.
    mark = Image.open(render(B / "mark-white.svg", PNG / "_mark.png", h=1200)).convert("RGBA")
    # Discord crops the server icon to a circle: keep the mark well inside it.
    on(BLACK, (512, 512), mark, 0.62).convert("RGB").save(PNG / "discord-server-icon.png", optimize=True)
    wide = Image.open(render(B / "lockup-horizontal-white.svg", PNG / "_wide.png", h=600)).convert("RGBA")
    banner = Image.new("RGBA", (960, 540), BLACK)
    k = 960 * 0.72 / wide.width
    fitted = wide.resize((round(wide.width * k), round(wide.height * k)), Image.LANCZOS)
    banner.alpha_composite(fitted, ((960 - fitted.width) // 2, (540 - fitted.height) // 2))
    banner.convert("RGB").save(PNG / "discord-banner.png", optimize=True)
    # The social preview is a wide card: the side-by-side lockup fills it better.
    card = Image.new("RGBA", (1280, 640), BLACK)
    k = 1280 * 0.74 / wide.width
    fitted = wide.resize((round(wide.width * k), round(wide.height * k)), Image.LANCZOS)
    card.alpha_composite(fitted, ((1280 - fitted.width) // 2, (640 - fitted.height) // 2))
    card.convert("RGB").save(PNG / "github-social-preview.png", optimize=True)
    for tmp in ("_mark.png", "_wide.png"):
        (PNG / tmp).unlink()
    home_icons()
    print("brand assets written")


if __name__ == "__main__":
    main()
