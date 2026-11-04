#!/usr/bin/env python3
"""Write the iOS app source (AltStore source format) for a release.

AltStore, SideStore and Feather add a "source" by URL and then offer its app's updates in the
app. Each release carries this file as `altstore.json`, so

    https://github.com/phantomptr/ps5upload/releases/latest/download/altstore.json

always describes the newest promoted release (a pre-release is never "latest"). The website
serves a copy at https://ps5upload.phantomptr.com/altstore.json (scripts/site-altstore.mjs), the
address the docs give people.

The app's identity (bundle id, version, minimum iOS) and the permissions it asks for are read
from the .ipa itself, so the source cannot disagree with what it points at.

The iOS build is not a release gate. A release without an .ipa must still have this file, or
every source user's updates stop with no message, so --previous (the last promoted release's
file) is carried forward then. With an .ipa, the previous versions stay listed behind the new one.

    gen-altstore-source.py --ipa release/PS5Upload-7.5.3-ios-unsigned.ipa --tag v7.5.3 \\
        --repo phantomptr/ps5upload --notes RELEASE_NOTES.md --previous old-altstore.json \\
        --out release/altstore.json
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import plistlib
import re
import sys
import zipfile
from pathlib import Path

SITE = "https://ps5upload.phantomptr.com"
ICON = f"{SITE}/assets/logo.png"
# Older versions kept in the source: enough to step back from a bad build, without the file
# growing with every release (each entry carries that release's notes).
MAX_VERSIONS = 5


def ipa_info(ipa: Path) -> dict:
    """The .app's Info.plist inside the .ipa."""
    with zipfile.ZipFile(ipa) as z:
        names = [n for n in z.namelist() if re.fullmatch(r"Payload/[^/]+\.app/Info\.plist", n)]
        if len(names) != 1:
            raise SystemExit(f"{ipa}: expected one Payload/*.app/Info.plist, found {len(names)}")
        return plistlib.loads(z.read(names[0]))


def privacy_of(info: dict) -> dict:
    """What the app asks permission for. AltStore compares this with the .ipa on iOS 16+ and warns
    on every install and update when a key is not declared."""
    return {k: v for k, v in sorted(info.items()) if re.fullmatch(r"NS\w*UsageDescription", k)}


def load_previous(path: Path | None) -> dict | None:
    """The previous source, or None when there is none or it is not a usable one."""
    if path is None or not path.is_file():
        return None
    try:
        source = json.loads(path.read_text())
        if source["apps"][0]["versions"][0]["downloadURL"]:
            return source
    except (ValueError, KeyError, IndexError, TypeError) as e:
        print(f"[gen-altstore-source] ignoring {path}: {e!r}", file=sys.stderr)
    return None


def older_versions(previous: dict | None, version: str) -> list:
    """Earlier versions to list behind `version`, newest first."""
    if previous is None:
        return []
    kept = [v for v in previous["apps"][0]["versions"] if v.get("version") != version]
    return kept[: MAX_VERSIONS - 1]


def build(ipa: Path, tag: str, repo: str, notes: str, today: str, previous: dict | None = None) -> dict:
    info = ipa_info(ipa)
    version = info.get("CFBundleShortVersionString") or tag.lstrip("v")
    url = f"https://github.com/{repo}/releases/download/{tag}/{ipa.name}"
    return {
        "name": "PS5Upload",
        "identifier": "com.phantomptr.ps5upload.source",
        "subtitle": "Manage a jailbroken PS5 from your iPhone",
        "website": SITE,
        "iconURL": ICON,
        "apps": [
            {
                "name": info.get("CFBundleDisplayName") or "PS5Upload",
                "bundleIdentifier": info["CFBundleIdentifier"],
                "developerName": "phantomptr",
                "subtitle": "Uploads, installs, saves and files for a jailbroken PS5",
                "localizedDescription": (
                    "PS5Upload gets games, packages and homebrew onto a jailbroken PS5 and manages "
                    "it: installs, saves, files, payloads and more. Free and open source."
                ),
                "iconURL": ICON,
                "tintColor": "#111111",
                "category": "utilities",
                "versions": [
                    {
                        "version": version,
                        "buildVersion": str(info.get("CFBundleVersion") or version),
                        "date": today,
                        "localizedDescription": notes or f"PS5Upload {version}",
                        "downloadURL": url,
                        "size": ipa.stat().st_size,
                        "minOSVersion": info.get("MinimumOSVersion") or "15.0",
                    },
                    *older_versions(previous, version),
                ],
                # The binary is unsigned, so it declares no entitlements of its own.
                "appPermissions": {"entitlements": [], "privacy": privacy_of(info)},
            }
        ],
        "news": [],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ipa", required=True, type=Path)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--notes", type=Path)
    ap.add_argument("--previous", type=Path, help="the last promoted release's altstore.json")
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()
    previous = load_previous(args.previous)
    if not args.ipa.is_file():
        if previous is None:
            print(f"[gen-altstore-source] no ipa at {args.ipa} and no previous source; nothing to write", file=sys.stderr)
            return 0
        args.out.write_text(json.dumps(previous, indent=2, ensure_ascii=False) + "\n")
        v = previous["apps"][0]["versions"][0]
        print(f"[gen-altstore-source] no ipa at {args.ipa}; kept the previous source ({v.get('version')})", file=sys.stderr)
        return 0
    notes = args.notes.read_text().strip() if args.notes and args.notes.is_file() else ""
    today = _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%d")
    source = build(args.ipa, args.tag, args.repo, notes, today, previous)
    args.out.write_text(json.dumps(source, indent=2, ensure_ascii=False) + "\n")
    v = source["apps"][0]["versions"][0]
    print(f"[gen-altstore-source] wrote {args.out}: {v['version']} -> {v['downloadURL']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
