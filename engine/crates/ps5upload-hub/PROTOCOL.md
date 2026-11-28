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
  that sends no `Origin` (the engine) is allowed. The same hub built into an engine
  (`PS5UPLOAD_HUB_LISTEN`, SELF_HOSTING.md) also allows its own public origin.
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
| `room` | the host (engine) | 22 base64url characters (`A-Z a-z 0-9 - _`), made from the secret: the first 21 characters of base64url(SHA-256(`ps5upload-hub room` + one zero byte + `secret`)), then `h`. Older apps: 16 random bytes, no padding (such an id never ends in `h`) |
| `secret` | the host (engine) | 43 base64url characters: 32 random bytes, no padding |
| `gid` | the hub | 12 base64url characters (9 random bytes), unique within the room |

The invite link carries the `room` only. The `secret` never leaves the host; the hub keeps only
its SHA-256 and uses it to let the same host take the room back after a reconnect.

A room id that ends in `h` is **bound** to its secret: the hub opens it only for the secret it
was made from, so nobody else with the link can open it, also on a hub that has just restarted
and has no rooms. The id is the proof; the hub stores nothing for it. An id of any other form (an
older app's) opens for any secret while it is free, as before: that host's room can be taken by
a link holder after a hub restart until the app is updated. Hosts and hubs of either age work
with each other.

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
- A bound room id (ending in `h`) with a secret it was not made from: `room_taken`, close,
  whether the room is open or not.
- A room whose host is gone waits **2 minutes** for a reclaim, then is removed (its id is free
  again: for any secret, or a bound id for its own only).

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
- A guest leaves once its connection to the host is up. One still in the room **3 minutes**
  after it joined is removed: `{"t":"error","why":"timeout"}`, close (4000); the host gets
  `left` for it. (The guest page holds a place it took early for a minute at most and gives ICE
  15 s, so no real guest stays that long; idle sockets cannot keep a room full.)

### Signal (both sides)

```
→ {"t":"signal","to":"host","data":{...}}           guest → its host
→ {"t":"signal","to":"<gid>","data":{...}}          host → one of its guests
```

Forwarded as `{"t":"signal","from":"host"|"<gid>","data":<data unchanged>}`.

- `data` must be a JSON **object** of at most **12 KiB** as sent (its text, spaces included), else
  `bad_data` and close. It is forwarded byte for byte.
  Its content is up to the engine and the page; the hub does not look inside. Suggested shapes:
  `{"sdp":{"type":"offer","sdp":"..."}}`, `{"ice":{"candidate":"...","sdpMid":"0","sdpMLineIndex":0}}`.
- A guest may only address `"host"`; a host only a `gid`. Anything else: `bad_target`, close.
- A host addressing a `gid` that has gone gets `{"t":"error","why":"no_peer"}`; this one error
  does **not** close the socket (the guest may have left a moment ago).
- A host over its signal limit is not closed: the signals past it are dropped (not forwarded,
  no error) until the minute is over. Guests make a host send (it answers each offer), so a
  limit that closed the host let anyone with the link cut it off.
- A guest over its own limit, or over its client's in the room (all that client's guests
  there together, those that left included), gets `rate_limited`, close. Joining again does not
  refill a client's: while it is used up, that client's `join` to the room gets `rate_limited`.

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
| `room_taken` | `host` with the wrong secret (for a bound room id: one it was not made from) |
| `no_room` | `join` to a room that does not exist or has no host right now |
| `room_full` | `join` to a room with 8 guests |
| `rate_limited` | over a per-minute limit (below; a host's signals past its limit are dropped instead) |
| `server_full` | `host` when the hub has 5,000 rooms |
| `too_many_rooms` | `host` from a client that already has 8 rooms |
| `no_peer` | signal to a guest that has gone (not fatal) |
| `timeout` | a guest still in its room 3 minutes after it joined |

## Limits (defaults)

| Limit | Default |
|-------|---------|
| guests per room | 8 |
| signals per guest | 60 per minute (was 20: one try of a browser that trickles each candidate went past it) |
| signals per guest client in one room (its guests together, joins again included) | 300 per minute (was 60) |
| signals per host (all its guests together) | 200 per minute (past it: dropped) |
| `join` attempts per client | 30 per minute (found or not) |
| rooms on the hub | 5,000 |
| rooms per client | 8 (a room counts toward its creator until removed) |
| open connections per client | 32 (HTTP 429 past it) |
| open connections per IPv6 /48 | 256 (HTTP 429 past it) |
| open connections on the hub | 2,000 (HTTP 503 past it) |
| time to send `host` or `join` | 10 s |
| room without host | removed after 2 minutes |
| guest in a room | removed 3 minutes after it joined |
| STUN requests per client (if STUN is on) | 60 per minute (over that: no answer) |

A *client* is an IPv4 address or an IPv6 /64. Behind a reverse proxy, the client address comes
from the proxy's header (last entry) only when the proxy is configured as trusted.

A guest's signals are its offer and each ICE candidate, one signal each, so a client should not
send the candidates the host cannot use: the engine takes UDP candidates with an address and
drops `.local` (mDNS) and TCP ones. The guest pages leave those out (Firefox on one network: 11
signals a try instead of 21).

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
- `/ws/` is `/ws`. A request to `/ws` that is not a WebSocket upgrade gets 400, 405 or 426.
- `GET /robots.txt`: `User-agent: *` / `Disallow: /`.
- Anything else: 404. The hub's name is public, so scanners and crawlers ask for `/`, `/.env`
  and the like all day; these are counted by a fixed class of the path (`root`, `favicon`,
  `robots`, `well_known`, `ws_not_websocket`, `other`) and never logged.
- `GET /metrics`: Prometheus text, counts only (rooms, guests, connections, rooms opened,
  reclaimed, closed and expired, joins, signals, dropped host signals, expired guests, STUN
  requests, errors by kind, refusals by the limit that fired, stray HTTP requests by class, guests
  joined from an app's own client with no Origin (`hub_joins_app_total`: the bridge PS5Upload Play
  joins through), and guest sockets closed with their open time in all
  (`hub_guests_left_total`, `hub_guest_connected_ms_total`: the load per join)).
  Kept private: localhost only behind Caddy, or a separate private port (`HUB_METRICS_LISTEN`)
  on Fly.

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
connection is up the guest leaves the hub (it would be removed 3 minutes after it joined anyway);
the host stays for new guests. The media does not depend on the hub.

## Rooms (Play Rooms, additive to v1)

Everything here is additive: `v` stays 1, invite rooms behave exactly as above, and an engine or
page that never sends the new fields sees no change. A room's own protocol number (`proto`, 2 for
13.0) travels inside the card and the list; it is not the hub's `v`.

### Lobby rooms

```
→ {"v":1,"t":"host","room":"<bound room>","secret":"<secret>","lobby":true}
← {"t":"hosted","room":"<room>","lobby":true,"caps":["ads","cid","knock","preview","status"],
   "ad_pow":{"n":"<nonce>","bits":18}}
```

- `room` must be a bound id (`room_for_secret`, ending in `h`), else `bad_room`. Engines make it
  from a `lobby_secret` kept apart from the invite secret, so a card never carries an invite room.
- Up to 16 guests at once (invite rooms: 8). The 3-minute guest timeout stays.
- The host learns each guest's client id in this room: `← {"t":"guest","gid":"…","cid":"<8 base64url>"}`
  (a keyed hash of the guest's IPv4 or IPv6 /64 and the room, new key on every hub start; other
  rooms see another `cid`; never the address). Use it for password lockouts and room blocks.
- `HUB_KNOCKS=off` (or `knocks off` at run time): `{"t":"error","why":"no_lobby"}` and a normal
  close (1000). Fall back to invite rooms only.

### Cards (host socket of a lobby room)

```
→ {"t":"ad","ad":{…},"pow":"<answer>"}     create: needs the proof of work
→ {"t":"ad","ad":{…}}                        update (renews too)
→ {"t":"ad"}                                 renew only
← {"t":"ad_ok","until":<unix seconds>}       listed or known until (12 h after create)
← {"t":"ad_refused","why":"<why>"}           the socket stays open
→ {"t":"unad"}                               withdraw now
← {"t":"ad_gone","why":"<why>","ad_pow":{"n":"<new nonce>","bits":18}}
```

- Proof of work: find any `answer` (at most 64 characters) such that
  SHA-256(`ps5upload-hub ad` + one zero byte + `n` + `answer`) starts with `bits` zero bits
  (18 by default: about 260,000 hashes, well under a second). A nonce is spent by its create; the
  next one comes with `ad_gone`.
- One card per lobby room and socket. It dies with the socket (`closed`), on `unad`, when the
  room closes, when not renewed for 150 s (`stale`: renew at least every 2 minutes; any `ad`
  renews), and 12 h after create (`expired`: create again with the new nonce).
- `ad_gone` `why`: `unad`, `stale`, `expired`, `off` (rooms switched off), `blocked`, `old`
  (below `min_proto`).
- `ad_refused` `why`: `pow`, `name` (room or host name), `game`, `bad` (another field, a card over
  1 KiB, or a renewal with no card), `old`, `blocked`, `limit` (2 cards per client, 10 creates per
  client an hour), `busy` (500 cards on the hub, or cut-back mode: new cards wait, none are
  evicted), `rate` (more than about one update every 10 s; the card stays), `off`, `no_lobby`
  (an invite room). A refused update keeps the card as it was, except `old` and `blocked`.
- The card (every field checked; unknown fields ignored, the whole at most 1 KiB):

```json
{"listed": true, "locked": false, "name": "Couch chaos", "host": "Sam", "tag": "4F2A",
 "game": {"title": "It Takes Two", "id": "PPSA01234"},
 "proto": 2, "ver": "13.0.0", "caps": ["web", "ps5app", "preview", "chat", "voice"],
 "slots": [1, 4], "pw": true, "lang": "en", "region": "eu-west", "pk": "<base64url>"}
```

| Field | Rule |
|---|---|
| `listed` | true: Public (in the list); false: Friends (only `/rooms/status` answers it). Default true |
| `locked` | true: hidden from the list; `/rooms/status` says `locked`. Default false |
| `name` | 3–32 characters after cleaning (see Names) |
| `host` | the host's display name, 1–24 characters, same rules |
| `tag` | 4 hex digits (the host device's short tag), shown uppercase |
| `game` | optional; `id` `[A-Z]{4}[0-9]{5}`, `title` 1–64 printable characters, no links or markup |
| `proto` | 1–999; below the hub's minimum (2): `old` |
| `ver` | `N.N.N` |
| `caps` | up to 16 strings `[a-z0-9_]{1,16}` |
| `slots` | `[in, max]`, max 1–8, in ≤ max (full rooms stay listed) |
| `pw` | a password is set (the password itself never reaches the hub) |
| `lang` | one of the 21 app languages (`en`, `pt-BR`, `zh-CN` …) |
| `region` | one of `na-west na-east central-am south-am eu-west eu-east middle-east africa asia-south asia-east asia-se oceania`; `"auto"` or absent: from the connection's country only; `"hidden"`: none shown |
| `pk` | the room key (P-256 public key, base64url, 40–100 characters), for sealing knock secrets |

Region shown: the hub looks up the host connection's country once at create (when it has a
country table), keeps only the coarse region, and drops the address at once; the card's own
region refines it within the same continent (a US host in Pacific time shows `na-west`). A VPN in
another continent keeps the country's region: that is where the traffic goes.

Names: invisible characters (zero-width, bidirectional overrides, variation selectors) are
dropped and spaces collapsed; then only letters and digits of any script, spaces and
`- _ . , ! ? ' ’ & ( ) # + : * ~` are allowed (no `@`, `/`, emoji or symbols); no links
(`http`, `www`, `word.tld`, chat services), no invite-code-like tokens (`K7P2Q9`) or long digit
runs, no repeated-character or punctuation spam, a small multilingual word list, and the
reserved words (PS5Upload, phantomptr, official, admin, support, moderator, staff).

### Previews (host socket)

```
← {"t":"preview","pid":"<12 base64url>"}
→ {"t":"preview","pid":"<same>","data":{…}}      within 3 s, at most 2 KiB
```

The answer goes to everyone who asked for this room within 10 s. One that holds anything shaped
like an IP address is refused (viewers get 502). Answer only with what the host chose to show.

### Knocks (lobby rooms)

A lobby guest may send only knock signals until its host lets it in:

- `data` `{"knock":{…}}` starts a knock (forwarded as usual); `{"knock":"sealed",…}` follows a
  challenge (two at most per knock). The hub never reads inside; before approval `data` is at
  most 2 KiB (larger: `bad_data`, close).
- The host lets the guest in with `{"knock":"ok"}` or any signal that is not a knock answer (an
  offer). `{"knock":"no",…}` ends the knock; `wait` and `challenge` keep it open.
- Answers to the guest, none of which closes the socket: `{"t":"error","why":"not_approved"}`
  (a non-knock signal before approval, dropped), `knock_pending` (one open knock per guest),
  `bad_knock`, `knocks_off`, and `{"t":"error","why":"rate_limited","retry_s":N}`: more than 5
  knocks from one client in one room (refilled one per 2 minutes), or more than 10 from one
  client across rooms (one a minute). Wait `retry_s`, then knock again.

### HTTP

CORS: a browser may call these from an allowed origin (`https://ps5upload.phantomptr.com`, the
app's own pages, and a built-in hub's own origin); the answer then carries
`access-control-allow-origin` for that origin and `vary: origin`. Any other `Origin`: 403. No
`Origin` (an app's own client): answered, no CORS headers. No cookies.

- `GET /rooms?sort=&game=&free=1&nopw=1&proto=&lang=&region=&near=1&me=&page=`

  ```json
  {"you": {"country": "DE", "region": "eu-west"},
   "proto": {"min": 2, "latest": 2}, "app": {"latest": "13.0.0"},
   "rooms": [{"room": "<lobby id>", "name": "Couch chaos", "host": "Sam", "tag": "4F2A",
              "game": {"title": "It Takes Two", "id": "PPSA01234"}, "proto": 2, "ver": "13.0.0",
              "caps": ["web"], "slots": [1, 4], "pw": true, "lang": "en", "region": "eu-west",
              "est_ms": 30, "age_s": 120}],
   "next": 8}
  ```

  - `sort`: `best` (default: has space, then lowest estimate, then works with `proto`, then
    newest), `latency`, `game` (title A–Z), `players` (most in), `space` (most free seats),
    `newest`. Filters: `game` (a title id, exact; or text, part of the title, any case), `free`,
    `nopw`, `proto` (the viewer's: rooms within 2 protocols), `lang`, `region`, `near` (the
    viewer's region or a neighbour). `me`: the viewer's own (time-zone) region, refining its
    country's. Unknown keys are ignored. `page`: the `next` of the previous page.
  - A page is at most 12 rooms and about 2 KiB (6 rooms in cut-back mode, with `"cutback":true`);
    at most 480 rooms deep. `you` holds the viewer's country and region when known (never its
    address); `est_ms` only when both regions are known; `region` only when the host shows one.
  - Cached for 5 s per query shape and viewer region (30 s in cut-back mode), shared by everyone.
  - Not listed: Friends cards, locked rooms, cards below `min_proto`, blocked tags.
  - Switched off: `{"off":true,"proto":{…},"rooms":[],"next":null}` (200).
- `GET /rooms/status?r=<id>,<id>…` (at most 16 ids; more are ignored): only the ids with a card
  (listed or not), never addresses:
  `{"every_s":60,"rooms":{"<id>":{"listed":false,"locked":false,"slots":[1,4],"pw":true,"proto":2,"game":{…},"pk":"…"}}}`.
  `every_s` is how often to ask again (300 in cut-back mode). The room id is the capability: the
  host hands its lobby id only to friends (with their friend pass); ids are unguessable.
- `GET /rooms/<id>/preview`: 200 with the host's preview; 404 `{"why":"no_room"}` (no card);
  503 `{"why":"off"}` (previews off or cut-back mode: "Details need a direct connection");
  504 `{"why":"no_answer"}` (the host did not answer in 3 s; shared for 10 s); 502 (the answer was
  refused). At most one request a room every 10 s reaches its host.

### Limits that leave people alone

Per client (IPv4 address or IPv6 /64), as token buckets: a burst well above what a person does,
then a steady refill. Past one: HTTP 429 `{"why":"slow_down","retry_s":N}` with `Retry-After`
(apps wait that long silently and try again). Only a flood that keeps going (30 s or more of
refusals) gets a temporary block: 2 minutes, doubling on repeats up to 30, forgotten after a clean
hour; a blocked client gets an empty 429 and the connection closed.

| Request | Burst | Then |
|---|---|---|
| `GET /rooms` | 30 | 1 a second |
| `GET /rooms/status` | 20 | 1 every 3 s |
| `GET /rooms/<id>/preview` | 10 | 1 every 6 s |
| knocks, all rooms | 10 | 1 a minute |
| knocks, one room | 5 | 1 every 2 minutes |
| WebSocket upgrades | 120 | 2 a second (past it: 429 before the upgrade) |
| messages on one WebSocket | 100 | 30 a second (past it: `rate_limited`, close) |
| card updates | 6 | 1 every 10 s |
| card creates | 10 | 10 an hour |

### Running it

- Kill switches (env at start, and at run time): `HUB_ROOMS` (cards, list, status),
  `HUB_PREVIEW`, `HUB_KNOCKS`, `HUB_MIN_PROTO`, `HUB_LATEST_PROTO`, `HUB_APP_LATEST`; blocklist
  `HUB_BLOCKLIST` (`tag 4F2A; ip 203.0.113.0/24; word …`). At run time, without a restart: the
  same lines in `HUB_CONTROL_FILE` (re-read when it changes) or `POST /admin/control` on the
  private metrics listener with `authorization: Bearer <HUB_ADMIN_TOKEN>` (deploy/FLY.md).
- Cut-back mode (automatic past 200 MiB resident or a projected 100 GB a month out; also
  `cutback on|off|auto`): list cache 30 s, 6 rooms a page, previews through the hub off, friends
  lookups every 5 minutes, new cards refused (existing ones stay). Past 220 MiB new sockets get
  503. One log line on each change.
- Budgets (`config.rs`): `HUB_MAX_ADS` 500, `HUB_MAX_ADS_PER_IP` 2, `HUB_AD_CREATES_PER_HOUR` 10,
  `HUB_AD_POW_BITS` 18, `HUB_AD_MAX_SECS` 43200, `HUB_AD_RENEW_SECS` 150, `HUB_LOBBY_GUESTS` 16,
  `HUB_LIST_CACHE_SECS` 5, `HUB_LIST_PAGE_ITEMS` 12, `HUB_LIST_PAGE_BYTES` 2048,
  `HUB_MEM_CUTBACK_MIB` 200, `HUB_MEM_REFUSE_MIB` 220, `HUB_OUT_BUDGET_GB` 100, `HUB_GEO_FILE`.
- Metrics (counts only): `hub_lobby_rooms`, `hub_ads_listed`, `hub_ads_friends`,
  `hub_ads_total{kind}`, `hub_ads_removed_total{why}`, `hub_ads_refused_total{why}`,
  `hub_rooms_list_total{result}`, `hub_rooms_status_total{result}`, `hub_previews_total{result}`,
  `hub_knocks_total{result}`, `hub_out_bytes_total{class}`, `hub_out_projected_bytes_month`,
  `hub_throttled_total{class}`, `hub_clients_blocked`, `hub_temp_blocks_total`, `hub_cutback`,
  `hub_resident_bytes`, `hub_rooms_list_cache_entries`, `hub_geo_ranges`, `hub_blocklist_tags`,
  `hub_blocklist_ips`.
- Privacy: no response holds an address (tests check every one); cards hold only the fields above;
  logs hold event names and counts (`control`, `cutback`, `geo_file_unreadable`), never a name,
  tag, room id or address. Client keys and countries live in memory only.
