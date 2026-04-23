#!/usr/bin/env python3
"""End-to-end test of mount/register/launch/unregister/unmount against
a live PS5. Each step has a tight timeout so if the payload wedges we
know exactly which call hung.

Invocation:
    python3 test_install_launch.py 192.168.1.50

Output is a running log — each line is a test step with its result.
Uses only the stdlib so it runs on any Python 3.8+.
"""
import json
import socket
import struct
import sys
import time
from typing import Tuple

FTX2_MAGIC = int.from_bytes(b"FTX2", "little")
FTX2_VERSION = 1
FRAME_HEADER_LEN = 28

# ── Frame types (must match engine/crates/ftx2-proto/src/lib.rs) ──
F_HELLO = 1
F_HELLO_ACK = 2
F_ERROR = 3
F_STATUS = 20
F_STATUS_ACK = 21
F_FS_LIST_VOLUMES = 34
F_FS_LIST_VOLUMES_ACK = 35
F_FS_MOUNT = 52
F_FS_MOUNT_ACK = 53
F_FS_UNMOUNT = 54
F_FS_UNMOUNT_ACK = 55
F_APP_REGISTER = 56
F_APP_REGISTER_ACK = 57
F_APP_UNREGISTER = 58
F_APP_UNREGISTER_ACK = 59
F_APP_LAUNCH = 60
F_APP_LAUNCH_ACK = 61
F_APP_LIST_REGISTERED = 62
F_APP_LIST_REGISTERED_ACK = 63

NAMES = {
    F_HELLO_ACK: "HELLO_ACK",
    F_ERROR: "ERROR",
    F_STATUS_ACK: "STATUS_ACK",
    F_FS_LIST_VOLUMES_ACK: "FS_LIST_VOLUMES_ACK",
    F_FS_MOUNT_ACK: "FS_MOUNT_ACK",
    F_FS_UNMOUNT_ACK: "FS_UNMOUNT_ACK",
    F_APP_REGISTER_ACK: "APP_REGISTER_ACK",
    F_APP_UNREGISTER_ACK: "APP_UNREGISTER_ACK",
    F_APP_LAUNCH_ACK: "APP_LAUNCH_ACK",
    F_APP_LIST_REGISTERED_ACK: "APP_LIST_REGISTERED_ACK",
}


def recv_exact(sock: socket.socket, n: int) -> bytes:
    chunks = []
    got = 0
    while got < n:
        chunk = sock.recv(n - got)
        if not chunk:
            raise RuntimeError("connection closed mid-frame")
        chunks.append(chunk)
        got += len(chunk)
    return b"".join(chunks)


def pack_frame(frame_type: int, body: bytes, trace_id: int = 1) -> bytes:
    return (
        struct.pack(
            "<IHHIQQ",
            FTX2_MAGIC,
            FTX2_VERSION,
            frame_type,
            0,
            len(body),
            trace_id,
        )
        + body
    )


def roundtrip(
    host: str,
    port: int,
    frame_type: int,
    body: bytes,
    timeout: float,
) -> Tuple[int, bytes]:
    """Send a frame and read exactly one response. Returns (resp_type, body)."""
    with socket.create_connection((host, port), timeout=timeout) as sock:
        sock.settimeout(timeout)
        sock.sendall(pack_frame(frame_type, body))
        hdr = recv_exact(sock, FRAME_HEADER_LEN)
        magic, version, rtype, flags, body_len, trace_id = struct.unpack(
            "<IHHIQQ", hdr
        )
        if magic != FTX2_MAGIC:
            raise RuntimeError(f"bad magic {magic:#x}")
        resp_body = recv_exact(sock, body_len) if body_len else b""
        return rtype, resp_body


class Step:
    def __init__(self, name: str):
        self.name = name
        self.start = time.time()

    def ok(self, msg: str = ""):
        dt = (time.time() - self.start) * 1000
        extra = f" — {msg}" if msg else ""
        print(f"✓ {self.name}  ({dt:.0f} ms){extra}")

    def fail(self, msg: str):
        dt = (time.time() - self.start) * 1000
        print(f"✗ {self.name}  ({dt:.0f} ms) — {msg}")


def main() -> int:
    host = sys.argv[1] if len(sys.argv) > 1 else "192.168.1.50"
    port = 9114

    print(f"=== PS5 install/launch test against {host}:{port} ===\n")

    # Step 1 — HELLO. Quick reachability check that also exercises the
    # simplest handler path. If this fails everything else will too.
    s = Step("1. HELLO (alive check)")
    try:
        rtype, body = roundtrip(host, port, F_HELLO, b"", timeout=3.0)
        if rtype == F_HELLO_ACK:
            s.ok(body.decode("utf-8", errors="replace")[:80])
        else:
            s.fail(f"got {NAMES.get(rtype, rtype)}: {body!r}")
            return 1
    except Exception as e:
        s.fail(f"{type(e).__name__}: {e}")
        return 1

    # Step 2 — FS_LIST_VOLUMES. Also a quick sanity check; if this
    # hangs we know the mgmt dispatcher is wedged.
    s = Step("2. FS_LIST_VOLUMES")
    try:
        rtype, body = roundtrip(
            host, port, F_FS_LIST_VOLUMES, b"", timeout=5.0
        )
        if rtype == F_FS_LIST_VOLUMES_ACK:
            try:
                parsed = json.loads(body.decode("utf-8"))
                n = len(parsed.get("volumes", []))
                s.ok(f"{n} volumes visible")
            except Exception:
                s.ok("(unparseable body)")
        else:
            s.fail(f"got {NAMES.get(rtype, rtype)}: {body!r}")
    except Exception as e:
        s.fail(f"{type(e).__name__}: {e}")

    # Step 3 — FS_MOUNT the .exfat image. Tight-ish timeout (10 s).
    # MDIOCATTACH + nmount are fast on healthy FS; a hang past 10 s
    # strongly suggests the payload got stuck.
    image_path = "/data/homebrew/PPSA00000.exfat"
    s = Step(f"3. FS_MOUNT  {image_path}")
    mount_point = None
    try:
        req = json.dumps({"image_path": image_path}, separators=(",", ":")).encode()
        rtype, body = roundtrip(host, port, F_FS_MOUNT, req, timeout=10.0)
        if rtype == F_FS_MOUNT_ACK:
            try:
                parsed = json.loads(body.decode("utf-8"))
                mount_point = parsed.get("mount_point")
                s.ok(f"mount_point={mount_point} dev={parsed.get('dev_node')}")
            except Exception:
                s.ok(f"(ack body: {body!r})")
        else:
            # ERROR body is a plain string; other responses unexpected.
            s.fail(f"got {NAMES.get(rtype, rtype)}: {body.decode('utf-8', 'replace')}")
    except socket.timeout:
        s.fail("TIMEOUT — payload probably wedged on MDIOCATTACH/nmount")
    except Exception as e:
        s.fail(f"{type(e).__name__}: {e}")

    # Step 4 — APP_REGISTER on the direct game folder. This exercises
    # the Sony install path without any mount indirection, which is
    # the shortest path to a working install call.
    game_folder = "/mnt/ext1/etaHEN/games/PPSA00000-app"
    s = Step(f"4. APP_REGISTER  {game_folder}")
    try:
        req = json.dumps({"src_path": game_folder}, separators=(",", ":")).encode()
        rtype, body = roundtrip(host, port, F_APP_REGISTER, req, timeout=30.0)
        if rtype == F_APP_REGISTER_ACK:
            try:
                parsed = json.loads(body.decode("utf-8"))
                s.ok(
                    f"title_id={parsed.get('title_id')} "
                    f"name={parsed.get('title_name')!r} "
                    f"nullfs={parsed.get('used_nullfs')}"
                )
            except Exception:
                s.ok(f"(ack: {body!r})")
        else:
            s.fail(f"got {NAMES.get(rtype, rtype)}: {body.decode('utf-8', 'replace')}")
    except socket.timeout:
        s.fail("TIMEOUT — payload probably wedged on sceAppInstUtilAppInstallTitleDir")
    except Exception as e:
        s.fail(f"{type(e).__name__}: {e}")

    # Step 5 — APP_LIST_REGISTERED. Should show titles from app.db.
    s = Step("5. APP_LIST_REGISTERED")
    ppsa10112_registered = False
    try:
        rtype, body = roundtrip(
            host, port, F_APP_LIST_REGISTERED, b"", timeout=15.0
        )
        if rtype == F_APP_LIST_REGISTERED_ACK:
            try:
                parsed = json.loads(body.decode("utf-8"))
                apps = parsed.get("apps", [])
                s.ok(f"{len(apps)} titles in app.db")
                for app in apps:
                    if app.get("title_id") == "PPSA00000":
                        ppsa10112_registered = True
                        print(f"    · PPSA00000 registered (src={app.get('src')!r})")
                    elif app.get("title_id") == "PPSA00000":
                        print(f"    · PPSA00000 registered (src={app.get('src')!r})")
            except Exception:
                s.ok(f"(unparseable body: {body[:80]!r})")
        else:
            s.fail(f"got {NAMES.get(rtype, rtype)}: {body.decode('utf-8', 'replace')}")
    except Exception as e:
        s.fail(f"{type(e).__name__}: {e}")

    # Step 6 — APP_LAUNCH. The user's primary concern: "Run" doesn't
    # work from Library. If PPSA00000 is registered, try launching it.
    if ppsa10112_registered:
        s = Step("6. APP_LAUNCH  PPSA00000")
        try:
            req = json.dumps({"title_id": "PPSA00000"}, separators=(",", ":")).encode()
            rtype, body = roundtrip(host, port, F_APP_LAUNCH, req, timeout=15.0)
            if rtype == F_APP_LAUNCH_ACK:
                s.ok("launch issued (does the game actually start? check PS5 screen)")
            else:
                s.fail(f"got {NAMES.get(rtype, rtype)}: {body.decode('utf-8', 'replace')}")
        except socket.timeout:
            s.fail("TIMEOUT — sceLncUtilLaunchApp wedged")
        except Exception as e:
            s.fail(f"{type(e).__name__}: {e}")
    else:
        print("⊘ 6. APP_LAUNCH PPSA00000 skipped (not registered)")

    # Step 7 — APP_UNREGISTER. Clean up even if install didn't fully
    # work — the unmount path is tolerant of "not registered".
    s = Step("7. APP_UNREGISTER  PPSA00000")
    try:
        req = json.dumps({"title_id": "PPSA00000"}, separators=(",", ":")).encode()
        rtype, body = roundtrip(host, port, F_APP_UNREGISTER, req, timeout=15.0)
        if rtype == F_APP_UNREGISTER_ACK:
            s.ok(body.decode("utf-8", "replace")[:80] if body else "clean")
        elif rtype == F_ERROR:
            # "not registered" is fine — idempotent cleanup
            s.ok(f"noop: {body.decode('utf-8', 'replace')}")
        else:
            s.fail(f"got {NAMES.get(rtype, rtype)}: {body.decode('utf-8', 'replace')}")
    except Exception as e:
        s.fail(f"{type(e).__name__}: {e}")

    # Step 8 — FS_UNMOUNT the image if we had a mount point.
    if mount_point:
        s = Step(f"8. FS_UNMOUNT  {mount_point}")
        try:
            req = json.dumps({"mount_point": mount_point}, separators=(",", ":")).encode()
            rtype, body = roundtrip(host, port, F_FS_UNMOUNT, req, timeout=10.0)
            if rtype == F_FS_UNMOUNT_ACK:
                s.ok("unmounted + detached")
            else:
                s.fail(f"got {NAMES.get(rtype, rtype)}: {body.decode('utf-8', 'replace')}")
        except Exception as e:
            s.fail(f"{type(e).__name__}: {e}")
    else:
        print("⊘ 8. FS_UNMOUNT skipped (no mount_point from step 3)")

    # Final alive check — if anything above wedged the payload, the
    # HELLO here will fail and that's our "payload-dead after X" signal.
    print("")
    s = Step("9. Post-test HELLO (alive?)")
    try:
        rtype, body = roundtrip(host, port, F_HELLO, b"", timeout=3.0)
        if rtype == F_HELLO_ACK:
            s.ok("payload survived the test run")
        else:
            s.fail(f"got {NAMES.get(rtype, rtype)}")
    except Exception as e:
        s.fail(f"{type(e).__name__}: {e} — PAYLOAD WEDGED")
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
