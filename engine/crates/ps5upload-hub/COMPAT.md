# Hub compatibility: the Play Rooms hub (13.0.0) and released apps

Checked on 2026-11-28, before the Fly app `ps5upload-hub` is redeployed with Play Rooms. The new hub
is `play-13` at 61aacb43c; the released one is tag `v12.6.46` (the hub on Fly today is that
protocol: v1, no rooms). Nothing was deployed and Fly was not touched.

**Verdict: go.** The new hub runs released 12.6.46 apps exactly as the old one does, a new app
on the old hub keeps internet invites and turns Rooms off quietly, and Play Rooms work end to end
on the new hub once the two bugs below are fixed (both are in the apps, not the hub; fixed on
this branch). Deploy the hub before the website's Rooms page goes live (see "Order").

## How it was run

- **Where.** A Linux container (Playwright 1.65.1's Ubuntu image with Rust 1.99.1), the hub and
  the engine built in it from each tag. Not on the Mac: macOS's application firewall drops
  incoming UDP on the LAN addresses to any new, unsigned binary (checked with a 20-line UDP
  listener: loopback gets the packet, 192.168.x and the Tailscale address don't), so WebRTC
  between Chromium and a freshly built engine never connects there, whichever hub runs. On the Mac
  the 12.6.46 run got through signalling to ICE with both hubs and stopped at the same point.
- **What plays the host.** Each tag's own engine test build with its Play Together code, routes,
  WebRTC and hub client, and a pretend console session (the `e2e_internet_guest_fixture` fakes;
  12.6.46 has no replayed host). **The guest** is each tag's own website page (`site/join/`,
  `site/pad/`, `site/rooms/`) served from a local origin in headless Chromium. **The hub** is a
  real `ps5upload-hub` process of either tag (`PS5UPLOAD_E2E_HUB_BIN`).
- **Specs.** `client/e2e/play-together-internet.spec.ts` (invite → name → host allows → Player 2,
  route `internet direct`, `lan` path, and the timing table through a 0/100/200 ms hub) and
  `phone-pad-internet.spec.ts` (a phone as a controller, presses reach the console, rumble, a
  watcher gets the picture) from each tag; `play-rooms-internet.spec.ts` (new, this branch).
- **Left out, the same for every hub:** the guest page's picture step (`E2E_SKIP_PICTURE`; this
  Chromium build never draws the H.264 frame on the join page in the container, also 12.6.46 on
  its own hub; the phone page's picture does draw) and "the invite service can't be reached"
  (it uses no hub; it fails on 12.6.46 with its own hub too).

## Results

| Test | App | Hub | Runs | Result |
|---|---|---|---|---|
| Baseline | 12.6.46 | 12.6.46 | 1 | pass (4/4) |
| 1. Old apps on the new hub: invite, join, WebRTC, phone pad, timing | 12.6.46 | 13.0 | 3 | **pass** (4/4 each) |
| 2. New app on the old hub: invite, join, WebRTC, phone pad, timing | 13.0 | 12.6.46 | 2 | **pass** (4/4 each) |
| 2. New app on the old hub: a Public room | 13.0 | 12.6.46 | 2 | **pass** after fix 2 |
| 3. New on new: invite, join, WebRTC, phone pad, timing | 13.0 | 13.0 | 1 | pass (4/4) |
| 3. New on new: Play Rooms end to end | 13.0 | 13.0 | 3 | **pass** after fix 1 |
| 4. Load test, Linux (musl, the production image) | | 13.0 | 3 | **pass** |

Join to channel open (median of 3, ms) through a hub 0 / 100 / 200 ms away, Join pressed at once:
12.6.46 on its hub 309 / 435 / 960; 12.6.46 on the new hub 121–206 / 452–465 / 954–977; 13.0 on the
old hub 117–185 / 402–450 / 946–953; 13.0 on the new hub 125 / 452 / 949. The same within noise.

**Test 2 in detail.** A Public room on the old hub: the engine's room says `not_listed: old_hub`,
`/api/join/rooms` answers the hub's 404 (the apps show "Your rooms service has no public rooms"),
an internet invite on the same hub plays as Player 2, and over the next 40 s the lobby opens no
socket (it waits 10 minutes), the old hub counts no errors and logs only its start line and a
count summary, and the engine logs nothing about it.

**Test 3 in detail.** Lobby opened and its card made with the hub's real proof of work (18
bits); the card in `GET /rooms` with `pw: true` and no address anywhere; CORS: our origin gets
`access-control-allow-origin` for itself, an app (no Origin) gets the list without CORS headers,
another origin gets 403; in Chromium a page on our origin reads the list and a page on another
origin is blocked; the website's Rooms page shows the card (its requests to hub.phantomptr.com answered by the local hub); `GET /rooms/<id>/preview` relays the
host engine's details (no address); the engine's proxy `/api/join/rooms` has the room; the guest
page from the card: name, password, the one-time rules notice, the knock sealed to the room key,
the host sees `password_ok: true`, allows, WebRTC through the hub's signalling, Player 2 via
`internet`; the hub's log never names the room or the guest.

**Test 4 (Linux, `rust:1.100.0-alpine3.24`, the hub's production build image).** 1,000 rooms
(lobby + invite socket each), 60 s steady + 40 s flood, all three runs:

| Run | Host connects failed | Host sockets dropped | Resident (idle / rooms / peak) | People's list p99 (steady / flood) | 429s to people | Hub CPU (steady / flood) |
|---|---|---|---|---|---|---|
| 1 | 0 | 0 | 1.7 / 24.6 / 26.0 MiB | 7.6 / 1.0 ms | 0 | 3 % / 100 % |
| 2 | 0 | 0 | 1.7 / 24.6 / 26.0 MiB | 3.5 / 5.5 ms | 0 | 3 % / 97 % |
| 3 | 0 | 0 | 1.7 / 24.6 / 26.0 MiB | 3.1 / 3.0 ms | 0 | 3 % / 97 % |

Every run: 1,000 cards at the end, 500 knocks answered, robots slowed (3.7–4.2 million 429s,
7 temporary blocks), at the design's rates 25.0 GB a month out (budget 100: keepalive 19.4,
status 2.4, renewals 1.6, lists 1.2, knocks 0.2, previews 0.1). The flood switches cut-back mode
on (by projected traffic), as designed. Outbound during the flood is mostly refusals: about 820 MB
in 40 s of 429s to 5 robots (~195 bytes each). The hub stays up and people are served; a long
flood is a Fly bill ($0.02/GB), not an outage (FLY.md, "Floods").

**The "72 host sockets failed" glitch: not reproduced.** 0 in the 3 Linux runs, 0 in a Mac run
today and in two earlier Mac logs from 2026-11-27 (`host sockets dropped 0` everywhere,
which the test asserts). The message printed every WebSocket that could not connect, knocking
guests (172.16.x) included, as "a host could not connect", while dropped hosts are counted
separately and fail the run. The likely cause (a hypothesis: that run's log is gone) is the test
machine itself: on macOS ~16k ephemeral ports, kept 30 s after close (`net.inet.tcp.msl` 15000),
against roughly 14k short connections a run makes (an estimate from its counts); the test's
comments show the robot loops were paced for exactly that. The message now says which it was
(`tests/load.rs`). Nothing points at the hub.

## Bugs found and fixed

1. **Every password or friend-pass knock from the website and the apps read as "Wrong
   password".** The page's `seal()` (client/src/lib/room/knock.ts, used by the join page and the
   desktop and phone apps) sealed differently from what the engine opens (room_seal.rs) and
   PS5Upload Play seals (src/net/seal.cpp): no version byte, the ephemeral key in the HKDF info,
   the gid as AES-GCM additional data, and `pw`/`pass`/`c` instead of `secret`/`challenge`. Found by
   test 3 (the guest got "Wrong password." for the right one). Fixed in the page to the engine's
   format (the gid and the challenge stay bound: both are inside the sealed text, which the engine
   checks). Tests: `room_seal::tests::opens_what_the_web_page_seals` opens blobs made by the page's
   own `seal()`; `room.test.ts` opens the page's blob the engine's way. Embedded and site pages
   rebuilt (`join_page.html`, `pad_page.html`, `site/join/`, `site/pad/`).
2. **On an older hub the lobby left a room behind for 2 minutes.** An old hub ignores `lobby` and
   opens a plain room; the engine closed the socket without `leave`, so the room waited 2 minutes
   for a reclaim and counted toward the computer's 8 rooms on that hub, every 10 minutes. Now it
   sends `leave` first. Test: `join_net::lobby::tests::an_older_hub_gets_its_room_back_and_a_long_wait`
   (fails without the fix); test 2 checks the old hub has only the invite's room open.

## Not fixed (note)

- **Order.** The website's Rooms page calls the hub's `/rooms` from the browser. An old hub has no
  `/rooms` and sends no CORS header, so a browser can't read its 404 and the page would say "Can't
  reach the rooms list" instead of "no public rooms". Deploy the hub first, then the site.
- `ava1_only_tests::transfer_rar_with_no_ava1_listener_is_helper_not_ava1` fails on this branch
  (400 instead of 202), before and apart from these changes; nothing here touches it.

## Re-running

```sh
# Linux box (Playwright image + Rust), the two checkouts mounted read-only, then per checkout:
cargo build -p ps5upload-hub && cargo test -p ps5upload-engine --lib --no-run   # in engine/
cd client && npm ci
# old app, new hub (and the other way round with the other checkout and hub binary):
PS5UPLOAD_E2E_INTERNET=1 PS5UPLOAD_E2E_HUB_BIN=<new hub> E2E_SKIP_PICTURE=1 PS5UPLOAD_E2E_TIMING=1 \
  npx playwright test play-together-internet phone-pad-internet --grep-invert "be reached"
# rooms on the new hub; on an old hub add PS5UPLOAD_E2E_HUB_OLD=1 and its binary:
PS5UPLOAD_E2E_INTERNET=1 npx playwright test play-rooms-internet
# load (in rust:1.100.0-alpine3.24 with musl-dev and procps):
cargo test -p ps5upload-hub --release --test load -- --ignored --nocapture
```

`E2E_SKIP_PICTURE` exists only in the container's copy of the 12.6.46 and 13.0 invite specs (a
two-line guard around the picture step); the committed specs still check the picture.
