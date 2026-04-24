# scripts/bundle-fonts.py
#
# Download Noto font subsets from Google Fonts and bundle them into
# the Tauri desktop client so CJK + Indic scripts render without a
# network fetch at runtime. Outputs woff2 files into
# `client/src/assets/fonts/` and a companion `client/src/fonts.css`
# with @font-face rules that reference them via Vite's import-
# relative URLs.
#
# Run from the repo root:  python3 scripts/bundle-fonts.py
#
# The current build doesn't import fonts.css from main.tsx by
# default — system fonts cover the English UI. Run this script
# (and accept the injected import) when you're ready to ship the
# broader i18n font coverage.
import os
import re
import urllib.request
import urllib.parse
from urllib.error import URLError, HTTPError

REPO_ROOT = os.getcwd()
OUT_DIR = os.path.join(REPO_ROOT, "client", "src", "assets", "fonts")
CSS_OUT = os.path.join(REPO_ROOT, "client", "src", "fonts.css")
MAIN_TSX = os.path.join(REPO_ROOT, "client", "src", "main.tsx")

FAMILIES = [
    ("Noto Sans", [400, 500, 600, 700]),
    ("Noto Sans Devanagari", [400, 500, 600, 700]),
    ("Noto Sans Bengali", [400, 500, 600, 700]),
    ("Noto Sans Thai", [400, 500, 600, 700]),
    ("Noto Sans KR", [400, 500, 600, 700]),
]

HEADERS = {
    "User-Agent": (
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
        "AppleWebKit/537.36 (KHTML, like Gecko) "
        "Chrome/120.0.0.0 Safari/537.36"
    ),
    "Accept": "text/css,*/*;q=0.1",
}


def fetch_text(url: str) -> str:
    req = urllib.request.Request(url, headers=HEADERS)
    with urllib.request.urlopen(req) as res:
        return res.read().decode("utf-8")


def fetch_binary(url: str) -> bytes:
    req = urllib.request.Request(url, headers=HEADERS)
    with urllib.request.urlopen(req) as res:
        return res.read()


def slugify(name: str) -> str:
    return re.sub(r"(^-|-$)", "", re.sub(r"[^a-z0-9]+", "-", name.lower()))


def ensure_fonts_import() -> None:
    """Inject `import "./fonts.css"` into main.tsx right after the
    existing `./index.css` import. Idempotent — re-running is a
    no-op if the import is already present. Skips silently when
    main.tsx isn't at the expected path (dev running from a branch
    with a different layout)."""
    if not os.path.exists(MAIN_TSX):
        return
    with open(MAIN_TSX, "r", encoding="utf-8") as f:
        text = f.read()
    if './fonts.css' in text:
        return
    anchor = 'import "./index.css";'
    if anchor not in text:
        return
    text = text.replace(anchor, f'{anchor}\nimport "./fonts.css";')
    with open(MAIN_TSX, "w", encoding="utf-8") as f:
        f.write(text)


def ext_from_format(fmt: str, url: str) -> str:
    fmt = fmt.lower()
    if "woff2" in fmt:
        return "woff2"
    if "woff" in fmt:
        return "woff"
    if "truetype" in fmt or "ttf" in fmt:
        return "ttf"
    if "opentype" in fmt or "otf" in fmt:
        return "otf"
    url_ext = os.path.splitext(url.split("?")[0])[1].lstrip(".")
    return url_ext or "bin"


def main() -> None:
    os.makedirs(OUT_DIR, exist_ok=True)
    url_to_file = {}
    css_blocks = []
    total_blocks = 0

    for family, weights in FAMILIES:
        weights_str = ";".join(str(w) for w in weights)
        family_param = urllib.parse.quote(family).replace("%20", "+")
        url = (
            "https://fonts.googleapis.com/css2?family="
            f"{family_param}:wght@{weights_str}&display=swap"
        )
        try:
            css = fetch_text(url)
        except (HTTPError, URLError) as exc:
            print(f"Failed to fetch CSS for {family}: {exc}")
            continue

        blocks = re.findall(r"@font-face\s*\{[\s\S]*?\}", css)
        if not blocks:
            print(f"No @font-face blocks for {family}. First 200 chars:\n{css[:200]}")
        for block in blocks:
            total_blocks += 1
            match = re.search(
                r"url\(([^)]+)\)\s*format\(['\"]([^'\"]+)['\"]\)",
                block,
            )
            if not match:
                continue
            font_url = match.group(1)
            font_fmt = match.group(2)

            filename = url_to_file.get(font_url)
            if not filename:
                weight_match = re.search(r"font-weight:\s*([0-9]+)", block)
                weight = weight_match.group(1) if weight_match else "400"
                style_match = re.search(r"font-style:\s*([^;]+)", block)
                style = style_match.group(1).strip() if style_match else "normal"
                family_slug = slugify(family)
                index = len(url_to_file) + 1
                ext = ext_from_format(font_fmt, font_url)
                filename = f"{family_slug}-w{weight}-{style}-{index}.{ext}"
                try:
                    font_data = fetch_binary(font_url)
                except (HTTPError, URLError) as exc:
                    print(f"Failed to fetch font {font_url}: {exc}")
                    continue
                with open(os.path.join(OUT_DIR, filename), "wb") as f:
                    f.write(font_data)
                url_to_file[font_url] = filename

            local_block = block.replace(font_url, f"../assets/fonts/{filename}")
            css_blocks.append(local_block)

    header = "/* Bundled fonts for multilingual UI */\n"
    with open(CSS_OUT, "w", encoding="utf-8") as f:
        f.write(header + "\n\n".join(css_blocks) + "\n")

    ensure_fonts_import()
    print(
        f"Done. Fonts: {len(url_to_file)} files, "
        f"{len(css_blocks)} @font-face blocks (parsed {total_blocks} blocks)."
    )


if __name__ == "__main__":
    main()
