#!/usr/bin/env python3
# PS5Upload Play spike - background-test peer (usage: echo-server.py LOGFILE).
# SPDX-License-Identifier: GPL-3.0-or-later
# Listens on 19271; for each client sends "ping N" once a second and logs the echo or the failure.
import socket, sys, time, threading

out = open(sys.argv[1], "a", buffering=1)


def log(msg):
    out.write(f"{time.strftime('%H:%M:%S')} {msg}\n")


def serve(conn, peer):
    log(f"client {peer} connected")
    conn.settimeout(3)
    n = 0
    while True:
        n += 1
        t = time.time()
        try:
            conn.sendall(f"ping {n}\n".encode())
            data = conn.recv(64)
            if not data:
                log(f"ping {n}: closed by console")
                return
            log(f"ping {n}: ok {1000 * (time.time() - t):.0f} ms")
        except OSError as e:
            log(f"ping {n}: {e} after {1000 * (time.time() - t):.0f} ms")
            if not isinstance(e, socket.timeout):
                return
        time.sleep(max(0, 1 - (time.time() - t)))


s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", 19271))
s.listen(4)
log("listening")
while True:
    c, a = s.accept()
    threading.Thread(target=serve, args=(c, a), daemon=True).start()
