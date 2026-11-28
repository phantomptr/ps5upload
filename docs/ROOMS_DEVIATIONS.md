# Play Rooms (13.0.0): deviations from the designs, and the helper API

## Agent E: helper (payload/) and Minion (client/src/console)

### Helper API for the engine (AVA1 management methods)

All JSON in and out (MgmtText). Full rows: `protocol/ps5upload-ava1/MGMT_METHODS.md`; constants
in `engine/crates/ps5upload-core/src/mgmt.rs` (`m::PLAY_ROOM`, `m::PLAY_GRANT`, `m::PADS_LIST`).
An older helper answers unknown method for all three.

| Method | Body | Answer |
|---|---|---|
| 191 `vpad.hb` (changed) | `{slot, token, live?}` | `{ok, neutral, has_user: true\|false\|null, user, made_for, seen_ms}` |
| 194 `vpad.status` (changed) | `{}` | each open slot also has `has_user`, `seen_user`, `seen_ms` (-1: none yet), `seen_code` |
| 203 `play.room` (new) | `{room: R\|null, ttl_s?}` | `{ok, rooms}`; refused `{ok:false, err: bad_request\|rooms_full}` |
| 204 `play.grant` (new) | `{grant: "<64 hex>"\|null, ttl_s?}` | `{ok, expires_s}` |
| 205 `pads.list` (new) | `{}` | `{ok, max: 4, guests: {open, slots: [{slot, open, has_user, user, made_for}]}, users: [..], free}` |

- **user** = the console user the guest pad is seated in now (`sceMbusGetDeviceInfo_` owner, read
  in the vinput companion through its new `OWNERS` frame about once a second while any pad is
  open); **made_for** = the user the pad was made for (the user in front unless a bind chose one).
  **Measured on the Pro (13.60, 2026-11-27): a pad just made reads back `made_for` as its owner at
  once, before anyone answered "Who's using this controller?".** So **has_user** is `true` when the
  owner is someone else than `made_for` (a pick: a Quick Play guest or another profile) or a bound
  pad sits in its bound user; `false` while the owner is still `made_for` (nobody picked yet, OR
  the guest picked that same user: the helper cannot tell these apart); `null` when not known (no
  read yet, companion not up, read failed). `seen_ms` is the reading's age. The companion is kept
  up while a guest pad is open (it is started if needed). Not yet seen on hardware: the owner
  changing after a real pick (nobody was at the TV to pick).
- **play.room** `R` (every field optional except `name`; unknown fields ignored; strings bounded and
  cleaned): `name`, `visibility` (`private|friends|public`), `locked`, `game: {name, title_id}`,
  `size` (1-8), `players: [{name, ping_ms, seat, bars (0-4, the rooms design's 6.1 tier), via}]`
  (8 listed at most), `playing`, `asks`, `chat`, `voice`, `hosted_from` (default: the name this
  console lists the calling device under). Replaced whole each call, kept `ttl_s` (10-600, default
  90): send it on every change and at least every `ttl_s / 3`; `{room: null}` on close. Keyed by
  the caller's AVA1 key (= the engine id); up to 4 engines.
- **play.grant**: one per engine, a new one replaces the old, `null` revokes; `ttl_s` 30-600,
  default 300; memory only; handed out once to the app (below), never shown in Minion or
  `/v1/status`. The engine must accept it only from a PS5Upload Play hello on this console and for
  its own engine id.
- **pads.list** counts the guest places (Players 2-4) only. The console's own controllers (the
  DualSense, Remote Play's pad) are not readable from the helper yet (no device list call found),
  so `free` = guest slots not open.

### Helper API for PS5Upload Play (127.0.0.1:9181, `payload/include/app_api.h`)

- `GET /v1/version` → `{"helper","api":2}` (2 since 13.0.0).
- `GET /v1/status` → adds `helper.api` and `room` (the newest room object, or null).
- `GET /v1/room` → `{"room": V|null, "rooms": [V..]}`; V = the room fields above plus `engine_id`
  (64 hex), `hosted_from`, `kind`, `online`, `age_s`; players carry `ping_ms`, `seat`, `bars`, `via`.
- `GET` or `POST /v1/play/grants` → `{"grants":[{"engine_id","name","kind","addr","port","app_ws",
  "grant","expires_s"}]}`; every grant is removed as it is handed out (one time); 20 reads a minute,
  then `429 slow_down`.
- `GET /v1/intent` → `{"intent":"join"|"host"|"rooms"|null}`: where Minion asked the app to open
  ("Join a friend" = `join`, the room card's Open = `host`); taken once, a minute at most. Read it
  right after start.
- `GET /v1/mesh` engine entries now carry `"id"` (the engine id, its AVA1 public key in hex).
- Every route: at most 120 requests in a burst, 20 a second sustained (`429 slow_down` with
  `Retry-After`).

### Minion (helper web API, POST like every route)

- `/api/room` → the `/v1/room` answer (read only).
- `/api/play-app/open {intent?}` opens PS5Upload Play (409 `other_game_running` while a game runs:
  opening it would close that game).
- `/api/app/close` and `/api/proc/kill` while a room is held: `409 room_active` with the room
  (`{"error":"room_active","room":..,"rooms":[..]}`) unless the body has `"room_ok": true`. The
  page asks first, naming the players, then sends `room_ok`.
- Requests from LAN addresses: 400 burst, 60 a second sustained, then `429 slow_down` with
  `Retry-After` (the TV's own browser, 127.0.0.1, is never gated). The page waits and retries
  quietly (3 times).

### Deviations

1. **Hosted-by marker is its own method (`play.room`), not the mesh hello's `play` field**
   (rooms design 10.4). The hello rewrites the device list on disk every call and is sent every
   5 minutes; the room changes with every join, leave and ping. `play.room` is memory only with a
   TTL, so a room whose engine went away disappears by itself.
2. **`pads.list` has no count of the console's own controllers** (integration design (d)): no
   helper-side device list was found; only guest places are counted. The engine still knows its
   own Remote Play pad (Player 1).
3. **`vpad.status` names the reading `seen_user`**: its `user` already meant "made for"; `vpad.hb`
   uses `user` (new there) as the brief asked.
4. **Support data:** `client/src/data/support.json` was not on `play-13` yet, so Minion reads
   `donate.ts` (crypto), `lib/supportLinks.ts` (`COFFEE_URL`) and a `THANKS_PAGE_URL` constant in
   `client/src/console/thanks/Thanks.tsx`; switch those imports to `support.json` once it lands.
   The codes are drawn on the page by the console's own encoder (`ui/qr.ts`), not pre-built SVGs.
5. **Minion's rules page shows the notice read only**: accepting it happens where rooms are made
   and joined. Its words are new `cw_room_notice_*` keys (the console strings are their own set);
   keep them in step with agent C's `room_notice_*`.
6. **"Join a friend"** opens the app with an intent left at the helper (`/v1/intent`), since a
   title cannot be launched with arguments from Minion.
7. **has_user cannot see a pick of the same user the pad was made for** (see above): the engine's
   PS-block / stuck-guest rules should treat `has_user:false` as "maybe still picking", and keep
   the existing "Server shutting down" fallback for the host-user case.

### Hardware (Pro, FW 13.60, 2026-11-27, test helper from this branch)

- `pads.list`, `play.room`, `play.grant` (and its `bad_request`), `vpad.open` (no bind, no input)
  → `vpad.hb`/`pads.list` owner readback (owner = made_for = the front user, `has_user:false`
  after the fix above) → `vpad.close`: all as described.
- Minion over LAN (switched on for the test, off again after): `/api/room` returns the room,
  `/api/app/close` without `room_ok` answers `409 room_active` with the room; Home shows "Your room"
  (name, lock, game · Friends · 2 of 4 players, Mia/Lee with bars and ms, 1 asking, "Hosted from
  Test Mac (not connected right now)"), Rules & responsibility and Thanks & Support pages render.
- Rate gate: 700 rapid requests from the Mac → 447 served, then 429 with Retry-After 1→6 s; after
  the wait, served again.
- Not tested on hardware: reading `/v1/play/grants` and `/v1/intent` (127.0.0.1 only on the
  console: needs PS5Upload Play), a real pick on a guest pad, bound pads.
