#!/usr/bin/env python3
"""Take a built debug FPKG to a mount verdict on a real console, in one command.

The install path has four ways to report success without anything having happened, so every
step here is followed by the console's own words rather than by an HTTP reply:

    rc: 0 from /api/pkg/dpi-direct-install   the daemon accepted the hand-off
    state = 7, error = 0x0                   the console finished installing
    origin = pkg in the app database         it registered
    the title's process is live              the nested mount worked

Only the last one is the mount, and it is the one that was missing for weeks. There is no
log line that proves one: `[SceSystemStateMgr] Power Mode Change: BIG_APP` is logged before
`[SceLncService] BeginAppMount()` and is printed even when the mount then fails, so it is a
marker to be ignored, not a pass condition. The two failure shapes seen so far are

    [SceLncService] lnc_manager.cpp(568) launchApp: LNC_ISOK::0x80020060   (torn down)
    [0]mountppfs() line=3719 error=45 0x2d                                (ppfs gave up)

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
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

BLOCK = 0x10000
# What the console prints when a mount does not complete. The ppfs family reports its own
# failures as `name() line=NNNN error=<code>`, and only a non-zero code is a failure —
# `verify_ppr_sblock_100() line=1126 error(0)` is a *pass* and must not match.
MOUNT_FAIL = ("verifyImage(", "PfsMountGameData_PPR() ret", "nmount() failed.", "0x80020060",
              "LaunchFlowError.")
MOUNT_FAIL_CODE = re.compile(r"\(\) line=\d+ error=(-?\d+)")
# There is no log line that proves a mount: `[SceSystemStateMgr] Power Mode Change: BIG_APP`
# is logged *before* `[SceLncService] BeginAppMount()`, so a title that mounts onto nothing
# still prints it. Measured 2026-09-14 — an RDR package printed BIG_APP and then failed at
# `mountppfs() line=3719 error=45 0x2d`, and every verdict this script had reported up to
# then was a false OK. The positive signal is the title's own process, which cannot exist
# unless the mount finished.


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
    """The header fields that have been silently wrong in a way the console only reports at
    launch. `0x50 * 0x60` is the inner metadata base in the *logical* mount space, and `0xA0`
    is the *stored* size — the two are in different spaces as soon as anything is compressed,
    so the base can only be checked against `0xA0` for a stored image. Sony's own
    webbrowser.pkg is the counter-example that has to stay accepted here: 0xA0 = 0x50000 while
    its meta base is 0x400000 and its logical mount is 0x4a0000. Report, do not refuse."""
    with pkg.open("rb") as f:
        head = f.read(0x1000)
    if head[:4] != b"\x7fFIH":
        return {"fatal": f"{pkg.name} is not a debug package (\\x7FFIH); magic={head[:4].hex()}"}
    u32 = lambda o: int.from_bytes(head[o : o + 4], "little")
    u64 = lambda o: int.from_bytes(head[o : o + 8], "little")
    meta_base, stored = u32(0x50) * u64(0x60), u64(0xA0)
    blocks, stored_blocks = u32(0x90), u64(0x60)
    return {
        "meta_base": meta_base,
        "stored_size": stored,
        "stored_blocks": blocks,
        "self_consistent": blocks * stored_blocks == stored,
        "base_within_stored_size": 0 < meta_base <= stored,
    }


def mount_failure(line: str) -> bool:
    if any(token in line for token in MOUNT_FAIL):
        return True
    code = MOUNT_FAIL_CODE.search(line)
    return bool(code) and int(code.group(1)) != 0


def registered_title(engine: str, addr: str, title_id: str) -> bool:
    try:
        url = f"{engine}/api/ps5/apps/installed?addr={addr}"
        with urllib.request.urlopen(url, timeout=30) as r:
            titles = json.loads(r.read()).get("titles", [])
    except (urllib.error.URLError, json.JSONDecodeError, TimeoutError):
        return False
    return any(t.get("title_id") == title_id and t.get("origin") == "pkg" for t in titles)


def running_title(engine: str, addr: str, title_id: str) -> bool:
    """True when the console has a live process for this title."""
    try:
        url = f"{engine}/api/ps5/process/list?addr={addr}"
        with urllib.request.urlopen(url, timeout=30) as r:
            procs = json.loads(r.read()).get("processes", [])
    except (urllib.error.URLError, json.JSONDecodeError, TimeoutError):
        return False
    return any(p.get("title_id") == title_id for p in procs)


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
    say(f"inner     meta base {check['meta_base']:#x}; stored image {check['stored_size']:#x} "
        f"in {check['stored_blocks']} blocks"
        + ("" if check["self_consistent"] else "   <-- 0x90 and 0xA0 disagree"))
    if not check["base_within_stored_size"]:
        say("          note: the base is outside the *stored* size, which is only wrong for a "
            "stored image — a compressed one carries its logical size in the descriptor")

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

    # The app database lags the install verdict, and a launch of a title it has not caught up
    # with gives no mount answer at all — the payload just sits on the frame. Wait for it.
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline and not registered_title(args.engine, args.addr, title_id):
        time.sleep(3)
    say(f"launch    {title_id} registered={registered_title(args.engine, args.addr, title_id)}")

    seen = set(syslog(args.engine, host).splitlines())
    launched = post_json(args.engine, "/api/ps5/app/launch",
                         {"addr": args.addr, "title_id": title_id})
    if not launched.get("ok"):
        print(f"launch call failed: {launched}", file=sys.stderr)
        return 2
    # Compare line *sets*, one poll apart, rather than diffing two snapshots. The log is a
    # ~1,550-line ring that the console's chatter refills in seconds, so a single check at the
    # end of a long window misses a failure line that has already scrolled away — that is what
    # produced false "mount OK" verdicts. A short poll interval keeps the window small, and a
    # set difference survives rotation (dropped lines cannot hide an added one).
    failure = mounted = None
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline and not (failure or mounted):
        time.sleep(2)
        text = syslog(args.engine, host)
        fresh = [l.strip() for l in set(text.splitlines()) - seen]
        seen = set(text.splitlines())
        for line in fresh:
            if mount_failure(line):
                failure = line
                break
        # Checked after the log so a failure printed in the same window wins: the two can
        # only be a poll apart, and a false OK is the costlier mistake.
        if not failure and running_title(args.engine, args.addr, title_id):
            mounted = f"a live {title_id} process"
    if failure:
        say(f"mount     FAILED  {failure}")
        return 1
    if not mounted:
        say(f"mount     NO VERDICT — {title_id} never appeared in the process list and the log "
            "carried no failure line. Re-run, or read the log yourself before believing this.")
        return 2
    say(f"mount     OK  {mounted}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
