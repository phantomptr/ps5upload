#!/usr/bin/env python3
"""Drive a stream install and record every state it passes through, with timings.

This is the measurement tool behind the install-state work. The install path can
report "done" three different ways (an HTTP reply, a Sony log line, the app
database) and they disagree in practice, so the only honest way to fix the state
machine is to watch all of them at once and see which one is right, and when.

What it prints, once per poll:

    t=<seconds since start>
    phase=<engine's phase>          what the client would render
    disk=<installed_bytes>/<total>  bytes observed landing on the console
    wire=<served_bytes>/<total>     bytes the console pulled from us (Range responses)
    reqs=<n>                        pkg-host requests answered
    sony=<log verdict>              PlayGo/BGFT's own last word, if any
    act=<active transaction count>  when the engine reports it

usage: ps5-install-trace.py PKG [--addr HOST:9114] [--engine URL]
                              [--interval SECS] [--timeout SECS] [--no-dpi]

Every run ends with `payload-restore`, because a Stream install leaves the
console running the DPI daemon in place of the main payload: without the
restore the management API is gone and the NEXT run's preflight and inventory
calls fail with `unknown` against a console that is perfectly healthy.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

VERDICT_MARKERS = ("request ended", "transfer ended", "progress.error_code", "ended (state=")


def post_json(engine: str, path: str, body: dict, timeout: float = 60.0) -> dict:
    req = urllib.request.Request(
        engine + path, data=json.dumps(body).encode(), headers={"content-type": "application/json"}
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read())
    except (urllib.error.URLError, json.JSONDecodeError, TimeoutError) as e:
        return {"error": str(e)}


def get_json(engine: str, path: str, timeout: float = 30.0) -> dict:
    try:
        with urllib.request.urlopen(engine + path, timeout=timeout) as r:
            return json.loads(r.read())
    except (urllib.error.URLError, json.JSONDecodeError, TimeoutError) as e:
        return {"error": str(e)}


def upload(engine: str, pkg: Path) -> dict:
    out = subprocess.run(
        ["curl", "-s", "-X", "POST", f"{engine}/api/pkg/upload", "-F", f"f=@{pkg}"],
        capture_output=True, text=True, check=False,
    ).stdout
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return {"error": out.strip() or "upload failed"}


def sony_lines(engine: str, host: str, title_id: str, lines: int = 12000) -> list[str]:
    """Console log lines that speak about THIS title's install, newest last."""
    d = get_json(engine, f"/api/ps5/syslog/tail?addr={host}&lines={lines}", timeout=60)
    text = d.get("text", "") if isinstance(d, dict) else ""
    out = []
    for line in text.splitlines():
        if title_id and title_id in line and any(m in line for m in VERDICT_MARKERS):
            out.append(line.strip())
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("pkg", type=Path)
    ap.add_argument("--addr", default="192.168.86.100:9114")
    ap.add_argument("--engine", default="http://127.0.0.1:19113")
    ap.add_argument("--interval", type=float, default=2.0)
    ap.add_argument("--timeout", type=float, default=1800.0, help="overall trace deadline")
    ap.add_argument("--no-dpi", action="store_true", help="stage the session only; do not install")
    args = ap.parse_args()

    host = args.addr.split(":")[0]
    say = lambda *a: print(*a, flush=True)

    meta = post_json(args.engine, "/api/pkg/parse", {"path": str(args.pkg)}, timeout=120)
    if meta.get("error"):
        say(f"parse failed: {meta['error']}")
        return 2
    content_id = meta["content_id"]
    title_id = meta.get("title_id") or ""
    total = int(meta.get("size") or 0)
    say(f"package   {args.pkg.name}")
    say(f"content   {content_id}  ({meta.get('package_type')}, {total} bytes)")

    # What is on the console RIGHT NOW — the "already installed" question, asked
    # before anything is started so a later change is unambiguous.
    before = get_json(args.engine, f"/api/pkg/installed?addr={host}%3A9114&title_id={title_id}")
    say(f"installed before: {json.dumps(before.get('artifacts', []), indent=None)}")
    pre = get_json(
        args.engine,
        f"/api/pkg/install/preflight?addr={host}%3A9114&content_id={content_id}"
        f"&package_type={meta.get('package_type', '')}&expected_size={total}"
        f"&package_fingerprint={meta.get('fingerprint', '')}",
    )
    say(f"preflight {pre.get('state')}  ver={pre.get('installed_version')!r}  {pre.get('detail', '')}")

    up = upload(args.engine, args.pkg)
    if not up.get("path"):
        say(f"upload failed: {up}")
        return 2

    started = post_json(args.engine, "/api/pkg/install/start", {
        "ps5_addr": args.addr, "path": up["path"], "content_id": content_id,
        "serve_only": True, "delete_staging": False,
    })
    session = started.get("session_id")
    if not session:
        say(f"install/start refused: {json.dumps(started)}")
        return 2
    say(f"session   {session}")

    t0 = time.monotonic()
    dpi_reply: dict = {}
    dpi_elapsed: list[float] = []

    if not args.no_dpi:
        ens = post_json(args.engine, "/api/pkg/dpi-ensure", {"ps5_addr": args.addr}, timeout=180)
        say(f"dpi-ensure t={time.monotonic()-t0:.1f}s ok={ens.get('ok')} "
            f"listening={ens.get('listening')} sent={ens.get('sent')} reason={ens.get('reason')}")

        def run_dpi() -> None:
            began = time.monotonic()
            # Deliberately long: we want the TRUE duration of the hand-off, not
            # our own timeout. The engine blocks in this call for as long as the
            # console takes to accept (which is minutes when Sony has to clear an
            # existing install first).
            dpi_reply.update(post_json(
                args.engine, "/api/pkg/dpi-direct-install",
                {"ps5_addr": args.addr, "session_id": session}, timeout=args.timeout + 120,
            ))
            dpi_elapsed.append(time.monotonic() - began)

        threading.Thread(target=run_dpi, daemon=True).start()

    seen_sony: set[str] = set()
    last_wire, last_disk, last_t = 0, 0, time.monotonic()
    verdict = None
    try:
        while time.monotonic() - t0 < args.timeout:
            time.sleep(args.interval)
            t = time.monotonic() - t0
            st = get_json(args.engine, f"/api/pkg/install/status?session={session}")
            if st.get("error"):
                say(f"t={t:6.1f}s  status error: {st['error']}")
                continue
            # `transfer_bytes` is the covered-bytes figure the client renders.
            # Deliberately NOT `bytes_served` (the raw sum): Sony re-fetches
            # ranges, so the sum passes the package size mid-transfer.
            sess = get_json(args.engine, f"/api/pkg/install/sessions")
            wire = int(st.get("transfer_bytes") or 0)
            reqs = int(st.get("served_requests") or 0)
            served_sum = 0
            if isinstance(sess, list):
                for row in sess:
                    if row.get("id") == session:
                        served_sum = int(row.get("bytes_served") or 0)
                        reqs = reqs or int(row.get("requests_served") or 0)
            disk = int(st.get("installed_bytes") or 0)
            fresh = [l for l in sony_lines(args.engine, host, title_id) if l not in seen_sony]
            seen_sony.update(fresh)

            now = time.monotonic()
            rate = (wire - last_wire) / max(now - last_t, 1e-6)
            last_wire, last_t = wire, now
            pct = f"{100.0 * min(wire, total) / total:5.1f}%" if total else "  n/a"
            # `got` is the covered-bytes progress the client renders; `sum` is the
            # raw served total, printed so the overshoot stays visible (it is how
            # the wrong metric was caught in the first place).
            say(f"t={t:6.1f}s phase={str(st.get('phase')):8s} got={wire:>12}/{total} {pct} "
                f"sum={served_sum:>12} disk={disk:>12} reqs={reqs:<5} rate={rate/1e6:7.1f}MB/s "
                f"err=0x{int(st.get('err_code') or 0):08x}"
                + (f" stalled={st.get('stalled')}" if st.get("stalled") else "")
                + (f" unverified={st.get('accepted_unverified')}" if st.get("accepted_unverified") else ""))
            for line in fresh:
                say(f"         sony | {line}")
            if dpi_elapsed and not dpi_reply.get("_logged"):
                dpi_reply["_logged"] = True
                say(f"         dpi-direct-install returned after {dpi_elapsed[0]:.1f}s: "
                    f"{json.dumps({k: v for k, v in dpi_reply.items() if k != '_logged'})}")
            if st.get("phase") in ("error", "Error") or st.get("cancelled"):
                verdict = st.get("phase")
                break
            if st.get("phase") in ("done", "Done") and not st.get("accepted_unverified"):
                verdict = "done"
                break
            if disk == last_disk and st.get("stalled"):
                verdict = "stalled"
                break
            last_disk = disk
    except KeyboardInterrupt:
        say("interrupted")

    # Put the main payload back, exactly as the client does in its `finally`.
    # Without it the console keeps the DPI daemon loaded, the management API is
    # gone, and the NEXT run's preflight/inventory calls fail against a console
    # that is perfectly healthy — which is a trap for anyone using this script
    # to compare runs.
    post_json(args.engine, "/api/pkg/payload-restore", {"ps5_addr": args.addr}, timeout=120)
    time.sleep(3)

    after = get_json(args.engine, f"/api/pkg/installed?addr={host}%3A9114&title_id={title_id}")
    say(f"installed after:  {json.dumps(after.get('artifacts', []), indent=None)}")
    say(f"verdict   {verdict}  after {time.monotonic()-t0:.1f}s")
    if dpi_elapsed:
        say(f"dpi call  {dpi_elapsed[0]:.1f}s  {json.dumps({k: v for k, v in dpi_reply.items() if k != '_logged'})}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
