#!/usr/bin/env python3
"""Wait for a PS5 package install to reach a terminal state, and say which it was.

`POST /api/pkg/install/start` answering `rc: 0` means only that the console *accepted* the
request. The streaming path returns even earlier: its DPI daemon acknowledges the hand-off
before the transfer has fetched a byte. Neither is an install, so this watches the console
instead, and reports what the console says.

The console's own sequence, newest last:

    [PlayGoCore][Request #N] application data size (X)
    [PlayGoCore][Request #N] transfer started (Y/X)
    [PlayGoCore][Request #N] prepromote ready (<title id>, app.pkg)     <- transfer done
    [PlayGoCore][Request #N] transfer ended (0x00000000)
    [PlayGoCore][Request #N] request ended (state = 7, error = 0x0)     <- install done
    [BGFT] [516] Task N : STATE_COMPLETE, RUN_STATE_COMPLETE

A failure replaces the last three with `transfer ended (0x…)` / `request ended (state = 8,
error = 0x…)` / `Task N : … : ended (state=0,runstate=2,error=0x…)`. That error code is the
console's verdict — `0x80b21185` (`CE-118872-7`) is BGFT refusing the transfer, and a launch
failure surfaces later as the game's own `CE-…` on screen, not here.

The registration in the app database lags all of it, so it is reported as confirmation
rather than as the verdict.

usage: ps5-install-watch.py [CONTENT_ID] [--addr HOST] [--engine URL] [--timeout SECS] [--json]

Exit status: 0 installed, 1 the console refused it, 2 timed out.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request

DEFAULT_ENGINE = "http://127.0.0.1:19113"
DEFAULT_ADDR = "192.168.86.100"

# The lines that carry a verdict, and what each one means.
REQUEST_OK = re.compile(r"request ended \(state = 7, error = 0x0")
REQUEST_FAIL = re.compile(r"request ended \(state = (\d+), error = 0x([0-9a-f]+)")
TRANSFER_FAIL = re.compile(r"transfer ended \(0x([0-9a-f]+)\)")
TASK_DONE = re.compile(r"Task \w+ : .*ended \(state=(\d+),runstate=(\d+),error=0x([0-9a-f]+)\)")
OPEN = re.compile(r"\[RequestInstall\] begin \(#(\d+), (\S+)\)")


def get(url: str, timeout: float = 30.0) -> str:
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return r.read().decode("utf-8", "replace")


def syslog(engine: str, addr: str, lines: int = 4000) -> str:
    try:
        return json.loads(get(f"{engine}/api/ps5/syslog/tail?addr={addr}&lines={lines}"))["text"]
    except (urllib.error.URLError, KeyError, json.JSONDecodeError, TimeoutError):
        return ""


def titles(engine: str, addr: str) -> list[dict]:
    try:
        return json.loads(get(f"{engine}/api/ps5/apps/installed?addr={addr}"))["titles"]
    except (urllib.error.URLError, KeyError, json.JSONDecodeError, TimeoutError):
        return []


def verdict_for(text: str, content_id: str) -> tuple[str, str] | None:
    """The last terminal signal about `content_id` in this log text, if any.

    The verdict lines name a *request number*, not the content id — only the lines that open
    an attempt carry the id — so the attempt is found first and its own lines are read after.
    Every line is re-scanned on each poll: the console's log rotates, and a marker kept by
    position can fall off the end between two of them.
    """
    title_id = content_id.split("-")[1].split("_")[0] if "-" in content_id else ""
    lines = text.splitlines()

    # The newest attempt for this content id, and the request number it was given.
    request = None
    for line in lines:
        if m := OPEN.search(line):
            if m.group(2) == content_id:
                request = m.group(1)
    if request is None:
        # No attempt in this window. A task line can still name the content id directly.
        for line in lines:
            if content_id in line and (m := TASK_DONE.search(line)):
                state, run, err = m.group(1), m.group(2), m.group(3)
                if state == "3" or (err == "0" and run == "4"):
                    return ("installed", "the console's BGFT task completed")
                return ("refused", f"task ended state={state} runstate={run} error=0x{err}")
        return None

    mine = f"[Request #{request}]"
    latest = None
    for line in lines:
        if mine not in line:
            continue
        if REQUEST_OK.search(line):
            latest = ("installed", "the console finished the request cleanly")
        elif m := REQUEST_FAIL.search(line):
            latest = ("refused", f"request ended state={m.group(1)} error=0x{m.group(2)}")
        elif m := TRANSFER_FAIL.search(line):
            if m.group(1) != "00000000":
                latest = ("refused", f"transfer ended 0x{m.group(1)}")
    for line in lines:
        if content_id not in line and title_id not in line:
            continue
        if m := TASK_DONE.search(line):
            state, run, err = m.group(1), m.group(2), m.group(3)
            if state == "3" or (err == "0" and run == "4"):
                latest = ("installed", "the console's BGFT task completed")
            else:
                latest = ("refused", f"task ended state={state} runstate={run} error=0x{err}")
    return latest


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("content_id", nargs="?", help="e.g. UP4433-PPSA17221_00-MINECRAFTPS50000")
    ap.add_argument("--addr", default=DEFAULT_ADDR)
    ap.add_argument("--engine", default=DEFAULT_ENGINE)
    ap.add_argument("--timeout", type=float, default=1800.0, help="seconds (default 1800)")
    ap.add_argument("--interval", type=float, default=5.0)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    if not args.content_id:
        print("content id required: the watcher must know which install to follow", file=sys.stderr)
        return 2

    title_id = args.content_id.split("-")[1].split("_")[0]
    deadline = time.monotonic() + args.timeout
    seen = None
    while time.monotonic() < deadline:
        found = verdict_for(syslog(args.engine, args.addr), args.content_id)
        if found and found != seen:
            seen = found
            if not args.json:
                print(f"{found[0]}: {found[1]}", flush=True)
            if found[0] == "installed":
                break
            if found[0] == "refused":
                return refuse(args, found[1])
        time.sleep(args.interval)

    # The app database is the confirmation a terminal log line does not give: it lags, so it
    # can still be catching up, and its absence is not proof the install failed.
    registered = any(t.get("title_id") == title_id and t.get("origin") == "pkg"
                     for t in titles(args.engine, args.addr))
    if args.json:
        print(json.dumps({"content_id": args.content_id, "verdict": seen, "registered": registered}))
    elif seen is None:
        print(f"timeout: no terminal line about {args.content_id} within {args.timeout:.0f}s"
              + (f"; the app database {'has' if registered else 'does not have'} {title_id}"),
              file=sys.stderr)
        return 2
    else:
        print(f"{'registered as' if registered else 'not yet in the app database:'} {title_id}",
              flush=True)
    return 0 if seen and seen[0] == "installed" else 2


def refuse(args, detail: str) -> int:
    registered = any(t.get("title_id") == args.content_id.split("-")[1].split("_")[0]
                     and t.get("origin") == "pkg"
                     for t in titles(args.engine, args.addr))
    if args.json:
        print(json.dumps({"content_id": args.content_id, "verdict": ["refused", detail],
                          "registered": registered}))
    return 1


if __name__ == "__main__":
    sys.exit(main())
