#!/usr/bin/env python3
# PS5Upload Play - home-screen and in-app art, drawn procedurally.
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Usage: make-art.py BRANDING_PNG_DIR OUTDIR WORKDIR
#
# Writes to OUTDIR: icon0.png (512x512 tile), pic0.png / pic1.png (3840x2160
# previews of the DDS art), logo.rgba (white mark, 300 px tall) and
# ui-bg.rgba (1920x1080 in-app background). WORKDIR gets pic0.rgba and
# pic1.rgba, the raw input of the BC7 encoder. Output is deterministic.
#
# The .rgba files are: u32 LE width, u32 LE height, width*height RGBA8
# (straight alpha). The pic*.rgba work files are headerless.
import struct
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageFilter

NAVY_TOP = np.array([9, 11, 26], np.float32)
NAVY_BOTTOM = np.array([4, 5, 14], np.float32)
INDIGO = np.array([30, 26, 78], np.float32)
GLOW = np.array([72, 64, 168], np.float32)
TEAL = np.array([16, 54, 84], np.float32)
MARK = np.array([240, 242, 255], np.float32)
MARK_GLOW = np.array([110, 120, 255], np.float32)


def smoothstep(a, b, x):
    t = np.clip((x - a) / (b - a), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def blob(u, v, cx, cy, radius, aspect=1.0):
    """Soft gaussian spot in normalised coordinates (v scaled to height)."""
    return np.exp(-(((u - cx) * aspect) ** 2 + (v - cy) ** 2) / (radius * radius))


def background(w, h, glow=1.0, calm=1.0):
    """Deep navy field, indigo bloom on the right, dark left side, vignette."""
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    u, v = x / w, y / h
    a = w / h  # aspect so that spots stay round
    img = NAVY_TOP * (1 - v[..., None]) + NAVY_BOTTOM * v[..., None]
    img += INDIGO * (smoothstep(0.30, 1.0, u) * (1.0 - 0.4 * v))[..., None] * 0.55
    img += GLOW * (blob(u, v, 0.745, 0.47, 0.34, a) * 0.55 * glow)[..., None]
    img += GLOW * (blob(u, v, 0.80, 0.30, 0.16, a) * 0.18 * glow)[..., None]
    img += TEAL * (blob(u, v, 0.95, 0.95, 0.30, a) * 0.45)[..., None]
    # two faint diagonal sheens across the right half
    for c, width, s in ((1.05, 0.05, 0.06), (1.22, 0.09, 0.04)):
        d = u * 0.8 + v * 0.6 - c
        img += GLOW * (np.exp(-(d / width) ** 2) * smoothstep(0.45, 0.8, u) * s * glow)[..., None]
    # a few large, very soft bokeh discs on the right
    for cx, cy, r, s in ((0.60, 0.20, 0.07, 0.05), (0.92, 0.18, 0.05, 0.06),
                         (0.88, 0.72, 0.09, 0.04), (0.66, 0.82, 0.05, 0.035)):
        dist = np.sqrt(((u - cx) * a) ** 2 + (v - cy) ** 2)
        img += GLOW * ((1.0 - smoothstep(r * 0.75, r, dist)) * s * glow)[..., None]
    # keep the left side (title and Play button) dark, then vignette
    img *= (0.62 + 0.38 * smoothstep(0.30, 0.78, u))[..., None]
    vig = np.sqrt(((u - 0.6) * 1.1) ** 2 + ((v - 0.5) * 1.3) ** 2)
    img *= (1.0 - 0.55 * smoothstep(0.25, 0.95, vig))[..., None]
    return img * calm


def mark_alpha(branding, height):
    """Alpha of the white mark at the given height, resampled without fringes."""
    mark = Image.open(branding / "mark-white-1024.png").convert("RGBA")
    width = round(mark.width * height / mark.height)
    return mark.getchannel("A").resize((width, height), Image.LANCZOS)


def add_mark(img, branding, cx, cy, height, glow):
    h, w = img.shape[:2]
    alpha = mark_alpha(branding, height)
    canvas = Image.new("L", (w, h), 0)
    canvas.paste(alpha, (round(cx * w - alpha.width / 2), round(cy * h - height / 2)))
    halo = np.asarray(canvas.filter(ImageFilter.GaussianBlur(height * 0.06)), np.float32) / 255
    wide = np.asarray(canvas.filter(ImageFilter.GaussianBlur(height * 0.18)), np.float32) / 255
    img = img + MARK_GLOW * (halo * 0.16 + wide * 0.16)[..., None] * glow
    a = (np.asarray(canvas, np.float32) / 255)[..., None]
    return img * (1 - a) + MARK * a


def to_rgb8(img, seed):
    """Quantise with a fixed-seed triangular dither so gradients do not band."""
    rng = np.random.default_rng(seed)
    noise = rng.random(img.shape[:2], np.float32) - rng.random(img.shape[:2], np.float32)
    return np.clip(np.floor(img + noise[..., None] * 0.75 + 0.5), 0, 255).astype(np.uint8)


def rgba_file(path, rgba):
    h, w = rgba.shape[:2]
    path.write_bytes(struct.pack("<II", w, h) + np.ascontiguousarray(rgba).tobytes())


def opaque(rgb):
    return np.dstack([rgb, np.full(rgb.shape[:2], 255, np.uint8)])


def main():
    branding, out, work = (Path(p) for p in sys.argv[1:4])
    out.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)

    # icon0: the white logo tile flattened onto white, 512x512 RGB
    icon = Image.open(branding / "icon-1024.png").convert("RGBA")
    tile = Image.new("RGBA", icon.size, (255, 255, 255, 255))
    tile.alpha_composite(icon)
    tile.convert("RGB").resize((512, 512), Image.LANCZOS).save(out / "icon0.png", optimize=True)

    # pic0 (selection) and pic1 (launch): same scene, pic1 a little brighter
    for name, glow, height, seed in (("pic0", 1.0, 0.60, 1), ("pic1", 1.15, 0.64, 2)):
        img = add_mark(background(3840, 2160, glow), branding, 0.745, 0.49, round(2160 * height), glow)
        rgb = to_rgb8(img, seed)
        Image.fromarray(rgb).save(out / f"{name}.png")
        (work / f"{name}.rgba").write_bytes(opaque(rgb).tobytes())

    # in-app background: calmer, darker, no mark
    bg = to_rgb8(background(1920, 1080, glow=0.75, calm=0.8), 3)
    rgba_file(out / "ui-bg.rgba", opaque(bg))
    Image.fromarray(bg).save(work / "ui-bg.png")

    # logo: white mark, 300 px tall, straight alpha
    alpha = np.asarray(mark_alpha(branding, 300))
    rgba_file(out / "logo.rgba", np.dstack([np.full(alpha.shape + (3,), 255, np.uint8), alpha]))


if __name__ == "__main__":
    main()
