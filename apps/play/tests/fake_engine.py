#!/usr/bin/env python3
# PS5Upload Play - a stand-in for the PS5Upload engine's guest socket, app channel and discovery
# responder, for the app's host tests. Speaks the same messages as engine join.rs, play_app.rs
# and play_discovery.rs; records what the app sent into a JSON report.
# SPDX-License-Identifier: GPL-3.0-or-later
import argparse
import base64
import hashlib
import json
import socket
import struct
import threading
import time

GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
report = {"hellos": [], "pads": [], "keyframes": 0, "pings": 0, "stats": [], "leaves": 0,
          "app_hellos": [], "intents": [], "bad_pads": 0, "connections": 0}
lock = threading.Lock()


def rec(key, value=None):
    with lock:
        if value is None:
            report[key] += 1
        else:
            report[key].append(value)


class Ws:
    def __init__(self, conn):
        self.c = conn
        self.buf = b""
        self.wlock = threading.Lock()

    def read_exact(self, n):
        while len(self.buf) < n:
            d = self.c.recv(65536)
            if not d:
                raise ConnectionError("closed")
            self.buf += d
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv(self):
        """(opcode, payload) of the next whole message (client frames are masked)."""
        msg, op = b"", None
        while True:
            h = self.read_exact(2)
            fin, opcode, masked, ln = h[0] & 0x80, h[0] & 0x0F, h[1] & 0x80, h[1] & 0x7F
            if not masked:
                raise ValueError("client frame not masked")
            if ln == 126:
                ln = struct.unpack(">H", self.read_exact(2))[0]
            elif ln == 127:
                ln = struct.unpack(">Q", self.read_exact(8))[0]
            mask = self.read_exact(4)
            data = bytes(b ^ mask[i % 4] for i, b in enumerate(self.read_exact(ln)))
            if opcode >= 8:
                if opcode == 8:
                    raise ConnectionError("close")
                if opcode == 9:
                    self.send(10, data)
                continue
            if opcode != 0:
                op = opcode
            msg += data
            if fin:
                return op, msg

    def send(self, opcode, data):
        if isinstance(data, str):
            data = data.encode()
        n = len(data)
        if n < 126:
            h = struct.pack("BB", 0x80 | opcode, n)
        elif n < 65536:
            h = struct.pack(">BBH", 0x80 | opcode, 126, n)
        else:
            h = struct.pack(">BBQ", 0x80 | opcode, 127, n)
        with self.wlock:
            self.c.sendall(h + data)

    def text(self, obj):
        self.send(1, json.dumps(obj))


def parse_pad(b):
    """join.rs parse_pad: [1][u32 seq][u32 buttons][6 x u8][n][n x (u8, u16, u16)] [2][6 x f32]"""
    if len(b) < 16 or b[0] != 1:
        return None
    seq, buttons = struct.unpack_from("<II", b, 1)
    lx, ly, rx, ry, l2, r2, n = b[9:16]
    if n > 2 or len(b) < 16 + n * 5:
        return None
    touch = [struct.unpack_from("<BHH", b, 16 + i * 5) for i in range(n)]
    rest = b[16 + n * 5:]
    motion = None
    if rest:
        if rest[0] != 2 or len(rest) != 25:
            return None
        motion = struct.unpack_from("<6f", rest, 1)
    return {"seq": seq, "buttons": buttons, "sticks": [lx, ly, rx, ry], "triggers": [l2, r2],
            "touch": touch, "motion": motion}


def guest(ws, args, state):
    kind, hello = ws.recv()
    h = json.loads(hello)["hello"]
    rec("hellos", h)
    resume = h.get("resume")
    if resume and resume != "tok1":
        ws.text({"stage": "refused", "why": "expired"})
        return
    if args.refuse:
        ws.text({"stage": "refused", "why": args.refuse})
        return
    if not resume:
        ws.text({"stage": "asking"})
        time.sleep(0.2)
    joined = {"stage": "joined", "player": 2, "token": "tok1", "hevc": True, "track": False, "audio": False}
    if not args.old:
        joined["codec_warning"] = None
    ws.text(joined)
    timed = bool(h.get("client", {}).get("caps", {}).get("timing")) and not args.old
    stop = threading.Event()

    def reader():
        try:
            while not stop.is_set():
                op, data = ws.recv()
                if op == 2:
                    p = parse_pad(data)
                    if p is None:
                        rec("bad_pads")
                    else:
                        rec("pads", p)
                    continue
                m = json.loads(data)
                if "leave" in m:
                    rec("leaves")
                    stop.set()
                    return
                if m.get("keyframe"):
                    rec("keyframes")
                if "ping" in m:
                    rec("pings")
                    pong = {"pong": m["ping"]}
                    if timed:
                        pong["engine_us"] = int(time.monotonic() * 1e6)
                    ws.text(pong)
                if "stats" in m:
                    rec("stats", m["stats"])
        except Exception:
            stop.set()

    threading.Thread(target=reader, daemon=True).start()
    vps = bytes([0, 0, 0, 1, 0x40, 1, 0xC, 1])
    ws.send(2, b"\x00" + vps)
    ws.send(2, b"\x03" + bytes([2, 16]) + struct.pack(">II", 48000, 480) + b"\0\0\0\0")
    t0 = time.monotonic()
    n = 0
    sent_drop = state.get("dropped", False)
    try:
        while not stop.is_set() and time.monotonic() - t0 < args.seconds:
            n += 1
            # a picture every 16 ms: an IDR (type 19) every 60, else TRAIL_R; two sound frames
            nal = 0x26 if n % 60 == 1 else 0x02
            au = bytes([0, 0, 0, 1, nal, 1]) + bytes(200)
            if timed:
                ws.send(2, b"\x07" + struct.pack(">Q", int(time.monotonic() * 1e6)) + au)
            else:
                ws.send(2, b"\x01" + au)
            for k in range(2):
                idx = (n * 2 + k) & 0xFFFF
                ws.send(2, b"\x04" + struct.pack(">H", idx) + bytes(40))
            if n % 60 == 0:
                ws.text({"status": {"player": 2, "state": "playing", "rtt_ms": 5, "host": "Fake host",
                                    "block_system": True, "stream_seat": False, "plays_as": None,
                                    "seat_note": None, "move_first": False, "host_user": None,
                                    "took_host": False, "queue": 0, "skipped": 0, "codec_warning": None}})
                ws.send(9, struct.pack("<Q", int(time.monotonic() * 1000)))
            if n == 30:
                ws.text({"rumble": [200, 40]})
                ws.text({"light": [0, 0, 255]})
            if args.drop_after and not sent_drop and time.monotonic() - t0 > args.drop_after:
                state["dropped"] = True
                ws.c.shutdown(socket.SHUT_RDWR)
                return
            time.sleep(0.016)
        if not stop.is_set() and args.bye:
            ws.text({"bye": args.bye})
            ws.send(8, b"")
        stop.wait(2)
    except OSError:
        pass


def app(ws, args, state):
    kind, hello = ws.recv()
    h = json.loads(hello)["hello"]
    rec("app_hellos", h)
    good = h.get("token") == "apptok"
    full = {"doc": {"rev": 1, "paired": True,
                    "engine": {"version": "13.0.0", "name": "Fake engine"},
                    "console": {"addr": "127.0.0.1", "name": "Test PS5"},
                    "session": {"state": "playing", "codec": "h265", "bitrate_kbps": 15000},
                    "together": {"open": True, "playing": True,
                                 "invite": {"code": "ABCDEF", "code_display": "ABC-DEF",
                                            "url": "http://192.168.1.2:19113/join#i=key", "works": "wifi",
                                            "expires_in_s": 1700},
                                 "pending": [{"id": 7, "name": "Sam", "client_kind": "ps5", "codec_warning": None}],
                                 "guests": [{"id": 3, "player": 2, "name": "Alex", "state": "playing", "rtt_ms": 4,
                                             "via": "lan", "client_kind": None, "client_app": None,
                                             "codec_warning": None}],
                                 "settings": {"block_system_buttons": True, "max_players": 3, "stream_seat": False,
                                              "invites": "anywhere", "rev": 4},
                                 "max_guests": 3},
                    "remote_play": {"paired": True}, "helper": {"present": None},
                    "keep_awake": "transfers", "app": {}}}
    if h.get("token") and not good:
        ws.text({"auth": "invalid"})
    ws.text(full if good else {"doc": {"engine": True, "version": "13.0.0", "paired": False}})
    while True:
        op, data = ws.recv()
        m = json.loads(data)
        rec("intents", m)
        req = m.get("req")
        if "pair" in m:
            ws.text({"pairing": {"code": "PQRSTU", "code_display": "PQR-STU", "expires_in_s": 300}})
            time.sleep(0.3)
            ws.text({"paired": {"id": "a1", "token": "apptok"}})
            good = True
            ws.text(full)
            continue
        if "ping" in m:
            ws.text({"pong": m["ping"]})
            continue
        kind = next(iter(k for k in m if k != "req"))
        ws.text({"done": kind, "req": req, "ok": True})
        if kind == "decide":
            full["doc"]["together"]["pending"] = []
            ws.text(full)


RP_RAW = {"service_enabled": 0, "has_per_user": 1, "user_enabled": 0, "account_uid": 1, "account_id_raw": 5,
          "account_type": "np"}
helper_state = {"rp_on": False, "pin": "idle"}


def helper_api(conn, method, path, hdr, rest):
    """payload/include/app_api.h, canned: Host must be 127.0.0.1:<port>, no Origin."""
    def answer(code, obj):
        body = json.dumps(obj).encode()
        conn.sendall(b"HTTP/1.1 %d X\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n" % (code, len(body)) + body)
    host = hdr.get("host", "")
    if not (host.startswith("127.0.0.1:") or host.startswith("localhost:")):
        return answer(403, {"error": "bad_host"})
    if "origin" in hdr:
        return answer(403, {"error": "origin_refused"})
    n = int(hdr.get("content-length", "0") or 0)
    while len(rest) < n:
        rest += conn.recv(4096)
    body = json.loads(rest[:n] or b"{}")
    rec("intents", {"helper": [method, path, body]})
    on = helper_state["rp_on"]
    rp = {"ready": on, "service_on": on, "user_on": on, "needs_user_permission": True, "signed_in": True,
          "psn_activated": True, "readiness": RP_RAW}
    if (method, path) == ("GET", "/v1/rp"):
        return answer(200, rp)
    if (method, path) == ("POST", "/v1/rp/enable"):
        if body.get("confirm") is not True:
            return answer(400, {"error": "confirm_required"})
        helper_state["rp_on"] = True
        rp.update(ready=True, service_on=True, user_on=True)
        return answer(200, rp)
    if (method, path) == ("POST", "/v1/rp/pin"):
        helper_state["pin"] = "waiting"
        return answer(200, {"state": "waiting", "pin": "12345678", "seconds_left": 300})
    if (method, path) == ("GET", "/v1/rp/pin"):
        return answer(200, {"state": helper_state["pin"], "pin": "12345678", "seconds_left": 299, "err": None})
    if (method, path) == ("POST", "/v1/rp/pin/cancel"):
        helper_state["pin"] = "idle"
        return answer(200, {"ok": True})
    if (method, path) == ("GET", "/v1/mesh"):
        # As the helper answers: "engine" is the one whose hello said where Play reaches it (on
        # a port of its own here), and it is in "online" too.
        offered = {"name": "Living room PC", "kind": "desktop", "version": "13.0.0", "addr": "192.168.1.20",
                   "tailnet": False, "port": 19200, "join_ws": "/api/join/ws", "app_ws": "/api/play/app"}
        return answer(200, {"engine": offered, "online": [
            offered,
            {"name": "Friend's PS5", "kind": "ps5", "version": "13.0.0", "addr": "192.168.1.30", "tailnet": False,
             "port": None, "join_ws": None, "app_ws": None},
            {"name": "Laptop", "kind": "desktop", "version": "13.0.0", "addr": "100.101.102.103", "tailnet": True,
             "port": None, "join_ws": None, "app_ws": None}]})
    if (method, path) == ("POST", "/v1/tailnet/forward"):
        if not str(body.get("peer", "")).startswith("100."):
            return answer(400, {"error": "not_tailnet_address"})
        return answer(200, {"local_port": 9182, "peer": body["peer"], "port": body.get("port")})
    return answer(404, {"error": "not_found"})


def serve(conn, args, state):
    try:
        conn.settimeout(30)
        req = b""
        while b"\r\n\r\n" not in req:
            d = conn.recv(4096)
            if not d:
                return
            req += d
        head, rest = req.split(b"\r\n\r\n", 1)
        lines = head.decode().split("\r\n")
        path = lines[0].split(" ")[1]
        hdr = {l.split(":", 1)[0].lower(): l.split(":", 1)[1].strip() for l in lines[1:] if ":" in l}
        if path.startswith("/v1/"):
            return helper_api(conn, lines[0].split(" ")[0], path, hdr, rest)
        if path not in ("/api/join/ws", "/api/play/app") or hdr.get("upgrade", "").lower() != "websocket":
            if path == "/api/version":
                body = json.dumps({"version": "13.0.0"}).encode()
                conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n" % len(body) + body)
            else:
                conn.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            return
        rec("connections")
        acc = base64.b64encode(hashlib.sha1(hdr["sec-websocket-key"].encode() + GUID).digest())
        conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                     b"Sec-WebSocket-Accept: " + acc + b"\r\n\r\n")
        ws = Ws(conn)
        ws.buf = rest
        (guest if path == "/api/join/ws" else app)(ws, args, state)
    except (ConnectionError, OSError, ValueError, KeyError, json.JSONDecodeError):
        pass
    finally:
        try:
            conn.close()
        except OSError:
            pass


def discovery(port):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        s.bind(("0.0.0.0", 19261))
    except OSError:
        return
    while True:
        data, peer = s.recvfrom(2048)
        if data.startswith(b"PS5UPLOAD_PLAY_DISCOVER v1"):
            body = json.dumps({"name": "Fake engine", "port": port, "version": "13.0.0", "id": "fake1"}).encode()
            if len(body) <= len(data):
                s.sendto(body, peer)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--port-file")
    ap.add_argument("--report", required=True)
    ap.add_argument("--seconds", type=float, default=3)
    ap.add_argument("--old", action="store_true", help="answer as an engine before 13.0.0 (no caps)")
    ap.add_argument("--refuse")
    ap.add_argument("--bye")
    ap.add_argument("--drop-after", type=float, default=0)
    ap.add_argument("--discovery", action="store_true")
    ap.add_argument("--lifetime", type=float, default=20)
    args = ap.parse_args()
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", args.port))
    srv.listen(8)
    port = srv.getsockname()[1]
    if args.port_file:
        with open(args.port_file, "w") as f:
            f.write(str(port))
    if args.discovery:
        threading.Thread(target=discovery, args=(port,), daemon=True).start()
    state = {}
    srv.settimeout(0.2)
    end = time.monotonic() + args.lifetime
    while time.monotonic() < end:
        try:
            c, _ = srv.accept()
        except socket.timeout:
            with lock:
                with open(args.report, "w") as f:
                    json.dump(report, f)
            continue
        threading.Thread(target=serve, args=(c, args, state), daemon=True).start()


if __name__ == "__main__":
    main()
