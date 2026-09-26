# scripts/translate-i18n.py
# Auto-translate missing i18n strings using LibreTranslate (or a
# compatible API).
#
# Locale-split aware: the old monolithic `client/src/i18n.ts` (one
# `const translations = { en: {...}, "zh-CN": {...} }` block) is gone.
# Each locale now lives in its own file under
# `client/src/i18n/locales/<code>.ts`, shaped:
#
#     const <id>: Translations = {
#       key: "value",
#       ...
#     };
#     export default <id>;
#
# This script reads `en.ts` as the canonical key set, and for every
# other locale file fills ONLY the keys that locale is missing
# (idempotent — re-runs skip already-translated keys). New keys are
# appended just before the closing `};`.
#
# Usage:
#   python3 scripts/translate-i18n.py
# Optional env:
#   TRANSLATE_URL       single endpoint override
#   TRANSLATE_URLS      comma list of endpoints to try in order
#   TRANSLATE_API_KEY   api key, if the endpoint needs one
#   TRANSLATE_BATCH     strings per request (default 20)
#   TRANSLATE_LANGS     comma list of locale codes (default: all
#                       non-en locale files found on disk)
#   TRANSLATE_SLEEP     seconds between requests (default 0.2)
#
# After running, re-run `node scripts/i18n-coverage.mjs` to confirm
# the coverage gate is satisfied.

import json
import os
import re
import time
import urllib.parse
import urllib.request
from typing import Dict, List, Tuple

REPO_ROOT = os.getcwd()
LOCALES_DIR = os.path.join(REPO_ROOT, "client", "src", "i18n", "locales")
EN_PATH = os.path.join(LOCALES_DIR, "en.ts")

TRANSLATE_URL = os.environ.get("TRANSLATE_URL", "")
TRANSLATE_URLS = os.environ.get(
    "TRANSLATE_URLS",
    "https://libretranslate.com/translate,https://translate.terraprint.co/translate",
)
TRANSLATE_API_KEY = os.environ.get("TRANSLATE_API_KEY", "")
BATCH = int(os.environ.get("TRANSLATE_BATCH", "20"))
SLEEP = float(os.environ.get("TRANSLATE_SLEEP", "0.2"))

CACHE_PATH = os.path.join(REPO_ROOT, "scripts", ".translate-cache.json")

# Matches one `  key: "value",` entry per line. The key is either a
# bare identifier (`upload_title`) or a quoted string (`"install.phase
# .staging"` — dotted keys must be quoted to stay valid TS). Values may
# contain escaped quotes (\") and escaped backslashes (\\); template
# literals and multi-line values are not used in these generated
# locale files.
ENTRY_RE = re.compile(
    r'^\s*(?:([A-Za-z0-9_$]+)|"((?:\\.|[^"\\])*)")\s*:\s*'
    r'"((?:\\.|[^"\\])*)"\s*,?\s*$',
    re.MULTILINE,
)


def bare_identifier(key: str) -> bool:
    return re.fullmatch(r"[A-Za-z_$][A-Za-z0-9_$]*", key) is not None


def render_key(key: str) -> str:
    """Object-literal key: bare identifiers as-is, everything else
    (dotted / hyphenated keys) quoted so the TS stays valid."""
    return key if bare_identifier(key) else f'"{escape_ts(key)}"'


def load_cache() -> Dict[str, str]:
    if not os.path.exists(CACHE_PATH):
        return {}
    with open(CACHE_PATH, "r", encoding="utf-8") as f:
        return json.load(f)


def save_cache(cache: Dict[str, str]) -> None:
    with open(CACHE_PATH, "w", encoding="utf-8") as f:
        json.dump(cache, f, ensure_ascii=False, indent=2)


def locale_files() -> List[str]:
    return sorted(
        f
        for f in os.listdir(LOCALES_DIR)
        if f.endswith(".ts") and not f.endswith(".d.ts")
    )


def split_literal(text: str) -> Tuple[str, str, str]:
    """Split a locale file into (head, body, tail) where `body` is the
    object-literal contents between `const <id>: Translations = {` and
    the closing `};`. New entries are appended to the end of `body`."""
    open_match = re.search(
        r"const\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Translations\s*=\s*\{",
        text,
    )
    if not open_match:
        raise RuntimeError("could not find 'const <name>: Translations = {'")
    body_start = open_match.end()
    close_match = re.search(r"\n\};\s*\n\s*export default", text[body_start:])
    if not close_match:
        raise RuntimeError("could not find end of literal (};\\nexport default)")
    body_end = body_start + close_match.start()
    return text[:body_start], text[body_start:body_end], text[body_end:]


def _decode_ts(raw: str) -> str:
    return raw.replace('\\"', '"').replace("\\n", "\n").replace("\\\\", "\\")


def parse_entries(body: str) -> Dict[str, str]:
    """Parse `key: "value"` entries from a literal body. Handles both
    bare-identifier and quoted keys. Values are returned with TS escape
    sequences decoded to real characters."""
    out: Dict[str, str] = {}
    for bare_key, quoted_key, raw in ENTRY_RE.findall(body):
        key = bare_key if bare_key else _decode_ts(quoted_key)
        out[key] = _decode_ts(raw)
    return out


def escape_ts(value: str) -> str:
    return (
        value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
    )


def protect_placeholders(text: str) -> Tuple[str, Dict[str, str]]:
    placeholders: Dict[str, str] = {}

    def repl(match):
        key = f"__VAR{len(placeholders)}__"
        placeholders[key] = match.group(0)
        return key

    return re.sub(r"\{\w+\}", repl, text), placeholders


def restore_placeholders(text: str, placeholders: Dict[str, str]) -> str:
    for key, val in placeholders.items():
        text = text.replace(key, val)
    return text


def translate_batch(texts: List[str], target: str) -> List[str]:
    urls = [u.strip() for u in TRANSLATE_URLS.split(",") if u.strip()]
    if TRANSLATE_URL:
        urls = [TRANSLATE_URL]
    data = {"q": texts, "source": "en", "target": target, "format": "text"}
    if TRANSLATE_API_KEY:
        data["api_key"] = TRANSLATE_API_KEY
    encoded = urllib.parse.urlencode(data, doseq=True).encode("utf-8")
    last_error = None
    payload = None
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
            with urllib.request.urlopen(req, timeout=30) as res:
                payload = res.read().decode("utf-8")
            break
        except Exception as exc:  # noqa: BLE001 - try the next endpoint
            last_error = exc
            payload = None
    if payload is None:
        raise RuntimeError(f"all translate endpoints failed: {last_error}")
    try:
        parsed = json.loads(payload)
    except json.JSONDecodeError:
        return [payload]
    # LibreTranslate returns a list when `q` is a list, or a dict for
    # a single string.
    if isinstance(parsed, list):
        return [item.get("translatedText", "") for item in parsed]
    if isinstance(parsed, dict) and "translatedText" in parsed:
        translated = parsed["translatedText"]
        if isinstance(translated, list):
            return translated
        return [translated]
    raise RuntimeError(f"unexpected translate response shape: {payload[:200]}")


def main() -> None:
    with open(EN_PATH, "r", encoding="utf-8") as f:
        en_text = f.read()
    _, en_body, _ = split_literal(en_text)
    en = parse_entries(en_body)
    if not en:
        raise RuntimeError("en.ts: parsed zero entries")

    env_langs = os.environ.get("TRANSLATE_LANGS")
    if env_langs:
        target_codes = [c.strip() for c in env_langs.split(",") if c.strip()]
    else:
        target_codes = [
            f[:-3] for f in locale_files() if f != "en.ts"
        ]

    cache = load_cache()
    total_added = 0

    for code in target_codes:
        fp = os.path.join(LOCALES_DIR, f"{code}.ts")
        if not os.path.exists(fp):
            print(f"skip {code}: {fp} not found")
            continue
        with open(fp, "r", encoding="utf-8") as f:
            text = f.read()
        head, body, tail = split_literal(text)
        existing = parse_entries(body)
        missing = [k for k in en if k not in existing]
        if not missing:
            print(f"{code}: up to date")
            continue
        print(f"{code}: {len(missing)} missing")

        additions: Dict[str, str] = {}
        i = 0
        while i < len(missing):
            chunk = missing[i : i + BATCH]
            i += BATCH
            to_translate = []
            ph_maps = []
            chunk_keys = []
            for key in chunk:
                cache_key = f"{code}:{en[key]}"
                if cache_key in cache:
                    additions[key] = cache[cache_key]
                    continue
                protected, ph = protect_placeholders(en[key])
                to_translate.append(protected)
                ph_maps.append(ph)
                chunk_keys.append(key)
            if not to_translate:
                continue
            translated = translate_batch(to_translate, code)
            for key, ph, t in zip(chunk_keys, ph_maps, translated):
                restored = restore_placeholders(t, ph)
                additions[key] = restored
                cache[f"{code}:{en[key]}"] = restored
            save_cache(cache)
            time.sleep(SLEEP)

        # Append the new entries to the end of the literal body. Make
        # sure the existing body ends with a comma first — generated
        # locale files are inconsistent about the trailing comma.
        body_trimmed = body.rstrip()
        if body_trimmed and not body_trimmed.endswith((",", "{")):
            body_trimmed += ","
        new_lines = "\n".join(
            f'{render_key(key)}: "{escape_ts(additions[key])}",'
            for key in missing
            if key in additions
        )
        new_text = f"{head}{body_trimmed}\n{new_lines}\n{tail}"
        with open(fp, "w", encoding="utf-8") as f:
            f.write(new_text)
        total_added += len(additions)
        print(f"{code}: wrote {len(additions)} translations")

    print(f"Done: added {total_added} translations across {len(target_codes)} locales")


if __name__ == "__main__":
    main()
