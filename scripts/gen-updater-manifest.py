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
      },
      "sha256": {
        "darwin-aarch64": "<64 lowercase hex digits>",
        ...one entry per key in "assets"
      }
    }

`sha256` sits beside `assets` (same keys) rather than inside it, so the
`assets` map keeps its plain key -> URL shape: apps from before 10.2
parse it unchanged and ignore the new field. The app checks the
downloaded file against it before offering to install.

The desktop app's `update_check` command reads this JSON, picks the
`assets` entry matching its own (os, arch), and hands the URL to
`update_download` when the user clicks "Download".

No signatures, no installer formats — just public HTTPS download URLs and
the SHA-256 of each file.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
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
    # Packaged Linux installs. The plain `linux-*` keys above are the
    # PORTABLE zip; someone who installed the .deb or .rpm needs the same
    # format back, not a tarball they cannot `dnf update` and that would
    # unpack a second copy beside the packaged one. `preferred_asset_keys()`
    # in commands/updates.rs asks the package database which package owns
    # the running executable and looks these up first, falling back to the
    # plain key when a release predates them.
    # The Windows installer build. Same idea as the Linux packages below: an
    # installed copy must be offered the installer, not the portable zip,
    # which leaves a second unmanaged copy (a user report).
    ("windows-x86_64-setup",  re.compile(r".*-win-x64-setup\.exe$")),
    ("windows-aarch64-setup", re.compile(r".*-win-arm64-setup\.exe$")),
    ("linux-x86_64-deb",  re.compile(r".*-linux-x64\.deb$")),
    ("linux-aarch64-deb", re.compile(r".*-linux-arm64\.deb$")),
    ("linux-x86_64-rpm",  re.compile(r".*-linux-x64\.rpm$")),
    ("linux-aarch64-rpm", re.compile(r".*-linux-arm64\.rpm$")),
    # One APK per ABI since 7.3.2: arm64 keeps the plain name and the "android" key
    # (older apps on any Android arch look that key up), 32-bit ARM gets its own.
    # Matches current_platform_key() in commands/updates.rs.
    ("android",         re.compile(r".*-android\.apk$")),
    ("android-armv7",   re.compile(r".*-android-armv7\.apk$")),
    # The unsigned iPhone .ipa — current_platform_key() returns "ios" there.
    ("ios",             re.compile(r".*-ios-unsigned\.ipa$")),
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


def sha256_of(path: Path) -> str:
    """Lowercase hex SHA-256 of a file, read in 1 MiB pieces."""
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


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
    sha256: dict[str, str] = {}
    for key, path in assets_paths.items():
        url = release_asset_url(args.repo, args.tag, path.name)
        assets[key] = url
        sha256[key] = sha256_of(path)
        print(f"[gen-updater-manifest] {key} -> {path.name} ({sha256[key]})")

    manifest = {
        "version": args.version,
        "notes": notes_text,
        "pub_date": _dt.datetime.now(_dt.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        ),
        "assets": assets,
        "sha256": sha256,
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
