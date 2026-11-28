#!/usr/bin/env python3
# PS5Upload Play - the app's words in other languages.
#
# English lives in src/ui/strings.hpp (PLAY_STRINGS); each other language is i18n/<code>.json,
# {"<id>": "<text>"}. This tool:
#   export              writes i18n/en.json from strings.hpp (the list translators work from)
#   check               every id in every language, the same printf conversions, the app's
#                       name kept, and no string longer than the screen can take
#   build OUTDIR        (in the build image) assets/i18n/<code>.bin for each language, and
#                       OUTDIR/atlas-chars.txt: the characters the UI font must have
#
# Languages whose letters the UI font has (Latin, Cyrillic, Greek) are drawn from the font.
# The others (CJK, Arabic, Persian, Hindi, Bengali, Thai) are shaped here, at build time, with
# Noto fonts and Pillow's raqm layout, into "runs": a run is a word (scripts with spaces) or a
# character cluster (scripts without), pre-rendered at each UI text size, and the translated
# string refers to it as U+E000 + 4 hex digits. This is PS5Term's approach
# (tools/generate-i18n.py, GPL-3.0-or-later), cut to word/cluster runs so text still wraps.
#
# <code>.bin: "PPL1", u32 strings, u32 runs, u32 flags (1: right to left), u32 sizes (3),
#   u32 size px [sizes], then strings x (u32 offset, u32 length) into the text blob (length 0:
#   English), then runs x sizes x (u16 width, u16 height, u16 baseline, u16 0, u32 offset,
#   u32 length) into the pixel blob, then the text blob, then the pixel blob. Pixels: one byte
#   per step, level (0-15) << 4 | (repeat - 1), row by row.
# SPDX-License-Identifier: GPL-3.0-or-later
import hashlib
import json
import re
import struct
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
I18N = ROOT / "i18n"
STRINGS = ROOT / "src/ui/strings.hpp"
PRINTF = re.compile(r"%(?:%|[-+ #0]*\d*(?:\.\d+)?(?:hh|h|ll|l|z|j|t)?[diouxXcsfeEgGp])")
# The app's own name stays as it is in every language.
KEEP = {"app_name": "PS5Upload Play"}
# The 21 languages PS5Upload has (client/src/i18n/locales), with each one's own name.
LANGUAGES = {
    "en": "English", "ar": "العربية", "bn": "বাংলা", "de": "Deutsch", "es": "Español", "fa": "فارسی",
    "fr": "Français", "hi": "हिन्दी", "hu": "Magyar", "id": "Bahasa Indonesia", "it": "Italiano",
    "ja": "日本語", "ko": "한국어", "pl": "Polski", "pt-BR": "Português (Brasil)", "ru": "Русский",
    "th": "ไทย", "tr": "Türkçe", "vi": "Tiếng Việt", "zh-CN": "简体中文", "zh-TW": "繁體中文",
}
# Shaped at build time: font, shaping language, direction, run unit.
SHAPED = {
    "ar": ("NotoSansArabic[wdth,wght].ttf", "ar", "rtl", "word"),
    "fa": ("NotoSansArabic[wdth,wght].ttf", "fa", "rtl", "word"),
    "hi": ("NotoSansDevanagari[wdth,wght].ttf", "hi", "ltr", "word"),
    "bn": ("NotoSansBengali[wdth,wght].ttf", "bn", "ltr", "word"),
    "ko": ("NotoSansKR[wght].ttf", "ko", "ltr", "word"),
    "th": ("NotoSansThai[wdth,wght].ttf", "th", "ltr", "cluster"),
    "ja": ("NotoSansJP[wght].ttf", "ja", "ltr", "cluster"),
    "zh-CN": ("NotoSansSC[wght].ttf", "zh-Hans", "ltr", "cluster"),
    "zh-TW": ("NotoSansTC[wght].ttf", "zh-Hant", "ltr", "cluster"),
}
# The UI's text sizes that translated text uses (canvas Font small, body, title) and where the
# baseline sits in each cell (tools/make-font.py: cell 1.32 x size, baseline 1.02 x size).
SIZES = [24, 32, 44]
MARKER = ""


def english():
    """(id, text) in PLAY_STRINGS order."""
    src = STRINGS.read_text(encoding="utf-8")
    out = []
    for m in re.finditer(r'X\((\w+),\s*((?:"(?:[^"\\]|\\.)*"\s*)+)\)', src):
        parts = re.findall(r'"((?:[^"\\]|\\.)*)"', m.group(2))
        # the strings use only \" and \\ escapes
        text = re.sub(r'\\(["\\])', r"\1", "".join(parts))
        out.append((m.group(1), text))
    return out


def load(code):
    return json.loads((I18N / f"{code}.json").read_text(encoding="utf-8"))


def export():
    data = {k: v for k, v in english()}
    (I18N / "en.json").write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"i18n/en.json: {len(data)} strings")


def check():
    eng = english()
    ids = [k for k, _ in eng]
    problems = []
    files = sorted(p.stem for p in I18N.glob("*.json"))
    missing = sorted(set(LANGUAGES) - set(files))
    if missing:
        problems.append(f"no file for: {', '.join(missing)}")
    if load("en") != dict(eng):
        problems.append("i18n/en.json is not strings.hpp's English: run tools/i18n.py export")
    for code in files:
        if code not in LANGUAGES:
            problems.append(f"{code}.json: not one of PS5Upload's languages")
            continue
        t = load(code)
        for k in ids:
            if k not in t or not isinstance(t[k], str) or not t[k].strip():
                problems.append(f"{code}: missing {k}")
        for k in t:
            if k not in ids:
                problems.append(f"{code}: unknown id {k}")
        for k, e in eng:
            if k not in t:
                continue
            if PRINTF.findall(t[k]) != PRINTF.findall(e):
                problems.append(f"{code}: {k} has {PRINTF.findall(t[k])}, English {PRINTF.findall(e)}")
            if k in KEEP and t[k] != KEEP[k]:
                problems.append(f"{code}: {k} must stay \"{KEEP[k]}\"")
            if "PS5Upload" in e and "PS5Upload" not in t[k]:
                problems.append(f"{code}: {k} lost the name PS5Upload")
            if MARKER in t[k] or "" in t[k]:
                problems.append(f"{code}: {k} has a private-use character")
    for p in problems:
        print("i18n:", p)
    print(f"i18n: {len(files)} languages x {len(ids)} strings, {len(problems)} problems")
    return 1 if problems else 0


# ---- build ----------------------------------------------------------------------------------
def plain_char(ch):
    """Drawn from the UI font: ASCII, Latin, Greek, Cyrillic, punctuation the strings use."""
    o = ord(ch)
    return (o < 0x2e80 and not (0x0590 <= o < 0x1e00) and not (0x2000 <= o < 0x2010)) or ch in "–—‘’“”•…›"


def split_units(text, unit):
    """A translation as [(kind, piece)]: 'fmt' (printf), 'plain' (UI font) or 'run'."""
    out = []
    pos = 0
    pieces = []
    for m in PRINTF.finditer(text):
        if m.start() > pos:
            pieces.append(text[pos:m.start()])
        pieces.append(m.group(0))
        pos = m.end()
    if pos < len(text):
        pieces.append(text[pos:])
    for piece in pieces:
        if PRINTF.fullmatch(piece):
            out.append(("fmt", piece))
            continue
        i = 0
        while i < len(piece):
            ch = piece[i]
            if plain_char(ch):
                j = i
                while j < len(piece) and plain_char(piece[j]):
                    j += 1
                out.append(("plain", piece[i:j]))
                i = j
                continue
            j = i + 1
            if unit == "word":
                # a word: up to the next space or plain character
                while j < len(piece) and not plain_char(piece[j]):
                    j += 1
            else:
                # one character with its combining marks (a cluster); Thai's leading vowels
                # (U+0E40-0E44) stay with the consonant after them, and SARA AM (U+0E33, whose
                # ring sits over the consonant before it) with the one before
                if "เ" <= ch <= "ไ" and j < len(piece) and not plain_char(piece[j]):
                    j += 1
                while j < len(piece) and (unicodedata.category(piece[j]) in ("Mn", "Mc", "Me")
                                          or piece[j] == "ำ"):
                    j += 1
            out.append(("run", piece[i:j]))
            i = j
    return out


class Shaper:
    def __init__(self, fonts_dir, code):
        from PIL import ImageFont, features
        if not features.check("raqm"):
            sys.exit("Pillow without raqm cannot shape text")
        file, self.language, self.direction, self.unit = SHAPED[code]
        self.fonts = [ImageFont.truetype(str(fonts_dir / file), size * 2, layout_engine=ImageFont.Layout.RAQM)
                      for size in SIZES]
        self.runs, self.index = [], {}

    def run(self, text):
        if text not in self.index:
            self.index[text] = len(self.runs)
            self.runs.append(self.render(text))
        return self.index[text]

    def render(self, text):
        """The text shaped and drawn at each size: [(width, height, baseline, pixels)]."""
        from PIL import Image, ImageDraw
        rendered = []
        for size, font in zip(SIZES, self.fonts):
            cell_h = round(size * 1.32)
            base = round(size * 1.02)
            opts = dict(direction=self.direction, language=self.language)
            width = max(1, int(round(font.getlength(text, **opts))))
            img = Image.new("L", (width, cell_h * 2))
            ImageDraw.Draw(img).text((0, base * 2), text, font=font, fill=255, anchor="ls", **opts)
            small = img.resize(((width + 1) // 2, cell_h), Image.Resampling.BOX)
            levels = bytes((v * 15 + 127) // 255 for v in small.tobytes())
            enc, i = bytearray(), 0
            while i < len(levels):
                j = i
                while j < len(levels) and j - i < 16 and levels[j] == levels[i]:
                    j += 1
                enc.append(levels[i] << 4 | (j - i - 1))
                i = j
            rendered.append((small.width, small.height, base, bytes(enc)))
        return rendered


def build(out_dir):
    out_dir = Path(out_dir)
    assets = out_dir / "i18n"
    assets.mkdir(parents=True, exist_ok=True)
    fonts_dir = ROOT / ".deps/fonts"
    eng = english()
    atlas = set()
    for code in LANGUAGES:
        if code == "en":
            continue
        t = load(code)
        shaper = Shaper(fonts_dir, code) if code in SHAPED else None
        texts = []
        for k, e in eng:
            value = t.get(k, "")
            if k in KEEP:
                value = KEEP[k]
            if not shaper:
                atlas.update(ch for ch in value if ord(ch) >= 0x80)
                texts.append(value.encode("utf-8"))
                continue
            parts = []
            for kind, piece in split_units(value, shaper.unit):
                if kind == "run":
                    parts.append(f"{MARKER}{shaper.run(piece):04x}")
                else:
                    if kind == "plain":
                        atlas.update(ch for ch in piece if ord(ch) >= 0x80)
                    parts.append(piece)
            texts.append("".join(parts).encode("utf-8"))
        runs = shaper.runs if shaper else []
        if len(runs) > 0xFFFF:
            sys.exit(f"{code}: too many runs")
        data = pack(texts, runs, shaper is not None and shaper.direction == "rtl")
        (assets / f"{code}.bin").write_bytes(data)
        print(f"i18n: {code}: {len(texts)} strings, {len(runs)} runs, {len(data) // 1024} KiB")
    # names.bin: each language's own name, for the Settings row (runs marked U+E002).
    names, name_runs = [], []
    for code, name in LANGUAGES.items():
        if code in SHAPED:
            names.append(f"\ue002{len(name_runs):04x}".encode("utf-8"))
            name_runs.append(Shaper(fonts_dir, code).render(name))
        else:
            atlas.update(ch for ch in name if ord(ch) >= 0x80)
            names.append(name.encode("utf-8"))
    (assets / "names.bin").write_bytes(pack(names, name_runs, False))
    (assets / "languages.txt").write_text("\n".join(LANGUAGES) + "\n", encoding="utf-8")
    (out_dir / "atlas-chars.txt").write_text("".join(sorted(atlas)), encoding="utf-8")


def pack(texts, runs, rtl):
    head = struct.pack("<4sIIII", b"PPL1", len(texts), len(runs), 1 if rtl else 0, len(SIZES)) + \
        struct.pack(f"<{len(SIZES)}I", *SIZES)
    table, blob = bytearray(), bytearray()
    for b in texts:
        table += struct.pack("<II", len(blob), len(b))
        blob += b
    rtable, pixels = bytearray(), bytearray()
    for rendered in runs:
        for w, h, base, enc in rendered:
            rtable += struct.pack("<HHHHII", w, h, base, 0, len(pixels), len(enc))
            pixels += enc
    return head + bytes(table) + bytes(rtable) + bytes(blob) + bytes(pixels)


def fonts_digest():
    return hashlib.sha256((ROOT / "third_party/noto/SOURCE.json").read_bytes()).hexdigest()


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "check"
    if cmd == "export":
        export()
    elif cmd == "check":
        sys.exit(check())
    elif cmd == "build":
        build(sys.argv[2])
    else:
        sys.exit("usage: i18n.py export | check | build OUTDIR")
