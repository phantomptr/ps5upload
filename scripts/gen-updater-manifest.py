#!/usr/bin/env python3
"""Generate the `latest.json` manifest the in-app updater reads.

The release workflow calls this once per release, after all per-platform
installers have been collected under `release/` and renamed to the
canonical `PS5Upload-<ver>-<os>-<arch>.<ext>` form. We scan for those
filenames, map each to a platform key, and emit:

    {
      "version": "2.2.0",
      "notes": "<release notes>",
      "pub_date": "2026-04-22T00:00:00Z",
      "assets": {
        "darwin-aarch64": "https://.../PS5Upload-2.2.0-mac-arm64.dmg",
        "darwin-x86_64":  "https://.../PS5Upload-2.2.0-mac-x64.dmg",
        "linux-x86_64":   "https://.../PS5Upload-2.2.0-linux-x64.zip",
        "linux-aarch64":  "https://.../PS5Upload-2.2.0-linux-arm64.zip",
        "windows-x86_64": "https://.../PS5Upload-2.2.0-win-x64.zip",
        "windows-aarch64":"https://.../PS5Upload-2.2.0-win-arm64.zip"
      }
    }

The desktop app's `update_check` command reads this JSON, picks the
`assets` entry matching its own (os, arch), and hands the URL to
`update_download` when the user clicks "Download".

No signatures, no installer formats — just public HTTPS download URLs.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import re
import sys
from pathlib import Path


# Map a release filename → the platform key the app expects. One entry
# per (os, arch). The pattern matches filenames produced by the
# workflow's `Normalise release assets` step.
PLATFORM_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("darwin-aarch64",  re.compile(r".*-mac-arm64\.dmg$")),
    ("darwin-x86_64",   re.compile(r".*-mac-x64\.dmg$")),
    ("windows-x86_64",  re.compile(r".*-win-x64\.zip$")),
    ("windows-aarch64", re.compile(r".*-win-arm64\.zip$")),
    ("linux-x86_64",    re.compile(r".*-linux-x64\.zip$")),
    ("linux-aarch64",   re.compile(r".*-linux-arm64\.zip$")),
]


def find_assets(release_dir: Path) -> dict[str, Path]:
    """Return {platform_key: asset_path} for every matching file.

    First match wins per platform — release files have unique
    filenames, so this is mostly defensive.
    """
    matches: dict[str, Path] = {}
    for path in sorted(release_dir.iterdir()):
        if not path.is_file():
            continue
        name = path.name
        for key, pat in PLATFORM_PATTERNS:
            if key in matches:
                continue
            if pat.match(name):
                matches[key] = path
                break
    return matches


def release_asset_url(repo: str, tag: str, filename: str) -> str:
    """Stable GitHub release asset URL pinned to the tag.

    Uses `releases/download/<tag>/<file>` (not `releases/latest/...`)
    so an old build carrying a stale manifest still gets the
    version-appropriate download URL.
    """
    return f"https://github.com/{repo}/releases/download/{tag}/{filename}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--release-dir", required=True, type=Path)
    ap.add_argument("--version", required=True)
    ap.add_argument("--tag", required=True, help="e.g. v2.2.0")
    ap.add_argument("--repo", required=True, help="owner/name on GitHub")
    ap.add_argument("--notes", type=Path, help="path to RELEASE_NOTES.md")
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()

    release_dir: Path = args.release_dir
    if not release_dir.is_dir():
        print(f"release dir not found: {release_dir}", file=sys.stderr)
        return 2

    assets_paths = find_assets(release_dir)
    if not assets_paths:
        # Not a fatal error — a release might legitimately omit all
        # platform bundles (e.g., a docs-only release). Emit a
        # manifest with empty assets so the in-app updater reports
        # "no download for your platform" cleanly instead of erroring.
        print(
            "[gen-updater-manifest] no matching assets found; "
            "emitting empty-assets manifest.",
            file=sys.stderr,
        )

    notes_text = ""
    if args.notes and args.notes.is_file():
        notes_text = args.notes.read_text().strip()

    assets: dict[str, str] = {}
    for key, path in assets_paths.items():
        url = release_asset_url(args.repo, args.tag, path.name)
        assets[key] = url
        print(f"[gen-updater-manifest] {key} -> {path.name}")

    manifest = {
        "version": args.version,
        "notes": notes_text,
        "pub_date": _dt.datetime.now(_dt.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        ),
        "assets": assets,
    }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(manifest, indent=2) + "\n")
    print(
        f"[gen-updater-manifest] wrote {args.out} with "
        f"{len(assets)} platform(s)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
