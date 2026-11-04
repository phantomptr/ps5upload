#!/usr/bin/env python3
"""Check the iOS build produced one real, unsigned iPhone app, then copy it to the given path.

Fails the job (rather than uploading something useless) when the .ipa is missing, holds no app,
is a simulator build, lacks the local-network permission text, carries another version than the
release (TAG_VERSION, when set), cannot open ps5upload:// links, or its binary is not arm64.
"""
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: verify-ios-ipa.py <destination.ipa>")
    found = list(Path("client/src-tauri/gen/apple").rglob("*.ipa"))
    if len(found) != 1:
        raise SystemExit(f"expected one built .ipa under client/src-tauri/gen/apple, found {len(found)}")
    ipa = found[0]
    with zipfile.ZipFile(ipa) as z:
        plists = [n for n in z.namelist()
                  if n.startswith("Payload/") and n.count("/") == 2 and n.endswith(".app/Info.plist")]
        if len(plists) != 1:
            raise SystemExit("the .ipa must hold exactly one Payload/<name>.app")
        info = plistlib.loads(z.read(plists[0]))
        if info.get("CFBundleSupportedPlatforms") != ["iPhoneOS"]:
            raise SystemExit(f"not a device build: {info.get('CFBundleSupportedPlatforms')}")
        if not info.get("NSLocalNetworkUsageDescription"):
            raise SystemExit("NSLocalNetworkUsageDescription is missing (Info.ios.plist not merged?)")
        version = info.get("CFBundleShortVersionString")
        expected = os.environ.get("TAG_VERSION")
        if expected and version != expected:
            raise SystemExit(f"CFBundleShortVersionString is {version!r}, the release is {expected!r}")
        schemes = {s for t in info.get("CFBundleURLTypes", []) for s in t.get("CFBundleURLSchemes", [])}
        if "ps5upload" not in schemes:
            raise SystemExit(f"CFBundleURLTypes has no ps5upload scheme: {sorted(schemes)}")
        name = info.get("CFBundleDisplayName") or info.get("CFBundleName")
        if name != "PS5Upload":
            raise SystemExit(f"home-screen name is {name!r}, not PS5Upload")
        exe = info.get("CFBundleExecutable", "")
        if not exe or "/" in exe:
            raise SystemExit(f"bad CFBundleExecutable {exe!r}")
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "bin"
            binary.write_bytes(z.read(plists[0].rsplit("/", 1)[0] + "/" + exe))
            archs = subprocess.check_output(["lipo", "-archs", str(binary)], text=True).split()
            if "arm64" not in archs:
                raise SystemExit(f"binary is {archs}, not arm64")
        print(f"{ipa.name}: {info.get('CFBundleIdentifier')} {info.get('CFBundleShortVersionString')} arm64")
    dest = Path(sys.argv[1])
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ipa, dest)
    print(f"copied to {dest}")


if __name__ == "__main__":
    main()
