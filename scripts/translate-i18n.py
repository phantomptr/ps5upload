# scripts/translate-i18n.py
# Auto-translate i18n strings using LibreTranslate (or compatible) API.
# Usage:
#   python3 scripts/translate-i18n.py
# Optional env:
#   TRANSLATE_URL (default: https://libretranslate.com/translate)
#   TRANSLATE_API_KEY (default: "")
#   TRANSLATE_BATCH (default: 20)
#   TRANSLATE_LANGS (comma list of codes, default new languages)
#   TRANSLATE_SLEEP (seconds between requests, default 0.2)

import json
import os
import re
import time
import urllib.parse
import urllib.request
from typing import Dict, List, Tuple

REPO_ROOT = os.getcwd()
I18N_PATH = os.path.join(REPO_ROOT, "desktop", "src", "i18n.ts")

DEFAULT_LANGS = [
    "vi", "hi", "bn", "pt-BR", "ru", "ja", "tr", "id", "th", "ko", "de", "it"
]

TRANSLATE_URL = os.environ.get("TRANSLATE_URL", "")
TRANSLATE_URLS = os.environ.get(
    "TRANSLATE_URLS",
    "https://libretranslate.de/translate,https://translate.astian.org/translate",
)
TRANSLATE_API_KEY = os.environ.get("TRANSLATE_API_KEY", "")
BATCH = int(os.environ.get("TRANSLATE_BATCH", "20"))
SLEEP = float(os.environ.get("TRANSLATE_SLEEP", "0.2"))
LANGS = os.environ.get("TRANSLATE_LANGS")
if LANGS:
    TARGET_LANGS = [l.strip() for l in LANGS.split(",") if l.strip()]
else:
    TARGET_LANGS = DEFAULT_LANGS

CACHE_PATH = os.path.join(REPO_ROOT, "scripts", ".translate-cache.json")


def load_cache() -> Dict[str, str]:
    if not os.path.exists(CACHE_PATH):
        return {}
    with open(CACHE_PATH, "r", encoding="utf-8") as f:
        return json.load(f)


def save_cache(cache: Dict[str, str]) -> None:
    with open(CACHE_PATH, "w", encoding="utf-8") as f:
        json.dump(cache, f, ensure_ascii=False, indent=2)


def extract_en_block(text: str) -> Tuple[Dict[str, str], int, int]:
    marker = "\n  en: {"
    start = text.find(marker)
    if start == -1:
        raise RuntimeError("en block not found")
    i = start + len(marker)
    depth = 1
    end = -1
    while i < len(text):
        ch = text[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        if depth == 0:
            end = i
            break
        i += 1
    if end == -1:
        raise RuntimeError("en block not closed")
    block = text[start + len(marker):end]
    # parse key: "value"
    pattern = re.compile(r"^\s*([a-zA-Z0-9_]+):\s*\"((?:\\\\.|[^\"])*)\",", re.MULTILINE)
    items = pattern.findall(block)
    if not items:
        raise RuntimeError("Failed to parse en block")
    en = {k: bytes(v, "utf-8").decode("unicode_escape") for k, v in items}
    return en, start, end


def protect_placeholders(text: str) -> Tuple[str, Dict[str, str]]:
    placeholders = {}
    def repl(match):
        key = f"__VAR{len(placeholders)}__"
        placeholders[key] = match.group(0)
        return key
    protected = re.sub(r"\{\w+\}", repl, text)
    return protected, placeholders


def restore_placeholders(text: str, placeholders: Dict[str, str]) -> str:
    for key, val in placeholders.items():
        text = text.replace(key, val)
    return text


def translate_batch(texts: List[str], target: str) -> List[str]:
    urls = [u.strip() for u in TRANSLATE_URLS.split(",") if u.strip()]
    if TRANSLATE_URL:
        urls = [TRANSLATE_URL]
    data = {
        "q": texts,
        "source": "en",
        "target": target,
        "format": "text",
    }
    if TRANSLATE_API_KEY:
        data["api_key"] = TRANSLATE_API_KEY
    encoded = urllib.parse.urlencode(data, doseq=True).encode("utf-8")
    last_error = None
    for url in urls:
        req = urllib.request.Request(
            url,
            data=encoded,
            headers={
                "Content-Type": "application/x-www-form-urlencoded",
                "User-Agent": "ps5upload-i18n/1.0",
            },
        )
        try:
            with urllib.request.urlopen(req) as res:
                payload = res.read().decode("utf-8")
            break
        except Exception as exc:
            last_error = exc
            payload = None
    if payload is None:
        raise last_error
    try:
        parsed = json.loads(payload)
    except json.JSONDecodeError:
        return [payload]
    # LibreTranslate returns list when q is list, or dict for single.
    if isinstance(parsed, list):
        return [item.get("translatedText", "") for item in parsed]
    if isinstance(parsed, dict) and "translatedText" in parsed:
        return [parsed["translatedText"]]
    return [payload]


def escape_ts(value: str) -> str:
    return (
        value
        .replace("\\", "\\\\")
        .replace("\"", "\\\"")
        .replace("\n", "\\n")
    )


def render_block(lang: str, items: Dict[str, str]) -> str:
    lines = [f"  \"{lang}\": {{"]
    for key, value in items.items():
        lines.append(f"    {key}: \"{escape_ts(value)}\",")
    lines.append("  },")
    return "\n".join(lines)


def replace_block(text: str, lang: str, block_str: str) -> str:
    # Replace existing block if present, otherwise insert before zh-CN.
    pattern = re.compile(rf"\n\s*\"{re.escape(lang)}\": \{{[\s\S]*?\n\s*\}},", re.MULTILINE)
    if pattern.search(text):
        return pattern.sub("\n" + block_str, text, count=1)
    zh_idx = text.find("\n  \"zh-CN\": {")
    if zh_idx == -1:
        raise RuntimeError("zh-CN block not found for insertion")
    return text[:zh_idx] + "\n" + block_str + text[zh_idx:]


def main() -> None:
    with open(I18N_PATH, "r", encoding="utf-8") as f:
        text = f.read()

    en, _, _ = extract_en_block(text)
    keys = list(en.keys())

    cache = load_cache()

    for lang in TARGET_LANGS:
        print(f"Translating: {lang} ({len(keys)} strings)")
        out: Dict[str, str] = {}
        batch = []
        batch_keys = []
        for key in keys:
            src = en[key]
            cache_key = f"{lang}:{src}"
            if cache_key in cache:
                out[key] = cache[cache_key]
                continue
            protected, placeholders = protect_placeholders(src)
            batch.append((protected, placeholders))
            batch_keys.append(key)
            if len(batch) >= BATCH:
                texts = [b[0] for b in batch]
                translated = translate_batch(texts, lang)
                for (prot, ph), k, t in zip(batch, batch_keys, translated):
                    restored = restore_placeholders(t, ph)
                    cache_key = f"{lang}:{en[k]}"
                    cache[cache_key] = restored
                    out[k] = restored
                batch.clear()
                batch_keys.clear()
                save_cache(cache)
                time.sleep(SLEEP)
        if batch:
            texts = [b[0] for b in batch]
            translated = translate_batch(texts, lang)
            for (prot, ph), k, t in zip(batch, batch_keys, translated):
                restored = restore_placeholders(t, ph)
                cache_key = f"{lang}:{en[k]}"
                cache[cache_key] = restored
                out[k] = restored
            batch.clear()
            batch_keys.clear()
            save_cache(cache)

        block = render_block(lang, out)
        text = replace_block(text, lang, block)

    with open(I18N_PATH, "w", encoding="utf-8") as f:
        f.write(text)

    print("Done: updated i18n.ts")


if __name__ == "__main__":
    main()
