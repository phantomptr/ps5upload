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

## Engine (rooms model, host controls, lobby, audit fixes, pairing)

Built against the rooms design (sections 2-6, 9-10) and the hub's PROTOCOL.md "Rooms". Where the
engine differs from the design:

1. **The way someone came is `came`, not `via`**, on guests and asks in `GET /api/join` (and the
   app status): `via` already means `lan`/`internet` there. Words: `invite`, `again`, `nearby`,
   `friends`, `rooms`, `group`.
2. **Refusal words on the guest socket** keep the old ones where they existed: a host's No and a
   block are `denied` (the lobby says `no`). New: `full` with `allowed`/`in`, `locked`, `password`
   with `tries_left`, `locked_out` with `wait_s`, `host_away`, `not_listed`, `old`, `name_taken`
   with `suggest`, `already_asking` (one ask per device), `expired` (a link this room gave out
   before; a link that never was stays `key`), `code` (a wrong typed code; older guests keep
   `key`).
3. **A knock from the home network or Tailscale carries the password in the clear**
   (`hello.knock.password`): the http guest page has no WebCrypto, and the socket never leaves the
   home network or the tailnet. Over the hub it is always sealed (`room_seal.rs`).
4. **A lobby knock hands back a resume token** (`{"knock": "ok", "token", "friend"?, "lobby"}`):
   the knock runs the normal guest session (gate, the host's answer, the seat), the seat is held for
   the resume window, and the data channel's hello is `{"hello": {"resume": token}}`. No second
   admission path.
5. **Chat and voice are join_comms.rs's** (agent B): the room keeps their modes in its settings and
   hands them over (`set_modes`) on open and on every room change; mutes are `POST /api/join/mute`.
   `ChatMode`/`VoiceMode` are B's types.
6. **Host away** = no host page attached to the console's session for 5 minutes
   (`rp_stream_api::host_away`), so a host who moved to another screen still gets asks.
7. **"PS5s of this household get in without asking"** is `approval.household` (off): a client that
   says `kind: ps5` from an address in this engine's console list.
8. **Keep other guests' pads through the host's restart**: only a guest who most likely took the
   host's user gives their pad back after the grace; everyone else keeps theirs (nothing to pick
   again on resume).
9. **The hosted-by marker** is the helper's `play.room` (203), every change and every 30 s, null on
   close; `play.grant` is 204 with a 64-hex grant (the helper's form).
10. **Name rules** come from `ps5upload_hub::names` (one copy); the look-alike table for unique
    names is a small vendored subset (Latin/Cyrillic/Greek and digit look-alikes), not full UTS #39.
11. **One sealed-knock format, PS5Upload Play's** (`apps/play/src/net/seal.cpp`): `0x01 ‖ point
    (65) ‖ nonce (12) ‖ AES-128-GCM`, HKDF info without the point, no AAD, plaintext `{"secret",
    "gid", "challenge"}` (gid "" on the guest socket). One secret per knock: the password, or for
    Play again the friend pass — so **a Play again knock needs no room password** (its pass is the
    proof; the host let them in before).
12. **The guest socket answers a knock that needs the password and didn't say it with a
    challenge** (`{"knock": "challenge", "c", "pk"}` → `{"knock": "sealed", "blob"}`); a plain
    `knock.password` is still taken on that socket. Refusals there are the usual stages (`{"stage":
    "refused", "why": …}`), and a taken name is `{"stage": "refused", "why": "name_taken",
    "suggest", "name_taken": {"suggest"}}`.
13. **Bridge v2** (`{"bridge": {"v": 2, "lobby", "knock"}}`): the bridge knocks on the lobby with
    its own hub connection and relays the answers; `challenge` gains `gid` (the bridge's), `ok`
    carries `token` and the host engine's DTLS `fp` (the lobby card has no fingerprint), and after
    `no` the bridge says `{"bridge": "failed", "why": "refused"}`.
14. **Companion intents** take `room` as `{"set": {…}}` or the fields themselves (`approval` may
    carry one switch: the others are kept); `decide` takes `allow: true | false | "group" | "me"`;
    `chat {quick | text, notice?}` is the host's line through join_comms; `ready {start: true}`.
    The status has `session.user` (the PS5 user in front, else the session's paired user) and
    `console.owner` (the first Remote Play user paired on this engine with a name, else the
    stream's user).

PS5Upload Play's assumptions (apps/play/docs/ROOMS.md): all in the engine now (see "Reconciled"
below).

Open items: the internet-direct preview channel for Friends (Details over the internet go through
the hub's relayed preview); the card's game is the host's pick or the console's running title
(asked once a minute), the marker sends only a picked game.

## Reconciled (rooms-gaps): one form for each, in engine, PS5Upload UI and PS5Upload Play

Checked end to end by `make -C apps/play e2e-engine`: the real engine's routes in-process
(`engine/.../src/play_cli_e2e_tests.rs`) with the app's own guest and companion code (`play-cli`).

| What | The one form | Fixed on |
|---|---|---|
| How someone found the room | `came` (`invite`, `again`, `nearby`, `friends`, `rooms`, `group`) on guests, asks and Recent players; `via` stays `lan`/`internet` | engine (asks and guests in the app's status had none); UI read `source`, app read `via` |
| Let a PS5's players in | `decide {id, allow: "group"}` (HTTP and the TV) | UI sent `allow: true, group: true` |
| A PS5's group on an ask | `group`: the first player's id; `players`: names of everyone from that PS5 with the one asking (the count the UI and the app show) | UI read `group` as a count |
| Wrong password | `{"stage": "refused", "why": "password", "tries_left"}` on the guest socket, `{"knock": "no", …, "tries_left"}` on a lobby | app read `left` |
| Name taken | `{"stage": "refused", "why": "name_taken", "suggest", "name_taken": {"suggest"}}` | app saw a plain refusal |
| Status + room push | one message `{"room": …, "status": …}` every second | app read only its first key (the room), never the status |
| Room approvals from the TV | one switch at a time, merged | engine replaced all three (one switch reset the others) |
| The host's quick message from the TV | `chat {quick, notice}` | app sent no notice (engine refuses before it) |
| Rooms list through the engine | `/api/join/rooms?<query>`, `/api/join/rooms/status?r=`, `/api/join/rooms/{id}/preview` (the hub's own paths) | engine took `?status=` / `?preview=` |
| Ready check | `POST /api/join/ready {addr, start}`, TV `ready {start}`; guests get `{"ready": {"left_s"}}`, answer `{"ready": true}`; views carry `ready {left_s, asked, ready, all}` and each guest `ready` | engine (new) |
| Recent players | `GET /api/join/history` → `{players: [{id, name, tag, last_unix, minutes, came}]}`, `POST /api/join/history/forget {id} | {all}` | engine (new) |
| Friends playing guard | `X-Confirm-Friends: 1`, `?confirm_friends=1` or `confirm_friends` in the body | already one |
| A room handed over from the phone | `https://…/join/#lobby=<id>&n=&tag=&pw=1&via=rooms|friends`: the app joins it as that room's card | app (new) |

### Gaps closed (engine)

- **Many controllers on one PS5**: a hello with `group` (a player-in's resume token) and
  `"media": false` asks as `came: "group"` (no invite or code needed, no same-device refusal),
  never a stranger unless that first player is; `decide allow: "group"` lets every further
  controller of that PS5 in without asking. Each counts against the room's size (`full` when it is).
  Each socket has its own pad, status, rumble and light. `{"release": 1}` from the first player
  frees their pad and place; their socket keeps the picture for the others and ends (`bye: left`)
  with the last of them. Guests carry `group` in every view.
- **"It's me"**: an ask from the host's own device (this computer, or a PS5 whose PS5Upload Play
  is paired here) carries `own: true`; `decide allow: "me"` (only for such an ask, else 409
  `not_own`) seats it on Player 1 when no host page drives the stream's controller, whatever "A
  friend on the stream's controller" says, until a host page drives it again.
- **Auto-restart after a friend takes the host's user**: 15 s and 35 s after the PS5 ended the
  stream for that reason, with no page started it again, the engine starts it itself with no
  page attached (the last session's size, rate, codec and paired user; keys from the store), at
  most 3 times in 10 minutes per console; logged either way. Not seen on hardware.
- **Ready check, Recent players** (above); **the host's PS5 link** (`host_link {rtt_ms, tier}`,
  Remote Play's own round trip) in the room view, the room push and the preview.
- **The helper's mesh hello** answers `"app_api": true` (payload/src/mesh.c).

### PS5Upload Play side

- The parsers that share token buffers (the room push, the chat history) take one reader at a
  time: every controller's socket has its own thread.
- The TV offers them (13.0.0 audit): an ask from the host's own device says so and △ is "It's me"
  (Player 1); My room has a Ready check row with each player's ✓; the player menu moves seats,
  gives Player 1 (the owner only) and takes it back. As a guest the app shows the host's "Ready?"
  over the game and puts "I'm ready" first in the Room menu (it answers for every player of that
  PS5). PS5Upload's own asks (panel and the prompt over the picture) offer "It's me" too.
- Many controllers: a new controller after the first player left (`release`) proves its group
  with another seat's token; the engine finds the group through any player of it
  (`group_by_token`) and keeps "let all from that PS5 in" on each of them (`allow_group`).

## Fixes before release (fixes-13)

- **Friend passes**: given only to people the host let in who are not strangers (from the public
  Rooms list, or playing beside one: `Meta::stranger`); a stranger's `remember` is ignored. The
  pass's id stays on the guest (`Meta::friend`, also for Play again); a remove or block revokes it.
- **A released first player's talk**: their socket (it keeps the picture for the others of that
  PS5) speaks in chat and voice as the PS5's player still in (lowest number; `Link::rebind`), with
  the notice, mic and the host's mutes carried along, so per-player mutes and remove reach it.
- **Two consoles hosting on one engine**: still one lobby per engine. It stands for the room opened
  first (`room_api::lobby_console`): its card, preview and every knock from a card (hub, or
  `knock.via` rooms/friends on this network) go there (`Entry::Lobby`). The other room's view says
  `listed: {state: not_listed, why: "other_console"}` (clients show "Not listed right now").
- **Asks per address**: one per device id; up to 3 from one address when each names its device
  (`MAX_PENDING - 1`, one place stays for another home), 2 without device ids.
- **Unique names**: checked again under the lock that queues or seats the ask, and held
  (`Party::claims`) until the ask ends. A device back without its token (same device tag, its old
  place away in the resume window) may take its own name again; once let in, the old place goes.
- **PS5Upload Play**: a change of way runs on the route thread (the picture never waits on it);
  a failed change waits 5 s, doubling to 2 minutes; `replaced` from the guest's own change never
  ends the game (the new socket takes over, or the token brings it back). "Back to game" keeps
  and uses the way the game was played over (`core/resume.hpp`).

# Play Rooms: where the clients (rooms-ui) differ from the design or assume

Branch `rooms-ui` (PS5Upload UI: desktop/phone/Docker web UI, guest join page, website Rooms page).
The design is `ps5upload-docs/superpowers/specs/2026-11-28-play-rooms-design.md`. The clients follow
the merged hub (`PROTOCOL.md` "Rooms"), the merged chat and voice engine (`API.md` "Play Rooms chat
and voice") and, where it was readable, the `rooms-engine` branch (`join_room_api.rs`,
`join_net/lobby.rs`, `room_seal.rs`, `together_guard.rs`). What follows is what the clients do that
those don't spell out, or where they read something two ways.

## Engine API the clients read

- **Where a player or an ask came from.** `GET /api/join` already has `via: "lan" | "internet"` on
  each guest (how they connect). The room's "how they found it" is read from `came` first, and
  from `via` only when it is a room word (`invite`, `again`, `nearby`, `friends`, `rooms`, `group`).
  `from_rooms: true` also marks a stranger.
- **Room view** (`room` of `GET /api/join`): read as `join_room_api.rs` `room_view` writes it
  (`lock`, `open_for_s`, `listed {state: off|listing|listed|not_listed, why, until_unix}`,
  `blocked [{name, tag, at_unix, always}]`, `preview_shows {…, check}`, `many_wrong_passwords`,
  `host_tag`, `lobby`) plus the chat/voice fields of `join_comms`. Also accepted, from the design:
  `locked`, `opened_at(_ms)`, `check_before`, `ready {left_s, ready}`, `host_link {rtt_ms, tier}`.
  An engine without `room` shows a Private room with the defaults and no room controls.
- **Room changes** go to `POST /api/join/room` with the engine's names: `preview` (the whole
  PreviewShows with `check`, never a part), `lang: "any"` for Any, `region: "hidden"` for no region,
  `public_ok: true` once the first-time Public confirm was given. Chat and voice modes and "new
  players start muted" go to `POST /api/join/comms`. `POST /api/join/open` carries the same body as
  `room` (Create a Play Room); when it also set chat/voice, `/api/join/comms` follows the open.
- **Ready check** (design 7.5): `POST /api/join/ready {addr, start: true}`; guests send
  `{"ready": true}` and get `{"ready": {"left_s"}}`. In the engine (rooms-gaps); the buttons show only
  when the room view has `ready`, and the guest prompt only when the message comes.
- **A whole PS5 at once**: `POST /api/join/decide {addr, id, allow: "group"}` for an ask with
  `players` naming more than one (integration P2).
- **Recent players**: `GET /api/join/history` → `{players: [{id, name, tag, last_unix, minutes,
  came}]}`, `POST /api/join/history/forget {id} | {all: true}`. Hidden when absent.
- **Rooms list in the apps**: `GET /api/join/rooms?<hub query>`, `/api/join/rooms/status?r=…`,
  `/api/join/rooms/<id>/preview` on the engine as a proxy of the hub's `/rooms…`, with an optional
  `hub` (host[:port]) in the list answer so the app knocks on the same hub. When the engine answers
  404, the app calls the hub itself (`https://hub.phantomptr.com`).
- **Preview on the home network**: `GET /api/join/preview?room=<lobby>` on the host engine.

## Guest protocol additions

- **Hello** (all 13.0 guests): `device` (16 random bytes, base64url, kept per browser/app),
  `proto: 2`, `caps: ["rooms","knock","sealed","preview","chat","voice","ready","seat_move","watch"]`.
- **Knock over the hub**: `{"knock": {via, name, device, client: "web" | "app", ver, proto, caps}}`.
  A challenge is answered with the blob of `room_seal.rs` (info `"ps5upload room knock v1"` ‖ E's
  public point; the challenge's `pk` wins over the card's). After `{"knock": "ok", "token"}` the
  page's first `hello` on the data channel gets `resume: <token>` added (and its other fields stay).
  Knock answers reach the session as stages: `wait` → `{"stage": "asking"}`, `no` →
  `{"stage": "refused", why, tries_left?, suggest?}` (`no` reads as `denied`). A hub
  `rate_limited` on a knock is waited out quietly (Retry with `retry_s` + jitter), never shown.
- **Refusal words** the clients know: `locked`, `host_away`, `password` (`tries_left`),
  `locked_out`, `not_listed`, `closed`, `not_playing`, `old`, `name_taken` (`suggest`, also read from
  `{"name_taken": {"suggest"}}`), and `bye: "removed"`.
- **Room push to a guest**: `{"room": {name, size, players: [{name, player|seat, host?, you, color?,
  id?}], you {rtt_ms, tier}, host_link?, chat, voice, locked, caps, lobby?, visibility?}}`. `lobby`
  is what a friend keeps to see "Alex's room is open" later (the engine's push carries it since
  the 13.0.0 audit; before, only `joined` had it and the page read the push).
- **Chat and voice**: exactly `API.md` (notice, comms, voice state with `people`/`you`, chat lines
  with `id`/`x`, `chat_history`, `comms_refused`, `muted`, binary `0x0A` up / `0x0B` down, mic key
  `mic`, `{"flag": sid}`). Lost voice frames are a short gap (the wasm Opus decoder has no
  concealment call). The separate unordered `voice` data channel is not opened yet: voice rides
  the `join` channel (allowed by API.md).

## Website and the PS5 app

- **Room hand-off link**: `https://ps5upload.phantomptr.com/join/#lobby=<id>&hub=<host>&n=<host
  name>&tag=<tag>&pw=1&via=rooms`. The join page joins it like an invite (name, password if `pw`,
  waiting, playing). The same link is what "Open in PS5Upload" (`ps5upload://join?link=…`) and "Play
  on my PS5" hand over: PS5Upload Play joins a `#lobby=` link from the phone hand-off as that
  room's card (rooms-gaps).
- **Rooms page**: `site/rooms/index.html` (built from `client/src/rooms`, `npm run build:rooms`,
  checked by `scripts/check-embedded-pages.mjs`). It calls the hub straight; the hub's CORS must allow
  the website origin (it does).

## Other choices

- **Friends playing** (`together_guard.rs`): one `fetch` wrapper in the app asks once and sends the
  same request again with `X-Confirm-Friends: 1`; the Stop buttons' own question counts for 5 s and
  the Remote Play socket's stop then carries `confirm_friends: true`. Calls the app makes through a
  Tauri command (not `fetch`) get the 409 as an error, unchanged. Installing a desktop update
  (it quits the app) reads `GET /api/engine/update` `friends_playing` and asks the same way; the
  web UI's update note says friends are playing.
- **Closing the desktop window while friends play**: asks; Keep running minimizes the window (the
  safe default), Quit ends their game. Needs `core:window:allow-destroy`.
- **Rest after uploads** is skipped while friends play on that PS5 (said in a notification).
- **Host's own mic** (voice from the host page's Remote Play socket, `room_mic`):
  `screens/Play/PlayRoom/HostVoice.tsx` (mic, push-to-talk on the backquote key, volumes).
- **PS5Upload Play pairing**: one click ("Does the TV of <PS5> show K7M-2QX? Yes, pair",
  `POST /api/play/app/pair_confirm`) and "Is it the same PS5?" for a paired PS5 at a new address
  (`POST /api/play/app/moved`), before a room starts and inside an open one.
- **GET /api/support** lives in `engine/.../support_api.rs` (this branch), routed in `lib.rs`, public
  like the guest pages. It reads `client/src/data/{supporters,support}.json`; the Dockerfile copies
  them.
