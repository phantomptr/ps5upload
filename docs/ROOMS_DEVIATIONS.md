# Play Rooms: where the clients (rooms-ui) differ from the design or assume

Branch `rooms-ui` (PS5Upload UI: desktop/phone/Docker web UI, guest join page, website Rooms page).
The design is `ps5upload-docs/superpowers/specs/2026-11-28-play-rooms-design.md`. The clients follow
the merged hub (`PROTOCOL.md` "Rooms"), the merged chat and voice engine (`API.md` "Play Rooms chat
and voice") and, where it was readable, the `rooms-engine` branch (`join_room_api.rs`,
`join_net/lobby.rs`, `room_seal.rs`, `together_guard.rs`). What follows is what the clients do that
those don't spell out, or where they read something two ways.

## Engine API the clients read

- **Where a player or an ask came from.** `GET /api/join` already has `via: "lan" | "internet"` on
  each guest (how they connect). The room's "how they found it" is read from `source` first, and
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
  `{"ready": true}` and get `{"ready": {"left_s"}}`. Not in the engine yet: the buttons show only
  when the room view has `ready`, and the guest prompt only when the message comes.
- **A whole PS5 at once**: `POST /api/join/decide {addr, id, allow: true, group: true}` for an ask with
  `group > 1` (integration P2). Assumed.
- **Recent players**: `GET /api/join/history` → `{players: [{id, name, tag, last_unix, minutes,
  via}]}`, `POST /api/join/history/forget {id} | {all: true}`. Hidden when absent.
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
  is what a friend keeps to see "Alex's room is open" later; when the push has none, the knock's
  `ok.lobby` is not stored yet (open item).
- **Chat and voice**: exactly `API.md` (notice, comms, voice state with `people`/`you`, chat lines
  with `id`/`x`, `chat_history`, `comms_refused`, `muted`, binary `0x0A` up / `0x0B` down, mic key
  `mic`, `{"flag": sid}`). Lost voice frames are a short gap (the wasm Opus decoder has no
  concealment call). The separate unordered `voice` data channel is not opened yet: voice rides
  the `join` channel (allowed by API.md).

## Website and the PS5 app

- **Room hand-off link**: `https://ps5upload.phantomptr.com/join/#lobby=<id>&hub=<host>&n=<host
  name>&tag=<tag>&pw=1&via=rooms`. The join page joins it like an invite (name, password if `pw`,
  waiting, playing). The same link is what "Open in PS5Upload" (`ps5upload://join?link=…`) and "Play
  on my PS5" hand over: PS5Upload Play (agent D) needs to accept a `#lobby=` link from the phone
  hand-off to join a room that way; until then it says it can't use the link.
- **Rooms page**: `site/rooms/index.html` (built from `client/src/rooms`, `npm run build:rooms`,
  checked by `scripts/check-embedded-pages.mjs`). It calls the hub straight; the hub's CORS must allow
  the website origin (it does).

## Other choices

- **Friends playing** (`together_guard.rs`): one `fetch` wrapper in the app asks once and sends the
  same request again with `X-Confirm-Friends: 1`; the Stop buttons' own question counts for 5 s and
  the Remote Play socket's stop then carries `confirm_friends: true`. Calls the app makes through a
  Tauri command (not `fetch`) get the 409 as an error, unchanged. The engine update path
  (`GET /api/engine/update` `friends_playing`) is not asked yet (open item).
- **Closing the desktop window while friends play**: asks; Keep running minimizes the window (the
  safe default), Quit ends their game. Needs `core:window:allow-destroy`.
- **Rest after uploads** is skipped while friends play on that PS5 (said in a notification).
- **Host's own mic** (voice from the host page's Remote Play socket, `room_mic`): not built in the
  client yet. Hosts hear and talk through a phone guest page for now (open item).
- **GET /api/support** lives in `engine/.../support_api.rs` (this branch), routed in `lib.rs`, public
  like the guest pages. It reads `client/src/data/{supporters,support}.json`; the Dockerfile copies
  them.
