#!/usr/bin/env python3
"""Quick FTX2 probe — sends a frame to mgmt port, returns the body."""
import json
import socket
import struct
import sys

FTX2_MAGIC = int.from_bytes(b"FTX2", "little")

def send_frame(host, port, frame_type, body=b""):
    pkt = struct.pack("<IHHIQQ", FTX2_MAGIC, 1, frame_type, 0, len(body), 1) + body
    s = socket.create_connection((host, port), timeout=15)
    s.sendall(pkt)
    hdr = b""
    while len(hdr) < 28:
        chunk = s.recv(28 - len(hdr))
        if not chunk:
            break
        hdr += chunk
    if len(hdr) < 28:
        s.close()
        return None, b""
    magic, ver, ftype, _, blen, _ = struct.unpack("<IHHIQQ", hdr)
    body_in = b""
    while len(body_in) < blen:
        chunk = s.recv(blen - len(body_in))
        if not chunk:
            break
        body_in += chunk
    s.close()
    return ftype, body_in

if __name__ == "__main__":
    if len(sys.argv) < 4:
        print("usage: ftx2_probe.py <host> <port> <frame_type> [json-body]", file=sys.stderr)
        sys.exit(1)
    host = sys.argv[1]
    port = int(sys.argv[2])
    ftype = int(sys.argv[3])
    body = sys.argv[4].encode() if len(sys.argv) > 4 else b""
    rt, rb = send_frame(host, port, ftype, body)
    print(f"resp_frame_type={rt} body_len={len(rb)}")
    try:
        parsed = json.loads(rb)
        print(json.dumps(parsed, indent=2))
    except Exception:
        print(rb[:2048].decode("utf-8", errors="replace"))
