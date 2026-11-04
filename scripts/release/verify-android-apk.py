#!/usr/bin/env python3
"""Check a built APK still has what the app needs from its manifest.

`gen/android` is regenerated on every build and patched by android-postinit-patch.sh, and a
missing line fails silently on a phone: no VIBRATE means haptics do nothing with no error, and a
template change once broke every Android connection. Nothing else looks at the shipped manifest.

usage: verify-android-apk.py <app.apk> [...]    (aapt2 from $ANDROID_HOME/build-tools)
       verify-android-apk.py --dump <file>      (a saved `aapt2 dump xmltree --file AndroidManifest.xml`)
       verify-android-apk.py --self-test        (make test-root)
"""
import os
import re
import subprocess
import sys
import zipfile
from pathlib import Path

PERMISSIONS = {
    "android.permission.INTERNET",
    "android.permission.ACCESS_LOCAL_NETWORK",
    "android.permission.ACCESS_NETWORK_STATE",
    "android.permission.ACCESS_WIFI_STATE",
    "android.permission.CHANGE_WIFI_MULTICAST_STATE",
    "android.permission.MANAGE_EXTERNAL_STORAGE",
    "android.permission.READ_EXTERNAL_STORAGE",
    "android.permission.WRITE_EXTERNAL_STORAGE",
    "android.permission.FOREGROUND_SERVICE",
    "android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE",
    "android.permission.FOREGROUND_SERVICE_DATA_SYNC",
    "android.permission.WAKE_LOCK",
    "android.permission.VIBRATE",
    "android.permission.RECORD_AUDIO",
    "android.permission.MODIFY_AUDIO_SETTINGS",
    "android.permission.CAMERA",
    "android.permission.REQUEST_INSTALL_PACKAGES",
}
# Declared in the manifest by name, each next to an attribute line it needs.
NAMES = {
    ".TransferService": "the foreground service for background transfers",
    ".UpdateInstallReceiver": "the in-app update's result receiver",
    ".MainActivity": "the app's activity",
}


def problems(tree: str) -> list[str]:
    """What is wrong with the output of `aapt2 dump xmltree`."""
    out = []
    declared = set(re.findall(r'android:name\([^)]*\)="(android\.permission\.[A-Z_0-9]+)"', tree))
    out += [f"missing permission {p}" for p in sorted(PERMISSIONS - declared)]
    for name, why in NAMES.items():
        # aapt2 prints the full class name; the source manifest's ".X" is resolved by then.
        if not re.search(r'android:name\([^)]*\)="[^"]*' + re.escape(name) + '"', tree):
            out.append(f"missing {name} ({why})")
    if not re.search(r'android:scheme\([^)]*\)="ps5upload"', tree):
        out.append("no ps5upload:// deep-link filter")
    if "android:networkSecurityConfig(" not in tree:
        out.append("no networkSecurityConfig: a release build could not reach its own engine")
    if "android:supportsPictureInPicture(" not in tree:
        out.append("no supportsPictureInPicture: Remote Play could not float")
    if "android:windowSoftInputMode(" not in tree:
        out.append("no windowSoftInputMode: older Androids would not report the keyboard's inset")
    return out


def dump(apk: Path) -> str:
    tools = Path(os.environ.get("ANDROID_HOME", "")) / "build-tools"
    found = sorted(tools.glob("*/aapt2"), key=lambda p: [int(x) if x.isdigit() else 0 for x in re.split(r"[.-]", p.parent.name)])
    if not found:
        raise SystemExit(f"no aapt2 under {tools}")
    return subprocess.check_output(
        [str(found[-1]), "dump", "xmltree", "--file", "AndroidManifest.xml", str(apk)], text=True
    )


def has_renderer_crash_handler(apk: Path) -> bool:
    """wry's generated client overrides this only through client/.cargo/config.toml."""
    with zipfile.ZipFile(apk) as z:
        return any(
            b"onRenderProcessGone" in z.read(n)
            for n in z.namelist()
            if re.fullmatch(r"classes\d*\.dex", n)
        )


def self_test() -> None:
    """The checker itself, on a small manifest in aapt2's format."""
    a = "http://schemas.android.com/apk/res/android:"
    lines = [f'{a}name(0x01010003)="{p}" (Raw: "{p}")' for p in sorted(PERMISSIONS)]
    lines += [f'{a}name(0x01010003)="com.phantomptr.ps5upload{n}"' for n in NAMES]
    lines += [
        f'{a}scheme(0x01010027)="ps5upload" (Raw: "ps5upload")',
        f"{a}networkSecurityConfig(0x01010527)=@0x7f120008",
        f"{a}supportsPictureInPicture(0x010104f7)=true",
        f"{a}windowSoftInputMode(0x0101022b)=0x10",
    ]
    good = "\n".join(lines)
    assert problems(good) == [], problems(good)
    # What each of these lines is there for: taking it away must be noticed.
    for needle, expected in (
        ("android.permission.VIBRATE", "missing permission android.permission.VIBRATE"),
        (".TransferService", "missing .TransferService"),
        ('="ps5upload"', "no ps5upload:// deep-link filter"),
        ("networkSecurityConfig", "no networkSecurityConfig"),
        ("supportsPictureInPicture", "no supportsPictureInPicture"),
    ):
        tree = "\n".join(l for l in lines if needle not in l)
        assert any(p.startswith(expected) for p in problems(tree)), (needle, problems(tree))
    print("verify-android-apk self-test ok")


def main() -> None:
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)
    if args[0] == "--self-test":
        self_test()
        return
    if args[0] == "--dump":
        bad = problems(Path(args[1]).read_text())
        if bad:
            raise SystemExit("\n".join(bad))
        print("manifest ok")
        return
    failed = False
    for name in args:
        apk = Path(name)
        bad = problems(dump(apk))
        if not has_renderer_crash_handler(apk):
            bad.append("no onRenderProcessGone: a renderer crash would take the app down")
        for b in bad:
            print(f"::error::{apk.name}: {b}")
        failed = failed or bool(bad)
        if not bad:
            print(f"{apk.name}: manifest ok")
    if failed:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
