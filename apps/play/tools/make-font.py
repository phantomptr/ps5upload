#!/usr/bin/env python3
# PS5Upload Play - bake the UI font into antialiased glyph cells (assets/font.bin).
# Adapted from PS5Term's tools/generate-font-atlas.py (GPL-3.0-or-later): the same 2x render and
# Lanczos downscale on a common baseline, at TV sizes, written as a binary file the app loads.
# Glyph data: JetBrains Mono (SIL OFL 1.1, third_party/jetbrains-mono).
#
# font.bin: "PPF1", u32 sizes; per size: u32 px, cell_w, cell_h, baseline, count,
# u32 codes[count] (ascending), u8 coverage[count * cell_w * cell_h], zero padding to 4 bytes.
# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import json
import struct
import sys
from pathlib import Path

from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
FONT = ROOT / "third_party/jetbrains-mono"
SIZES = [24, 32, 44, 72, 112]
# Beyond ASCII for the text sizes: Latin-1 and the punctuation and arrows the strings use.
EXTRA = list(range(0xA0, 0x100)) + [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2026,
                                    0x2039, 0x203A, 0x2190, 0x2191, 0x2192, 0x2193, 0x2713, 0x2715,
                                    0x25A1, 0x25B3, 0x25CB, 0x25CF]


def main(out, extra_file=None):
    for rec in json.loads((FONT / "SOURCE.json").read_text())["files"]:
        if hashlib.sha256((FONT / rec["file"]).read_bytes()).hexdigest() != rec["sha256"]:
            raise SystemExit("pinned font source hash mismatch")
    path = FONT / "JetBrainsMono-Regular.ttf"
    have = set(TTFont(str(path)).getBestCmap())
    blob = bytearray(b"PPF1" + struct.pack("<I", len(SIZES)))
    for px in SIZES:
        font = ImageFont.truetype(str(path), px * 2)
        cell_w = round(px * 0.6)
        cell_h = round(px * 1.32)
        base = round(px * 1.02)
        codes = list(range(32, 127))
        if px <= 44:
            codes += [c for c in EXTRA if c in have]
            # every letter the translations draw from the font (tools/i18n.py build)
            used = {ord(ch) for ch in Path(extra_file).read_text(encoding="utf-8")} if extra_file else set()
            lacking = sorted(c for c in used if c not in have)
            if lacking:
                raise SystemExit("the UI font has no " + " ".join(f"U+{c:04X}" for c in lacking))
            codes += sorted(used)
        codes = sorted(set(codes))
        cov = bytearray()
        for code in codes:
            cell = Image.new("L", (cell_w * 2, cell_h * 2))
            ImageDraw.Draw(cell).text((0, base * 2), chr(code), font=font, fill=255, anchor="ls")
            cov += cell.resize((cell_w, cell_h), Image.Resampling.LANCZOS).tobytes()
        blob += struct.pack("<5I", px, cell_w, cell_h, base, len(codes))
        blob += struct.pack(f"<{len(codes)}I", *codes)
        blob += cov
        blob += bytes(-len(blob) % 4)  # the next size starts 4-byte aligned
    Path(out).write_bytes(blob)
    print(f"font.bin: {len(blob)} bytes, sizes {SIZES}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None)
