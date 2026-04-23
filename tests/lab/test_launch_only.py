#!/usr/bin/env python3
"""Tight focus: does APP_LAUNCH work on a title already registered
in app.db? This is the specific case the user's "Run" button hits:
the title was registered by some PS5-side tool (not by us), so
Library shows a Run button that calls appLaunch directly — no
register step.

If this succeeds: the wedge is only in register, launch is fine,
and we have a clean path for "Run known-registered titles".
If this fails: even launch has the privilege-gate issue, and we
need a different strategy (process injection or kernel patches).
"""
import json
import socket
import struct
import sys
import time

FTX2_MAGIC = int.from_bytes(b"FTX2", "little")
F_HELLO = 1
F_APP_LAUNCH = 60
F_APP_LAUNCH_ACK = 61
F_APP_LIST_REGISTERED = 62
F_APP_LIST_REGISTERED_ACK = 63
F_ERROR = 3


def recv_exact(s, n):
    out = b""
    while len(out) < n:
        c = s.recv(n - len(out))
        if not c:
            raise RuntimeError("closed")
        out += c
    return out


def frame(ft, body, trace=1):
    return struct.pack("<IHHIQQ", FTX2_MAGIC, 1, ft, 0, len(body), trace) + body


def send(host, port, ft, body, timeout):
    t0 = time.time()
    with socket.create_connection((host, port), timeout=timeout) as s:
        s.settimeout(timeout)
        s.sendall(frame(ft, body))
        hdr = recv_exact(s, 28)
        _, _, rtype, _, blen, _ = struct.unpack("<IHHIQQ", hdr)
        body_out = recv_exact(s, blen) if blen else b""
    return rtype, body_out, (time.time() - t0) * 1000


def main():
    host = sys.argv[1] if len(sys.argv) > 1 else "192.168.1.50"
    port = 9114
    print(f"=== LAUNCH-ONLY probe against {host}:{port} ===\n")

    # Confirm payload is alive
    try:
        rt, b, ms = send(host, port, F_HELLO, b"", 3.0)
        print(f"✓ HELLO ({ms:.0f} ms) — payload responsive")
    except Exception as e:
        print(f"✗ HELLO — {e}")
        return 1

    # Look at what's registered
    try:
        rt, b, ms = send(host, port, F_APP_LIST_REGISTERED, b"", 10.0)
        if rt == F_APP_LIST_REGISTERED_ACK:
            apps = json.loads(b.decode()).get("apps", [])
            print(f"✓ LIST ({ms:.0f} ms) — {len(apps)} titles in app.db")
            candidates = [a for a in apps if a.get("title_id", "").startswith(("PPSA", "PLAS", "CUSA"))][:5]
            print("    Candidates to test launch:")
            for a in candidates:
                print(f"      · {a.get('title_id')}  src={a.get('src')!r}")
        else:
            print(f"✗ LIST — got frame type {rt}")
    except Exception as e:
        print(f"✗ LIST — {e}")

    # The critical test: launch PPSA00000 directly
    print("\n--- Testing APP_LAUNCH on PPSA00000 (pre-registered) ---")
    try:
        body = json.dumps({"title_id": "PPSA00000"}, separators=(",", ":")).encode()
        rt, b, ms = send(host, port, F_APP_LAUNCH, body, 15.0)
        if rt == F_APP_LAUNCH_ACK:
            print(f"✓ LAUNCH ({ms:.0f} ms) — CHECK YOUR PS5 SCREEN")
            print("  If the game starts, the Run button works for pre-registered titles.")
            print("  If the game doesn't start but we got OK, Sony silently no-opped the launch.")
        elif rt == F_ERROR:
            print(f"✗ LAUNCH ({ms:.0f} ms) — ERROR: {b.decode('utf-8', 'replace')}")
        else:
            print(f"? LAUNCH ({ms:.0f} ms) — unexpected frame type {rt}: {b!r}")
    except socket.timeout:
        print("✗ LAUNCH — TIMEOUT (sceLncUtilLaunchApp wedged)")
    except Exception as e:
        print(f"✗ LAUNCH — {e}")

    # Post-launch health check
    print("")
    try:
        rt, b, ms = send(host, port, F_HELLO, b"", 3.0)
        print(f"✓ post-test HELLO ({ms:.0f} ms) — payload survived launch")
    except Exception as e:
        print(f"✗ post-test HELLO — {e}  PAYLOAD WEDGED")
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
