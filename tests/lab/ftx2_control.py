#!/usr/bin/env python3
import json
import socket
import struct
import sys
import hashlib

FTX2_MAGIC = int.from_bytes(b"FTX2", "little")
FTX2_VERSION = 1
FRAME_ERROR = 3
FRAME_HELLO = 1
FRAME_HELLO_ACK = 2
FRAME_TAKEOVER_REQUEST = 18
FRAME_TAKEOVER_ACK = 19
FRAME_BEGIN_TX = 10
FRAME_BEGIN_TX_ACK = 11
FRAME_QUERY_TX = 12
FRAME_QUERY_TX_ACK = 13
FRAME_ABORT_TX = 16
FRAME_ABORT_TX_ACK = 17
FRAME_STATUS = 20
FRAME_STATUS_ACK = 21
FRAME_SHUTDOWN = 22
FRAME_SHUTDOWN_ACK = 23
TX_META_LEN = 24


def recv_exact(sock: socket.socket, length: int) -> bytes:
    chunks = []
    got = 0
    while got < length:
        chunk = sock.recv(length - got)
        if not chunk:
            raise RuntimeError("connection closed")
        chunks.append(chunk)
        got += len(chunk)
    return b"".join(chunks)


def frame(frame_type: int, trace_id: int = 1, body: bytes = b"") -> bytes:
    return struct.pack("<IHHIQQ", FTX2_MAGIC, FTX2_VERSION, frame_type, 0, len(body), trace_id) + body


def tx_meta_body(command: str, body_text: bytes) -> bytes:
    tx_id = "lab-default-tx"
    kind = 0
    flags = 0
    metadata = body_text

    if body_text:
        try:
            parsed = json.loads(body_text.decode("utf-8"))
            tx_id = str(parsed.get("tx_id", tx_id))
            kind = int(parsed.get("kind_id", 0))
            flags = int(parsed.get("flags", 0))
            metadata = json.dumps(parsed, separators=(",", ":")).encode("utf-8")
        except json.JSONDecodeError:
            metadata = body_text

    if command == "query-tx" or command == "abort-tx":
        metadata = b""

    tx_id_bytes = hashlib.md5(tx_id.encode("utf-8")).digest()
    return tx_id_bytes + struct.pack("<II", kind, flags) + metadata


def main() -> int:
    if len(sys.argv) < 2 or sys.argv[1] not in {"hello", "status", "takeover", "shutdown", "begin-tx", "query-tx", "abort-tx"}:
        print("usage: ftx2_control.py [hello|status|takeover|shutdown|begin-tx|query-tx|abort-tx] [host] [port] [json_body]", file=sys.stderr)
        return 2

    command = sys.argv[1]
    host = sys.argv[2] if len(sys.argv) > 2 else "192.168.137.2"
    port = int(sys.argv[3]) if len(sys.argv) > 3 else 9113
    body = sys.argv[4].encode("utf-8") if len(sys.argv) > 4 else b""
    if command == "hello":
        frame_type = FRAME_HELLO
    elif command == "status":
        frame_type = FRAME_STATUS
    elif command == "begin-tx":
        frame_type = FRAME_BEGIN_TX
    elif command == "query-tx":
        frame_type = FRAME_QUERY_TX
    elif command == "abort-tx":
        frame_type = FRAME_ABORT_TX
    elif command == "takeover":
        frame_type = FRAME_TAKEOVER_REQUEST
    else:
        frame_type = FRAME_SHUTDOWN

    with socket.create_connection((host, port), timeout=2.0) as sock:
        if command in {"begin-tx", "query-tx", "abort-tx"}:
            body = tx_meta_body(command, body)
        sock.sendall(frame(frame_type, body=body))
        hdr = recv_exact(sock, 28)
        magic, version, response_type, flags, body_len, trace_id = struct.unpack("<IHHIQQ", hdr)
        body = recv_exact(sock, body_len) if body_len else b""

    if magic != FTX2_MAGIC:
        raise RuntimeError(f"bad magic: {magic:#x}")
    if version != FTX2_VERSION:
        raise RuntimeError(f"bad version: {version}")

    text = body.decode("utf-8", errors="replace")
    if response_type == FRAME_ERROR:
        print(text, file=sys.stderr)
        return 1
    if response_type == FRAME_HELLO_ACK:
        try:
            print(json.dumps(json.loads(text), indent=2, sort_keys=True))
        except json.JSONDecodeError:
            print(text)
        return 0
    if response_type == FRAME_STATUS_ACK:
        try:
            print(json.dumps(json.loads(text), indent=2, sort_keys=True))
        except json.JSONDecodeError:
            print(text)
        return 0
    if response_type in {FRAME_BEGIN_TX_ACK, FRAME_QUERY_TX_ACK, FRAME_ABORT_TX_ACK}:
        try:
            print(json.dumps(json.loads(text), indent=2, sort_keys=True))
        except json.JSONDecodeError:
            print(text)
        return 0
    if response_type in {FRAME_TAKEOVER_ACK, FRAME_SHUTDOWN_ACK}:
        print(text or "{}")
        return 0

    print(f"unexpected response_type={response_type} body={text}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
