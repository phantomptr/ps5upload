# Play Rooms in PS5Upload Play (13.0.0)

What the app does for Play Rooms, how it talks to the engine, the hub and the helper, and where it
departs from the designs (the Play Rooms design, 2026-11-28, and the integration design,
2026-11-27, in the docs repo). Code: `src/core/{rooms,chat,voice,seats,textkb,notice}.*`,
`src/net/seal.*`, `src/app/{rooms_net,rooms_ui,room_host,room_overlay,seats_app}.cpp`,
`src/ps5/voice.cpp`.

## Screens

- **Home** (role-aware): the best next step first — back to a game this PS5 was in (kept 5 minutes,
  `resume.txt`), a friend's room open (the hub's `/rooms/status`, at its `every_s` pace), a pushed
  invite, a game nearby, Play again — then Join a friend's game, Host from this PS5, Settings,
  Thanks & Support, Close. Footer: the PS5Upload this PS5 knows and the version.
- **Join** (the Join list): Your friends' rooms, Shared with you, Nearby, Public rooms (the hub's
  list), then Enter a code and Type an address. Cards twice a row's height with the expected round
  trip as bars and words (measured, last time, or about). L1/R1 cycle the sorts, △ opens the filter
  sheet (Has space, No password, Works with my version, Near me, Language, Game on the keyboard),
  □ opens Details. A card focused for a second fetches its Details (a public card only after the
  notice): the host engine's preview, and for a game nearby a real round trip (5 checks on
  `/api/join/probe`). Measured values never re-sort the card under the cursor.
- **Joining a room** = the same flow as an invite: name → password (the keyboard) → waiting (with
  the real round trip and Players on this PS5) → playing. Name taken: ✕ Join as "Sam (2)", □ Pick
  another name. Wrong password: "Wrong password. N tries left." and ✕ types it again.
- **In game**: chat lines bottom-left (the last 3, fading after 6 s, links spelled with "·", rude
  words hidden in quick-only rooms by default), unread count and the mic's state top-right, who is
  talking top-left. **Room menu**: hold OPTIONS + TOUCHPAD, then △ (△ never reaches the game, and
  no PS press follows): your ping and the host's PS5 link, 16 quick messages, Type a message, My
  mic, How I talk, game sound while others talk, Show chat, each person (←/→ volume, □ mute for
  you, △ Tell the host), Rules & responsibility, Leave room. While it is open the game gets
  "nothing pressed".
- **Host** (L1/R1 tabs): My room (the room's line and listing, your PS5 link, asks with source,
  tag, password ✓ and the round trip, players with bars, room settings: who can see it,
  password, size, approvals, chat, voice, new players muted, mute all, lock, blocked, name; New
  invite code; Close room), Remote Play, This PS5. Rows are kept by identity, so a new ask never
  moves the cursor. Ending or loosening actions (close, wider visibility, removing the password,
  approvals on, unlock, PS & Create to friends, a friend on the stream's controller, invites
  anywhere) ask in words first and are only for the stream's PS5 user or the console's owner
  when the engine names them (`session.user`, `console.owner`).
- **Notice** (once per version, Settings › Rules & responsibility shows it again), **Thanks &
  Support** (the names from `client/src/data/supporters.json`, bundled; a newer list from a
  nearby engine's `GET /api/support`; Ko-fi and the site's thanks page as QR codes), **How it
  works** (3 pages, first run), **Controls**.

## Voice

- Capture: `libSceAudioIn` voice chat port, 16 kHz mono, grain 256, its own thread, opened only
  when the person turns the mic on (off on every join). Opus VOIP, wideband, DTX and in-band FEC,
  at the engine's `kbps`/`frame_ms` (24/20 or 12/40).
- Nothing goes out unless: mic on, the room's voice not off, not muted by the host, a live mic
  (silent state 0), the app in front, not paused, and push-to-talk held (Create, kept from the
  game while the guest's PS & Create are blocked) or open mic in an open-mic room **with a
  headset** (no echo cancelling here; the headset bit of `sceAudioOutGetPortState` is 0x40 as in
  PS4 headers — not yet seen on hardware).
- Playback: each speaker (by `sid`) through its own jitter buffer (40–100 ms) and Opus decoder at
  48 kHz, mixed into the game's sound, which dips by "a little" (−6 dB) or "more" (−12 dB) while
  someone talks (50 ms down, 400 ms back). Volume 0–200 % and mute per person, this PS5 only.

## Wire (as the engines merged into play-13 speak it)

- Chat and voice: API.md "Play Rooms chat and voice" — `{"notice": n}` once the notice is accepted
  (sent on every join when `comms.accepted` is false), `{"chat": {"quick"|"text"}}`,
  `{"mic": ...}`, `{"flag": sid}`, binary up `[0x0A][seq][level][Opus]`, down `[0x0B][n]…`;
  `comms`, `voice` (people by sid, you, pace), `muted`, `chat` (ids, `x`), `chat_history`,
  `comms_refused` (every `why` in words; `notice` shows the notice).
- Hub (PROTOCOL.md "Rooms"): short GETs over the app's own TLS client — `GET /rooms` (sort,
  filters, page), `GET /rooms/status?r=` at `every_s`, `GET /rooms/<id>/preview`. A 429 is waited
  out silently (Retry-After, jittered); other failures back off doubling to about a minute.
- Helper (app API 2): `/v1/version`, `/v1/play/grants` (one-time, pairs without a code: the
  companion hello carries `grant` and `app_id`), `/v1/intent` (read at start: `join`/`rooms` open
  the Join list, `host` the host screen), `id` on `/v1/mesh` entries. Older helpers: everything
  falls back to discovery and the code.

## Assumptions the engine has to match (not in the merged API yet)

1. **Knocks on the guest socket** (Nearby, Play again, LAN/Tailscale rooms): the hello says
   `"knock": {"via": "rooms"|"friends"|"nearby"|"again"}`, plus `"device"` (base64url of this
   PS5's 16-byte id), `"proto": 2` and `"caps": [...]`. A room with a password answers
   `{"knock": "challenge", "c": "<base64url>", "pk": "<room key>"}`; the app answers
   `{"knock": "sealed", "blob": "<base64url>"}`; then `{"knock": "wait"}` / `{"knock": "no",
   "why", "left"}` or the usual stages. `{"name_taken": {"suggest"}}` ends the try (the app joins
   again with the name chosen).
2. **The sealed blob** (`src/net/seal.hpp`, checked against Python `cryptography` in `make test`):
   `0x01 ‖ ephemeral P-256 point (65) ‖ nonce (12) ‖ AES-128-GCM(ciphertext ‖ tag)`, key =
   HKDF-SHA256(ECDH x, salt = the challenge's bytes, info = "ps5upload room knock v1", 16 B),
   plaintext `{"secret","gid","challenge"}` (gid "" on the guest socket, the hub's gid on a lobby).
3. **Public and friends' rooms through this home's PS5Upload** (no WebRTC in the app yet):
   `/api/join/bridge` with `{"bridge": {"v": 2, "lobby": "<id>", "knock": {"via", "name",
   "device", "client": "ps5", "ver"}}}`; the bridge relays the lobby's knock answers
   (`challenge` with `pk` and `gid`, `wait`, `no`) and takes the app's `{"knock": "sealed"}`, so the
   password never reaches the bridge in the clear; then `{"bridge": "open"}` and the usual hello.
   Without a PS5Upload at home the card says to keep one open (the fallback rule).
4. **Host controls on the companion socket** (`/api/play/app`, token-scoped): `{"room": {key:
   value}}` (visibility, password, approval, chat, voice, start_muted, lock, name),
   `{"kick": {"player", "block": "room"|"always"}}`, `{"unblock": {"tag"}}`, `{"mute": {"player",
   "voice", "chat"} | {"all": true, "voice"}}`, `{"decide": {"ask", "allow": "group"}}`,
   `{"chat": {"quick"}}`. The doc carries `room` (as `GET /api/join`), each guest's `tag`,
   `from_rooms`, `muted`, `group`, each ask's `via`, `tag`, `password_ok`, `rtt_hint`, `players`
   (a group), and `session.user` / `console.owner`. Chat and voice modes and mutes must reach
   `join_comms`.
5. **Many controllers**: a seat's hello has `"media": false` and `"group": "<first player's
   token>"`; `{"release": 1}` frees the first player's place while the others play on.
6. **Discovery** answers `you: {known, streaming, paired, name}`; `joined` names the host's
   `lobby` (kept with the friend pass for "<friend>'s room is open").
7. **The room push** `{"room": {name, size, players: [{id, name, color, seat, you}], you, host_link,
   chat, voice, muted}}` is read when the engine sends it; the app works from `comms`/`voice`
   alone too.

## Tests

- `make -C apps/play test`: `test_rooms` (lists, sorts, filters, previews, the room push, comms
  and voice state, refusals, names, chat cleaning/rate/log/history, voice framing and gating,
  push-to-talk on Create and the Room-menu chord, the notice gate, engine-id pairings, the
  keyboard, seats, the sealed knock) and `seal_check.py`; the e2e against the stand-in engine
  (`tests/fake_engine.py`): sealed knocks (direct and through the bridge), wrong password, name
  taken, chat before/after the notice, history, ids, voice up/down, groups, the hub list with a
  429, friends' rooms, Details with a measured round trip, room controls from the TV, helper
  grants and intent.
- `make -C apps/play shots`: every new screen (`rooms`, `rooms-details`, `rooms-filter`,
  `rooms-offline`, `keyboard`, `notice`, `thanks`, `welcome`, `help`, `name-taken`,
  `wrong-password`, `asking-ping`, `asking-seats`, `playing-chat`, `playing-room-menu`,
  `host-room`, `host-this-ps5`, `host-close`); `LANG_CODE=xx` for each language.
