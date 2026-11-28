#!/usr/bin/env python3
# PS5Upload Play - a stand-in for the PS5Upload engine's guest socket, app channel and discovery
# responder, for the app's host tests. Speaks the same messages as engine join.rs, play_app.rs
# and play_discovery.rs; records what the app sent into a JSON report.
# SPDX-License-Identifier: GPL-3.0-or-later
import argparse
import base64
import hashlib
import json
import os
import socket
import struct
import threading
import time

GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
report = {"hellos": [], "pads": [], "keyframes": 0, "pings": 0, "stats": [], "leaves": 0,
          "app_hellos": [], "intents": [], "bad_pads": 0, "connections": 0, "hub_hosts": [], "hub_host_msgs": [],
          "probe_sockets": 0, "probe_checks": [], "bursts": [], "replaced": 0, "bridges": [],
          "chats": [], "mics": [], "voice_up": [], "knocks": [], "http": [], "notices": [], "flags": []}
T0 = time.monotonic()
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


# ---- Play Rooms (docs/ROOMS.md): the room key, sealed knocks, lists and previews ----
ROOM_SK = int.from_bytes(hashlib.sha256(b"fake room key").digest(), "big") % (2**256 - 2**224) or 1


def b64u(b):
    return base64.urlsafe_b64encode(b).decode().rstrip("=")


def b64d(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def room_pk():
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives import serialization
    k = ec.derive_private_key(ROOM_SK, ec.SECP256R1())
    return b64u(k.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint))


def unseal(blob, challenge):
    """src/net/seal.hpp, as the host engine opens it."""
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    from cryptography.hazmat.primitives.kdf.hkdf import HKDF
    box = b64d(blob)
    if box[0] != 1:
        return None
    sk = ec.derive_private_key(ROOM_SK, ec.SECP256R1())
    shared = sk.exchange(ec.ECDH(), ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), box[1:66]))
    key = HKDF(algorithm=hashes.SHA256(), length=16, salt=b64d(challenge), info=b"ps5upload room knock v1").derive(shared)
    try:
        return json.loads(AESGCM(key).decrypt(box[66:78], box[78:], None))
    except Exception:
        return None


def knock_secret(ws, args, gid):
    """A knock that needs the password: challenge, sealed, checked. True when it is right."""
    c = b64u(os.urandom(16))
    ws.text({"knock": "challenge", "c": c, "pk": room_pk(), "gid": gid})
    _, data = ws.recv()
    m = json.loads(data)
    plain = unseal(m.get("blob", ""), c) if m.get("knock") == "sealed" else None
    ok = bool(plain) and plain.get("secret") == args.room_pw and plain.get("challenge") == c and plain.get("gid") == gid
    rec("knocks", {"sealed": m.get("knock") == "sealed", "ok": ok, "gid": gid})
    if not ok:
        # as the engine says it (join.rs): on the guest socket a refusal stage, on a lobby a knock answer
        ws.text({"stage": "refused", "why": "password", "tries_left": 4} if gid == "" else
                {"knock": "no", "why": "password", "tries_left": 4})
    return ok


ROOMS = [
    {"room": "lobbyA", "name": "Couch chaos", "host": "Sam", "tag": "4F2A",
     "game": {"title": "It Takes Two", "id": "PPSA01234"}, "proto": 2, "ver": "13.0.0",
     "caps": ["web", "ps5app", "preview", "chat", "voice"], "slots": [1, 4], "pw": True, "lang": "en",
     "region": "eu-west", "est_ms": 30, "age_s": 120},
    {"room": "lobbyB", "name": "Sunday racers", "host": "Kai", "tag": "9C11",
     "game": {"title": "Gran Turismo 7", "id": "PPSA01476"}, "proto": 2, "ver": "13.0.0",
     "caps": ["ps5app", "preview"], "slots": [4, 4], "pw": False, "lang": "de", "est_ms": 70, "age_s": 30},
]
PREVIEW = {"proto": 2, "caps": ["chat", "voice", "preview", "group"],
           "host": {"name": "Sam", "tag": "4F2A", "ver": "13.0.0", "playing_for_min": 42},
           "room": {"name": "Couch chaos", "visibility": "public", "size": 4, "pw": True, "approval": "ask",
                    "chat": "quick", "lang": "en", "region": "eu-west", "watchers": 0},
           "game": {"id": "PPSA01234", "title": "It Takes Two"},
           "stream": {"res": "1080p", "fps": 60, "codec": "hevc", "kbps": 15000, "hdr": False},
           "host_link": {"rtt_ms": 2, "tier": "excellent"},
           "players": [{"name": "Sam", "seat": 1, "kind": "host"},
                       {"name": "Mia", "seat": 2, "kind": "ps5", "tier": "good", "rtt_ms": 31}],
           "asking": 0}
rooms_429 = {"left": 0}


def http_rooms(conn, path):
    """The hub's GET /rooms, /rooms/status, /rooms/<id>/preview and the engine's /api/join/preview."""
    def answer(code, obj, extra=b""):
        body = json.dumps(obj).encode()
        conn.sendall(b"HTTP/1.1 %d X\r\nContent-Type: application/json\r\n%sContent-Length: %d\r\n\r\n"
                     % (code, extra, len(body)) + body)
    rec("http", path)
    p, _, q = path.partition("?")
    params = dict(kv.split("=", 1) for kv in q.split("&") if "=" in kv)
    if p == "/rooms":
        if rooms_429["left"] > 0:
            rooms_429["left"] -= 1
            return answer(429, {"why": "slow_down", "retry_s": 1}, b"Retry-After: 1\r\n")
        page = int(params.get("page", "0") or 0)
        rooms = ROOMS[page:page + 1]
        return answer(200, {"you": {"country": "DE", "region": "eu-west"}, "proto": {"min": 2, "latest": 2},
                            "app": {"latest": "13.0.0"}, "rooms": rooms, "next": page + 1 if page + 1 < len(ROOMS) else None})
    if p == "/rooms/status":
        ids = params.get("r", "").split(",")
        out = {i: {"listed": False, "locked": False, "slots": [2, 4], "pw": False, "proto": 2,
                   "game": {"title": "ASTRO BOT", "id": "PPSA02345"}} for i in ids if i == "lobbyF"}
        return answer(200, {"every_s": 60, "rooms": out})
    if p.startswith("/rooms/") and p.endswith("/preview"):
        rid = p[len("/rooms/"):-len("/preview")]
        if rid == "lobbyA":
            return answer(200, PREVIEW)
        return answer(404, {"why": "no_room"})
    if p == "/api/join/preview":
        return answer(200, PREVIEW)
    return answer(404, {"why": "not_found"})


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


def guest(ws, args, state, via=None):
    kind, hello = ws.recv()
    h = json.loads(hello)["hello"]
    rec("hellos", dict(h, _via=via) if via else h)
    resume = h.get("resume")
    if resume and state.get("live"):
        rec("replaced")
    if resume and resume != "tok1":
        ws.text({"stage": "refused", "why": "expired"})
        return
    # Play Rooms: a knock that needs the password, a name already in the room
    if h.get("knock") and args.room_pw and not resume and via != "lobby":
        if not knock_secret(ws, args, ""):
            return
    if args.name_taken and h.get("name") == args.name_taken and not resume:
        s = args.name_taken + " (2)"
        ws.text({"stage": "refused", "why": "name_taken", "suggest": s, "name_taken": {"suggest": s}})
        return
    if args.refuse:
        ws.text({"stage": "refused", "why": args.refuse})
        return
    # Through a bridge the guest is in an internet room (join.rs): no friend pass is taken or given
    # there, and something to show is needed (the invite's key or a code).
    internet = via == "bridge"
    if internet and not resume and not h.get("invite") and not h.get("code"):
        ws.text({"stage": "refused", "why": "key"})
        return
    if internet and args.key and h.get("invite") and h.get("invite") != args.key:
        ws.text({"stage": "refused", "why": "key"})
        return
    # Play again (join_friends.rs): a pass this engine gave, or refused "friend".
    again = None if internet else h.get("again")
    if again and again != "pass1":
        ws.text({"stage": "refused", "why": "friend"})
        return
    if not resume:
        ws.text({"stage": "asking"})
        time.sleep(0.2)
    joined = {"stage": "joined", "player": 2, "token": "tok1", "hevc": True, "track": False, "audio": False}
    if h.get("remember") and not again and not resume and not internet:
        joined["friend"] = "pass1"
    if not args.old:
        joined["codec_warning"] = None
    rooms = h.get("proto") == 2 and not args.old
    if rooms:
        joined["lobby"] = "lobbyF"
    ws.text(joined)
    if rooms:
        ws.text({"room": {"name": "Couch chaos", "size": 4,
                          "players": [{"id": 1, "name": "Fake host", "seat": 1, "color": [0, 0, 255]},
                                      {"id": 2, "name": h.get("name", ""), "seat": 2, "you": True},
                                      {"id": 3, "name": "Mia", "seat": 3}],
                          "you": {"rtt_ms": 5, "tier": "excellent"}, "host_link": {"rtt_ms": 2, "tier": "excellent"},
                          "chat": "text", "voice": "ptt", "muted": {"voice": False, "chat": False}}})
        # the engine's chat and voice (API.md "Play Rooms chat and voice"): nothing until the notice
        ws.text({"comms": {"notice": 1, "accepted": False, "chat": "on", "voice": "ptt", "caps": ["chat", "voice"]}})
    timed = bool(h.get("client", {}).get("caps", {}).get("timing")) and not args.old
    stop = threading.Event()

    def reader():
        try:
            while not stop.is_set():
                op, data = ws.recv()
                if op == 2 and data[:1] == b"\x0a":
                    # room voice up: [0x0A][seq u16][level u8][Opus]
                    if len(data) >= 5:
                        rec("voice_up", {"seq": struct.unpack(">H", data[1:3])[0], "level": data[3], "len": len(data) - 4})
                    continue
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
                if "notice" in m:
                    rec("notices", m["notice"])
                    comms["accepted"] = True
                    ws.text({"comms": {"notice": 1, "accepted": True, "chat": "on", "voice": "ptt",
                                       "caps": ["chat", "voice"]}})
                    ws.text({"chat_history": [{"id": 1, "sid": 0, "from": "Fake host", "player": 1, "host": True,
                                               "t": 1, "quick": 1}]})
                    ws.text({"voice": voice_state()})
                    ws.text({"chat": {"id": 2, "sid": 3, "from": "Mia", "player": 3, "host": False, "t": 2, "quick": 3}})
                if "flag" in m:
                    rec("flags", m["flag"])
                if "chat" in m:
                    rec("chats", m["chat"])
                    if not comms["accepted"]:
                        ws.text({"comms_refused": {"what": "chat", "why": "notice", "notice": 1}})
                    else:
                        comms["id"] += 1
                        line = {"id": comms["id"], "sid": 2, "from": h.get("name", ""), "player": 2, "host": False, "t": 3}
                        line.update(m["chat"])
                        ws.text({"chat": line})
                if "mic" in m:
                    rec("mics", m["mic"])
                    if m["mic"] in ("on", "off"):
                        comms["mic"] = m["mic"]
                        ws.text({"voice": voice_state()})
        except Exception:
            stop.set()

    comms = {"accepted": False, "id": 10, "mic": "off"}

    def voice_state():
        return {"mode": "ptt", "you": 2, "mic": comms["mic"], "ptt": False, "paused": None, "stepped": False,
                "kbps": 24, "frame_ms": 20, "speaking": [],
                "people": [{"sid": 0, "name": "Fake host", "player": 1, "host": True, "muted": False, "paused": None, "mic": "on"},
                           {"sid": 2, "name": h.get("name", ""), "player": 2, "host": False, "muted": False, "paused": None, "mic": comms["mic"]},
                           {"sid": 3, "name": "Mia", "player": 3, "host": False, "muted": False, "paused": None, "mic": "on"}]}

    threading.Thread(target=reader, daemon=True).start()
    me = object()
    state["live"] = me
    vps = bytes([0, 0, 0, 1, 0x40, 1, 0xC, 1])
    ws.send(2, b"\x00" + vps)
    ws.send(2, b"\x03" + bytes([2, 16]) + struct.pack(">II", 48000, 480) + b"\0\0\0\0")
    t0 = time.monotonic()
    n = 0
    sent_drop = state.get("dropped", False)
    try:
        while not stop.is_set() and time.monotonic() - t0 < args.seconds:
            if state.get("live") is not me:
                # a newer socket took this guest's seat (a change of way, join.rs "replaced")
                ws.text({"bye": "replaced"})
                return
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
            if rooms and n % 6 == 0:
                # room voice down: [9][n][(speaker, seq u16, len, Opus)...], two speakers
                k = (n // 6) & 0xFFFF
                ws.send(2, b"\x0b\x02" + bytes([0]) + struct.pack(">H", k) + bytes([3]) + b"abc"
                        + bytes([3]) + struct.pack(">H", k) + bytes([2]) + b"de")
                if n == 60:
                    ws.text({"voice": {"speaking": [3]}})
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


def probe(ws, args, state):
    """join_route.rs: {"p": n} -> {"p": n, "us"}; {"burst": n} -> binary pieces, {"burst_done": n}."""
    rec("probe_sockets")
    opened = time.monotonic()
    while True:
        op, data = ws.recv()
        m = json.loads(data)
        if "p" in m:
            rec("probe_checks", round(time.monotonic() - T0, 3))
            ws.text({"p": m["p"], "us": int(time.monotonic() * 1e6)})
        if "burst" in m:
            n = min(int(m["burst"]), 512 * 1024) if time.monotonic() - opened < 30 else 0
            rec("bursts", round(time.monotonic() - opened, 3))
            sent = 0
            while sent < n:
                k = min(16384, n - sent)
                ws.send(2, bytes(k))
                sent += k
            ws.text({"burst_done": n})


def bridge(ws, args, state):
    """join_net/bridge.rs on the guest's own engine: {"bridge": {v, room, fp}} -> "connecting",
    "open" (or "failed"), then the socket is the host's guest socket (here: this stand-in's)."""
    op, data = ws.recv()
    b = json.loads(data).get("bridge", {})
    rec("bridges", b)
    ws.text({"bridge": "connecting"})
    time.sleep(0.2)
    if b.get("v") == 2 and b.get("lobby"):
        # a room from Rooms: the bridge knocks on the host's lobby for the app and relays the answers
        if args.room_pw and not knock_secret(ws, args, "g1"):
            return
        ws.text({"knock": "wait"})
        time.sleep(0.3)
        ws.text({"bridge": "open", "path": "internet", "rtt_ms": 45})
        guest(ws, args, state, via="lobby")
        return
    if args.bridge_fail:
        ws.text({"bridge": "failed", "why": args.bridge_fail})
        return
    ws.text({"bridge": "open", "path": "internet", "rtt_ms": 45})
    guest(ws, args, state, via="bridge")


def app(ws, args, state):
    kind, hello = ws.recv()
    h = json.loads(hello)["hello"]
    rec("app_hellos", h)
    good = h.get("token") == "apptok" or h.get("grant") == "g" * 64
    if h.get("grant") == "g" * 64:
        ws.text({"paired": {"id": "a2", "token": "apptok"}})
    full = {"doc": {"rev": 1, "paired": True,
                    "engine": {"version": "13.0.0", "name": "Fake engine"},
                    "console": {"addr": "127.0.0.1", "name": "Test PS5"},
                    "session": {"state": "playing", "codec": "h265", "bitrate_kbps": 15000},
                    "together": {"open": True, "playing": True,
                                 "invite": {"code": "ABCDEF", "code_display": "ABC-DEF",
                                            "url": "http://192.168.1.2:19113/join#i=key", "works": "wifi",
                                            "expires_in_s": 1700},
                                 "pending": [{"id": 7, "name": "Sam", "client_kind": "ps5", "codec_warning": None, "came": "nearby", "tag": "4F2A", "own": False}],
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
helper_state = {"rp_on": False, "pin": "idle", "intent": "join",
                "grants": [{"engine_id": "e1" * 32, "name": "Living room PC", "kind": "desktop", "addr": "127.0.0.1",
                            "port": 19113, "app_ws": "/api/play/app", "grant": "g" * 64, "expires_s": 290}]}


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
    if (method, path) == ("GET", "/v1/version"):
        return answer(200, {"helper": "13.0.0", "api": 2})
    if path == "/v1/play/grants":
        g = helper_state.pop("grants", [])
        return answer(200, {"grants": g})
    if (method, path) == ("GET", "/v1/intent"):
        return answer(200, {"intent": helper_state.pop("intent", None)})
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
        offered = {"name": "Living room PC", "kind": "desktop", "version": "13.0.0", "addr": "192.168.1.20", "id": "e1" * 32,
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


hub_rooms = {}  # room -> (host ws, {gid: guest ws})


def hub(ws, args, state):
    """A stand-in for the signalling hub's /ws (protocol v1, ps5upload-hub PROTOCOL.md), enough for
    the app's waiting room: host, join, signal both ways, leave."""
    _, first = ws.recv()
    m = json.loads(first)
    if m.get("t") == "host":
        room = m["room"]
        rec("hub_hosts", {"room": room, "secret_len": len(m.get("secret", ""))})
        with lock:
            hub_rooms[room] = (ws, {})
        ws.text({"t": "hosted", "room": room})
        try:
            while True:
                _, data = ws.recv()
                m = json.loads(data)
                rec("hub_host_msgs", m)
                if m.get("t") == "signal":
                    g = hub_rooms[room][1].get(m.get("to"))
                    if g:
                        g.text({"t": "signal", "from": "host", "data": m["data"]})
                elif m.get("t") == "leave":
                    break
        finally:
            with lock:
                hub_rooms.pop(room, None)
        return
    if m.get("t") == "join":
        with lock:
            r = hub_rooms.get(m.get("room"))
        if not r:
            ws.text({"t": "error", "why": "no_room"})
            return
        gid = "g%d" % (len(r[1]) + 1)
        r[1][gid] = ws
        ws.text({"t": "joined", "gid": gid})
        r[0].text({"t": "guest", "gid": gid})
        while True:
            _, data = ws.recv()
            m = json.loads(data)
            if m.get("t") == "signal":
                r[0].text({"t": "signal", "from": gid, "data": m["data"]})
            elif m.get("t") == "leave":
                r[0].text({"t": "left", "gid": gid})
                return


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
        if path.startswith("/rooms") or path.startswith("/api/join/preview"):
            return http_rooms(conn, path)
        if path == "/api/join/probe" and args.no_probe:
            conn.sendall(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            return
        if path not in ("/api/join/ws", "/api/play/app", "/api/join/probe", "/api/join/bridge", "/ws") \
                or hdr.get("upgrade", "").lower() != "websocket":
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
        {"/api/join/ws": guest, "/api/join/probe": probe, "/api/join/bridge": bridge, "/ws": hub}.get(path, app)(
            ws, args, state)
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
            body = json.dumps({"name": "Fake engine", "port": port, "version": "13.0.0", "id": "fake1",
                               "together": 1}).encode()
            if len(body) <= len(data):
                s.sendto(body, peer)


def proxy(listen, target, delay_ms, hole_after, hole_until):
    """A way to the engine with its own round trip: each chunk waits delay_ms/2 each way; between
    hole_after and hole_until seconds (from the start) nothing goes through at all (a black hole:
    the sockets stay open)."""
    def pump(src, dst):
        q = []
        cv = threading.Condition()

        def writer():
            while True:
                with cv:
                    while not q:
                        cv.wait()
                    due, d = q[0]
                    wait = due - time.monotonic()
                    if wait > 0:
                        cv.wait(wait)
                        continue
                    q.pop(0)
                if d is None:
                    try:
                        dst.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass
                    return
                now = time.monotonic() - T0
                if hole_after and hole_after <= now < (hole_until or 1e9):
                    continue
                try:
                    dst.sendall(d)
                except OSError:
                    return
        threading.Thread(target=writer, daemon=True).start()
        while True:
            try:
                d = src.recv(65536)
            except OSError:
                d = b""
            with cv:
                q.append((time.monotonic() + delay_ms / 2000.0, d or None))
                cv.notify()
            if not d:
                return

    while True:
        c, _ = listen.accept()
        try:
            e = socket.create_connection(("127.0.0.1", target))
        except OSError:
            c.close()
            continue
        threading.Thread(target=pump, args=(c, e), daemon=True).start()
        threading.Thread(target=pump, args=(e, c), daemon=True).start()


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
    ap.add_argument("--bridge-fail", help="the stand-in bridge answers failed with this why")
    ap.add_argument("--key", help="the invite key the stand-in takes through a bridge (default: any)")
    ap.add_argument("--no-probe", action="store_true", help="an engine without the checks socket")
    ap.add_argument("--room-pw", help="knocks need this password (sealed to the room key)")
    ap.add_argument("--name-taken", help="a guest with this name is told it is taken")
    ap.add_argument("--rooms-429", type=int, default=0, help="the first N GET /rooms answer 429 Retry-After 1")
    ap.add_argument("--proxy", action="append", default=[],
                    help="DELAY_MS[:HOLE_AFTER_S[:HOLE_UNTIL_S]]: another way in, with that round trip")
    args = ap.parse_args()
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", args.port))
    srv.listen(8)
    port = srv.getsockname()[1]
    proxies = []
    for spec in args.proxy:
        parts = [float(x) for x in spec.split(":")]
        ls = socket.socket()
        ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        ls.bind(("127.0.0.1", 0))
        ls.listen(8)
        proxies.append(ls.getsockname()[1])
        threading.Thread(target=proxy, args=(ls, port, parts[0], parts[1] if len(parts) > 1 else 0,
                                              parts[2] if len(parts) > 2 else 0), daemon=True).start()
    if args.port_file:
        with open(args.port_file + ".ways", "w") as f:
            f.write(" ".join(str(x) for x in proxies))
        with open(args.port_file, "w") as f:
            f.write(str(port))
    if args.discovery:
        threading.Thread(target=discovery, args=(port,), daemon=True).start()
    rooms_429["left"] = args.rooms_429
    state = {}
    srv.settimeout(0.2)
    end = time.monotonic() + args.lifetime
    while time.monotonic() < end:
        try:
            c, _ = srv.accept()
        except socket.timeout:
            with lock:
                # whole or not at all: the test may stop this process at any moment
                with open(args.report + ".tmp", "w") as f:
                    json.dump(report, f)
                os.replace(args.report + ".tmp", args.report)
            continue
        threading.Thread(target=serve, args=(c, args, state), daemon=True).start()


if __name__ == "__main__":
    main()
