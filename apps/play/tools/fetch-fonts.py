#!/usr/bin/env python3
"""Fetch the pinned Noto fonts that tools/i18n.py shapes translations with (copied from PS5Term's tools/fetch-ui-fonts.py).

SPDX-License-Identifier: GPL-3.0-or-later

Downloads each file in third_party/noto/SOURCE.json into the ignored .deps/fonts/ and checks
its SHA-256. Used only at build time; the fonts are not shipped in the app.
"""
import hashlib
import json
import sys
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / '.deps' / 'fonts'


def main():
    manifest = json.loads((ROOT / 'third_party' / 'noto' / 'SOURCE.json').read_text())
    DEST.mkdir(parents=True, exist_ok=True)
    for entry in manifest['files']:
        target = DEST / entry['file']
        if target.exists() and hashlib.sha256(target.read_bytes()).hexdigest() == entry['sha256']:
            continue
        head, _, name = entry['url'].rpartition('/')
        with urllib.request.urlopen(f'{head}/{urllib.parse.quote(name)}', timeout=120) as response:
            data = response.read()
        if hashlib.sha256(data).hexdigest() != entry['sha256']:
            sys.exit(f'{entry["file"]}: SHA-256 mismatch')
        target.write_bytes(data)
    print(f'{len(manifest["files"])} fonts in {DEST.relative_to(ROOT)}')


if __name__ == '__main__':
    main()
