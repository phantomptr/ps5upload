#!/usr/bin/env python3
"""Take a built debug FPKG to a mount verdict on a real console, in one command.

The install path has four ways to report success without anything having happened, so every
step here is followed by the console's own words rather than by an HTTP reply:

    rc: 0 from /api/pkg/dpi-direct-install   the daemon accepted the hand-off
    state = 7, error = 0x0                   the console finished installing
    origin = pkg in the app database         it registered
    no launch error                          the nested mount worked

Only the last one is the mount, and it is the one that was missing for weeks. A title whose
mount fails is torn down with `0x80020060`, which surfaces at launch as

    [SceLncService] lnc_manager.cpp(568) launchApp: LNC_ISOK::0x80020060

**Launching such a title coredumps SceShellUI** (it restarts itself; the console and the
payload survive). That is why --launch is opt-in: run the install steps as often as you like,
and add the flag only when you want the mount answer.

usage: fpkg-mount-test.py PKG [--content-id ID] [--addr HOST:9114] [--engine URL]
                             [--launch] [--timeout SECS]

Exit status: 0 mounted (or installed, without --launch), 1 refused, 2 timed out / error.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

BLOCK = 0x10000
MOUNT_FAIL = ("0x80020060", "verifyImage", "nmount() failed", "LNC_ISOK::0x80")


def post_json(engine: str, path: str, body: dict) -> dict:
    req = urllib.request.Request(
        engine + path, data=json.dumps(body).encode(), headers={"content-type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            return json.loads(r.read())
    except (urllib.error.URLError, json.JSONDecodeError) as e:
        return {"error": str(e)}


def upload(engine: str, pkg: Path) -> dict:
    """curl does the multipart body; this is the only non-urllib call in the flow."""
    out = subprocess.run(
        ["curl", "-s", "-X", "POST", f"{engine}/api/pkg/upload", "-F", f"f=@{pkg}"],
        capture_output=True, text=True, check=False,
    ).stdout
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return {"error": out.strip() or "upload failed"}


def preflight(pkg: Path) -> dict:
    """The two header fields that are silently wrong in a way the console only reports at
    launch: the inner metadata base must land inside the inner image."""
    with pkg.open("rb") as f:
        head = f.read(0x1000)
    if head[:4] != b"\x7fFIH":
        return {"fatal": f"{pkg.name} is not a debug package (\\x7FFIH); magic={head[:4].hex()}"}
    u32 = lambda o: int.from_bytes(head[o : o + 4], "little")
    u64 = lambda o: int.from_bytes(head[o : o + 8], "little")
    meta_base, inner = u32(0x50) * u64(0x60), u64(0xA0)
    return {
        "meta_base": meta_base,
        "inner_size": inner,
        "stored_blocks": u32(0x90),
        "logical_blocks": inner // BLOCK if BLOCK else 0,
        "meta_base_in_range": 0 < meta_base <= inner,
    }


def syslog(engine: str, host: str, lines: int = 8000) -> str:
    try:
        url = f"{engine}/api/ps5/syslog/tail?addr={host}&lines={lines}"
        with urllib.request.urlopen(url, timeout=60) as r:
            return json.loads(r.read())["text"]
    except (urllib.error.URLError, KeyError, json.JSONDecodeError, TimeoutError):
        return ""


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("pkg", type=Path)
    ap.add_argument("--content-id", help="defaults to the package's file name stem")
    ap.add_argument("--addr", default="192.168.86.100:9114", help="mgmt addr:port the payload serves")
    ap.add_argument("--engine", default="http://127.0.0.1:19113")
    ap.add_argument("--launch", action="store_true", help="launch it and read the mount verdict")
    ap.add_argument("--timeout", type=float, default=900.0)
    args = ap.parse_args()

    if not args.pkg.is_file():
        print(f"no such package: {args.pkg}", file=sys.stderr)
        return 2
    host = args.addr.split(":")[0]
    content_id = args.content_id or args.pkg.stem
    title_id = content_id.split("-")[1].split("_")[0] if "-" in content_id else content_id
    say = lambda *a: print(*a, flush=True)

    check = preflight(args.pkg)
    if check.get("fatal"):
        print(check["fatal"], file=sys.stderr)
        return 2
    say(f"package   {args.pkg.name}")
    say(f"content   {content_id}   (title {title_id})")
    say(
        f"inner     meta base {check['meta_base']:#x} of {check['inner_size']:#x} bytes"
        f"{'' if check['meta_base_in_range'] else '   <-- OUT OF RANGE'}"
    )
    say(f"blocks    {check['stored_blocks']} stored / {check['logical_blocks']} logical"
        + ("" if check['stored_blocks'] < check['logical_blocks'] else "   (stored: uncompressed)"))
    if not check["meta_base_in_range"]:
        print("refusing: the console reads the inner superblock at a base outside the image",
              file=sys.stderr)
        return 2

    up = upload(args.engine, args.pkg)
    if not up.get("path"):
        print(f"upload failed: {up}", file=sys.stderr)
        return 2
    say(f"uploaded  {up['path'].split('/')[-1]}")

    started = post_json(args.engine, "/api/pkg/install/start", {
        "ps5_addr": args.addr, "path": up["path"], "content_id": content_id,
        "serve_only": True, "delete_staging": False,
    })
    session = started.get("session_id")
    if not session:
        print(f"install/start refused: {started}", file=sys.stderr)
        return 2
    say(f"serving   session {session}")

    ensure = post_json(args.engine, "/api/pkg/dpi-ensure", {"ps5_addr": args.addr})
    say(f"dpi       listening={ensure.get('listening')} sent={ensure.get('sent')}")
    install = post_json(args.engine, "/api/pkg/dpi-direct-install",
                        {"ps5_addr": args.addr, "session_id": session})
    say(f"dpi-install rc={install.get('rc')} (accepted only — the verdict follows)")
    post_json(args.engine, "/api/pkg/payload-restore", {"ps5_addr": args.addr})

    verdict = subprocess.run(
        [sys.executable, str(Path(__file__).with_name("ps5-install-watch.py")), content_id,
         "--addr", host, "--engine", args.engine, "--timeout", str(args.timeout), "--json"],
        capture_output=True, text=True, check=False,
    )
    try:
        watched = json.loads(verdict.stdout.strip().splitlines()[-1])
    except (json.JSONDecodeError, IndexError):
        print(f"watcher failed: {verdict.stdout.strip()} {verdict.stderr.strip()}", file=sys.stderr)
        return 2
    say(f"install   {watched.get('verdict')} registered={watched.get('registered')}")
    if not watched.get("verdict") or watched["verdict"][0] != "installed":
        return 1

    if not args.launch:
        say("mounted   not tested (--launch does that, and a failed mount coredumps SceShellUI)")
        return 0

    before = syslog(args.engine, host)
    launched = post_json(args.engine, "/api/ps5/app/launch",
                         {"addr": args.addr, "title_id": title_id})
    if not launched.get("ok"):
        print(f"launch call failed: {launched}", file=sys.stderr)
        return 2
    deadline = time.monotonic() + 30
    failure = None
    while time.monotonic() < deadline:
        time.sleep(3)
        text = syslog(args.engine, host)
        fresh = "\n".join(l for l in text.splitlines() if l not in before)
        for token in MOUNT_FAIL:
            if token in fresh:
                failure = next(l.strip() for l in fresh.splitlines() if token in l)
                break
        if failure:
            break
    if failure:
        say(f"mount     FAILED  {failure}")
        return 1
    say("mount     OK (no 0x80020060 and no verifyImage; the app's own failure, if any, is later)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
