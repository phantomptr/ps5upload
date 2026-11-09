# Play Together hub protocol, v1

The contract between the hub (`ps5upload-hub`), the host's engine and the guest page
(`https://ps5upload.phantomptr.com`). The hub only introduces the two sides: it forwards
signalling messages (SDP offers and answers, ICE candidates) that it never reads. Media goes peer
to peer over WebRTC; there is no relay.

Endpoint: `wss://hub.phantomptr.com/ws`.

## Transport

- WebSocket, **JSON text frames only**, one message per frame, each at most **16 KiB**
  (16384 bytes of UTF-8). A binary frame is an error.
- `Origin`: a browser must come from an allowed origin (by default
  `https://ps5upload.phantomptr.com` and the PS5Upload app's own pages: `tauri://localhost`,
  `http://tauri.localhost`, `https://tauri.localhost`); anything else gets HTTP 403 before the upgrade. A client
  that sends no `Origin` (the engine) is allowed.
- Keepalive: the hub sends a WebSocket ping every 20 s to a peer it has not heard from in that
  time (a peer that pings the hub itself, like the engine, gets none). A peer that sends nothing
  at all (no message, no pong) for 60 s is closed with code 1001.
- A new socket must send `host` or `join` within 10 s, or it is closed (4000, reason
  `timeout`).
- Connections: at most 32 open at once from one client and 2,000 in all; past either the upgrade
  is refused with HTTP 429 (per client) or 503 (the hub is full). Browsers and WebSocket libraries answer
  pings on their own; nothing to implement beyond reading the socket.
- `v` is the protocol version. It is required (`1`) on `host` and `join`, optional on the rest,
  and when present it must be `1`.

## Identifiers

| Name | Made by | Form |
|------|---------|------|
| `room` | the host (engine) | 22 base64url characters (`A-Z a-z 0-9 - _`): 16 random bytes, no padding |
| `secret` | the host (engine) | 43 base64url characters: 32 random bytes, no padding |
| `gid` | the hub | 12 base64url characters (9 random bytes), unique within the room |

The invite link carries the `room` only. The `secret` never leaves the host; the hub keeps only
its SHA-256 and uses it to let the same host take the room back after a reconnect.

## Messages

### Host

```
→ {"v":1,"t":"host","room":"<room>","secret":"<secret>"}
← {"t":"hosted","room":"<room>"}
```

- New room: created, owned by this socket.
- Existing room, same secret (**reclaim**): this socket becomes the host. The previous host
  socket, if still open, is closed with code **4001** ("replaced"). After `hosted`, the hub
  sends one `{"t":"guest","gid":...}` for every guest still in the room, so the new socket can
  pick their negotiation back up.
- Existing room, different secret: `{"t":"error","why":"room_taken"}`, close.
- A room whose host is gone waits **2 minutes** for a reclaim, then is removed (its id is free
  again, for any secret).

Host receives:

```
← {"t":"guest","gid":"<gid>"}                       a guest joined
← {"t":"signal","from":"<gid>","data":{...}}        from that guest
← {"t":"left","gid":"<gid>"}                        that guest left or disconnected
```

### Guest

```
→ {"v":1,"t":"join","room":"<room>"}
← {"t":"joined","gid":"<gid>"}
← {"t":"signal","from":"host","data":{...}}
← {"t":"closed"}                                    the host left, closed the room, or removed this guest
```

- No such room, or its host is away: `{"t":"error","why":"no_room"}`, close.
- Room has 8 guests: `{"t":"error","why":"room_full"}`, close.
- After `closed` the hub closes the socket (code 1000).

### Signal (both sides)

```
→ {"t":"signal","to":"host","data":{...}}           guest → its host
→ {"t":"signal","to":"<gid>","data":{...}}          host → one of its guests
```

Forwarded as `{"t":"signal","from":"host"|"<gid>","data":<data unchanged>}`.

- `data` must be a JSON **object** of at most **12 KiB** serialized, else `bad_data` and close.
  Its content is up to the engine and the page; the hub does not look inside. Suggested shapes:
  `{"sdp":{"type":"offer","sdp":"..."}}`, `{"ice":{"candidate":"...","sdpMid":"0","sdpMLineIndex":0}}`.
- A guest may only address `"host"`; a host only a `gid`. Anything else: `bad_target`, close.
- A host addressing a `gid` that has gone gets `{"t":"error","why":"no_peer"}`; this one error
  does **not** close the socket (the guest may have left a moment ago).

### Leave

```
→ {"t":"leave"}                        guest: leave the room. Host: close the room for good.
→ {"t":"leave","gid":"<gid>"}          host only: remove that guest.
```

- Guest leaves: the host gets `left`; the guest's socket is closed (1000).
- Host closes the room: every guest gets `closed`; the room is removed at once (no reclaim
  window); the host's socket is closed (1000).
- Host removes a guest: that guest gets `closed`. The host does **not** get `left` for it.
- Host socket drops without `leave`: every guest gets `closed`, the room waits 2 minutes for a
  reclaim.
- Guest socket drops: the host gets `left`.

## Errors

`{"t":"error","why":"<kind>"}`. Except `no_peer`, every error is followed by a close with code
**4000** and the kind as the close reason.

| `why` | When |
|-------|------|
| `bad_json` | not JSON, not an object, `t` missing or not a string, a field of the wrong type |
| `bad_version` | `v` missing on `host`/`join`, or not `1` |
| `unknown_type` | `t` is not `host`, `join`, `signal`, `leave` |
| `too_large` | frame over 16 KiB (frames over 32 KiB are cut by the WebSocket layer, code 1009) |
| `bad_frame` | a binary frame |
| `bad_room` | `room` not 22 base64url characters |
| `bad_secret` | `secret` not 43 base64url characters |
| `bad_state` | `host`/`join` on a socket that already has a role; `signal`/`leave` before one |
| `bad_target` | a signal `to` the sender may not address |
| `bad_data` | signal `data` missing, not an object, or over 12 KiB |
| `room_taken` | `host` with the wrong secret |
| `no_room` | `join` to a room that does not exist or has no host right now |
| `room_full` | `join` to a room with 8 guests |
| `rate_limited` | over a per-minute limit (below) |
| `server_full` | `host` when the hub has 5,000 rooms |
| `too_many_rooms` | `host` from a client that already has 8 rooms |
| `no_peer` | signal to a guest that has gone (not fatal) |

## Limits (defaults)

| Limit | Default |
|-------|---------|
| guests per room | 8 |
| signals per guest | 20 per minute |
| signals per host (all its guests together) | 200 per minute |
| `join` attempts per client | 30 per minute (found or not) |
| rooms on the hub | 5,000 |
| rooms per client | 8 (a room counts toward its creator until removed) |
| open connections per client | 32 (HTTP 429 past it) |
| open connections on the hub | 2,000 (HTTP 503 past it) |
| time to send `host` or `join` | 10 s |
| room without host | removed after 2 minutes |
| STUN requests per client (if STUN is on) | 60 per minute (over that: no answer) |

A *client* is an IPv4 address or an IPv6 /64. Behind a reverse proxy, the client address comes
from the proxy's header (last entry) only when the proxy is configured as trusted.

## Close codes

| Code | Meaning |
|------|---------|
| 1000 | normal end (`leave`, `closed`) |
| 1001 | the hub is shutting down, or the peer was silent for 60 s |
| 1009 | frame over the hard cap (32 KiB) |
| 4000 | protocol error; the reason is the error kind |
| 4001 | replaced: the same room was reclaimed from another socket |

On 1001 from a shutdown a host should reconnect (with backoff) and send `host` again with the
same room and secret; its guests must `join` again. The hub keeps rooms in memory only, so
after a restart the room is new to it: the `host` with the same room and secret opens it again,
and a guest that comes back before its host gets `no_room` for a moment (the guest page tries
again for up to 20 s after a 1001).

## STUN

Optional and off by default; clients use public STUN servers (`stun:stun.cloudflare.com:3478`,
`stun:stun.l.google.com:19302`). When the hub runs it (`--stun` / `HUB_STUN_LISTEN`), it answers
RFC 5389 Binding requests with a success response carrying XOR-MAPPED-ADDRESS, on UDP, IPv4 and
IPv6. It ignores every other message and does no TURN.

## Other endpoints

- `GET /health`: `ok`.
- `GET /metrics`: Prometheus text, counts only (rooms, guests, connections, hosts, reclaims,
  joins, signals, expired rooms, STUN requests, errors by kind). Kept private: localhost only
  behind Caddy, or a separate private port (`HUB_METRICS_LISTEN`) on Fly.

## Privacy

The hub keeps everything in memory and nothing on disk. Its logs and metrics never hold a
client address, a room id, a gid, a secret or a payload: only event names, counts and error
kinds.

## A whole exchange

```
host  → {"v":1,"t":"host","room":"R","secret":"S"}
host  ← {"t":"hosted","room":"R"}
guest → {"v":1,"t":"join","room":"R"}
guest ← {"t":"joined","gid":"G"}
host  ← {"t":"guest","gid":"G"}
host  → {"t":"signal","to":"G","data":{"sdp":{"type":"offer","sdp":"..."}}}
guest ← {"t":"signal","from":"host","data":{"sdp":{"type":"offer","sdp":"..."}}}
guest → {"t":"signal","to":"host","data":{"sdp":{"type":"answer","sdp":"..."}}}
host  ← {"t":"signal","from":"G","data":{"sdp":{"type":"answer","sdp":"..."}}}
        ... ICE candidates both ways the same way ...
guest → {"t":"leave"}
host  ← {"t":"left","gid":"G"}
```

Who sends the offer is up to the engine and the page; the hub does not care. Once the WebRTC
connection is up, both sides may stay connected to the hub (for renegotiation) or the guest may
leave; the media does not depend on the hub.
