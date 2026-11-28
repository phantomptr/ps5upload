# Routes: the best way to the host, with backups

How PS5Upload Play (and, in a smaller way, the browser guest page) picks the way to the host's
PS5Upload engine, keeps other ways as backups, checks them without disturbing the game, and
changes way when it should. It also covers what "a good connection" means in numbers.

Code:

| Part | Where |
|---|---|
| Tiers, meters, switch rules, backup budget (pure logic) | `apps/play/src/net/route.{hpp,cpp}`, tests `apps/play/tests/test_route.cpp` |
| Checking one way over the engine's checks socket | `apps/play/src/net/route_probe.{hpp,cpp}` |
| The guest's ways: race, backups, switching | `apps/play/src/app/guest_route.cpp` |
| Ways from discovery; the bridge way; this home's PS5Upload; the ways for a handed invite and Play again | `apps/play/src/app/route_pick.{hpp,cpp}` |
| Easy join's invites into the ways (§6) | `apps/play/src/app/easy.cpp` (`join_invite`, `join_again`), `waiting_room.cpp` |
| On screen | `apps/play/src/app/route_ui.{hpp,cpp}` |
| Engine: checks socket, tiers, per-guest way and tier, `/api/join/routes` | `engine/crates/ps5upload-engine/src/join_route.rs` |
| Engine: the bridge for internet invites | `engine/crates/ps5upload-engine/src/join_net/bridge.rs` |
| Browser page and host panel | `client/src/join/routeTier.ts`, `join/session.ts`, `screens/Play/PlayTogether.tsx` |
| Stand-in engine with slow and dark ways | `apps/play/tests/fake_engine.py --proxy`, `tests/e2e_host.py` §11 |

## 1. The tiers

A way is measured by four numbers:

- **Round trip (RTT).** This is app to engine and back, measured over the same TCP socket the game
  uses, so queueing behind pictures counts. It is smoothed as RFC 6298 does, with a gain of 1/8.
- **Jitter.**
  - On a way being checked: how much the round trip changes from one check to the next (gain 1/4).
  - On the way in use: also the pictures' arrival jitter, using RFC 3550's interarrival estimate
    from the engine's timestamps (gain 1/16).
  - The larger of the two counts.
- **Loss.** Checks not answered within 1 s, out of the last 32.
- **Headroom.** What the way carries divided by what the stream needs (default 15 Mbit/s, the
  engine's 1080p rate). It is measured once at connect by a short burst, and only when the
  burst is long enough not to be TCP slow start.

A reading belongs to the best tier whose every line it is within. Parts that are not known yet
never lower the tier. If the engine skipped pictures for this guest lately (its status
`skipped` went up), the tier is capped:

- one skip in the last 10 s: Playable at most;
- three skips: Poor.

| Tier | Round trip | Jitter | Loss | Headroom | Feels like |
|---|---|---|---|---|---|
| **Excellent** | ≤ 20 ms | ≤ 5 ms | ≤ 0.1 % | ≥ 1.5× | Same as the couch: home network, near Tailscale |
| **Good** | ≤ 40 ms | ≤ 15 ms | ≤ 0.5 % | ≥ 1.25× | Fine for every game; fast shooters feel it a little |
| **Playable** | ≤ 80 ms | ≤ 30 ms | ≤ 1 % | ≥ 1.0× | Slower games are fine, fast ones feel late; lower the stream |
| **Poor** | worse than any of the above | | | | Lag or stutter; another way or a lower stream needed |
| *No answer* | no answer for 3 s, or the socket is gone | | | | Change way now |

The same table is in three places, and a test in each pins the same cases:

- the engine: `join_route.rs` `LIMITS`, test `tiers_match_the_app_and_the_page`;
- the app: `route.cpp` `kLimits`, test `test_tiers`;
- the page: `routeTier.ts` `LIMITS`, test `routeTier.test.ts`.

### Where the numbers come from

Public guidance:

- **ITU-T Y.1541, class 0** (real-time, highly interactive):
  - mean one-way delay ≤ 100 ms;
  - delay variation ≤ 50 ms;
  - loss ≤ 0.1 % (10⁻³).
  - This sets the **0.1 % loss for Excellent**, and puts the **jitter** ceiling well under 50 ms.
  - The values are as summarized in RFC 5976: <https://datatracker.ietf.org/doc/rfc5976>.
  - ITU listing: <https://www.itu.int/rec/T-REC-Y.1541/en>.
- **ITU-T G.114** (2003): below 150 ms one-way, most applications are not significantly
  affected, and highly interactive tasks are affected much earlier. This is why a 75–80 ms
  round trip is the **end of Playable** and not later.
  <https://www.itu.int/dms_pubrec/itu-t/rec/g/T-REC-G.114-200305-I!!SUM-HTM-E.htm>
- **A cloud-gaming service's published requirement:** under 80 ms to its data centre is
  required. This is the **Playable round-trip line**.
  - The same page's bandwidth per setting: 720p60 15 Mbit/s, 1080p60 25 Mbit/s, 4K60 40 Mbit/s.
  - <https://www.nvidia.com/en-us/geforce-now/system-reqs/>
  - A lower "recommended" 40 ms figure is quoted around the web, but it was not on the page we
    read. It agrees with **Good ≤ 40 ms**, but treat it as unverified.
- **Claypool & Claypool**, "Latency and Player Actions in Online Games" (CACM 2006): fast,
  precise games should stay at or under 100 ms; third-person and strategy games tolerate more.
  <https://ftp.cs.wpi.edu/pub/techreports/pdf/06-13.pdf>
- **Cisco's video QoS tutorial:**
  - video loss of about 0.5 %;
  - jitter of 10–50 ms tolerated;
  - interactive video should be treated like voice.
  - Together these support **Good ≤ 0.5 %** and **Playable ≤ 1 %** loss.
  - <https://www.cisco.com/c/en/us/support/docs/quality-of-service-qos/qos-video/212134-Video-Quality-of-Service-QOS-Tutorial.html>
- **Remote Play bandwidth (Sony):** at least 5 Mbit/s, 15 Mbit/s recommended, both ways; wired
  or 5 GHz Wi-Fi. <https://www.playstation.com/en-sg/support/games/ps4-remote-play-connection-troubleshooting>
- **Headroom:** no game-streaming vendor publishes a figure. Streaming guidance says to use no
  more than 70–80 % of the link, which is 1.25–1.4× headroom:
  <https://www.elgato.com/us/en/explorer/news/streaming/how-fast-should-my-internet-be-to-stream/>.
  The engine already lowers the stream to leave 20 % free (`join_net` `FIT_SHARE` = 1/1.2) and
  calls a guest slow below 0.9× of the rate for 5 s (`SLOW_UNDER`, `SLOW_FOR`). So **Good needs
  1.25×** and **Playable 1.0×**.

Our own measurements:

- **The PS5's own share of the delay.**
  - Hardware decode takes 4.3 ms for HEVC and 5.8 ms for H.264. H.264 holds about 3 pictures
    (about 50 ms), HEVC none.
  - Colour conversion takes 2.9 ms; the sound buffer is about 16 ms (`docs/PHASE0.md`).
  - So **HEVC** costs about 10 ms before the network, and the network lines above are for the
    network alone.
- **Tailscale.** Our own Tailscale core measured a direct round trip of 0.5–2 ms on the test bed.
  This is Excellent, the same as the home network.
- **The internet.** A phone on mobile data that joined the Pro across the internet through the
  hub (11.6.0) had a round trip of about 225 ms, which is **Poor**. That matches how it felt.
- **The page's picture word.** The host's panel already calls a page's picture "smooth" at up
  to 150 ms of stalls per 2 s report, "stutters" at up to 600 ms, and "poor" above that.
  - For a guest whose app said nothing, the engine's own reading
    (`join_route::engine_tier`) caps "stutters" like one fall-behind (Playable at most) and
    "poor" like three (Poor).
- **The guest page's colour.** The browser guest page colours its delay green under 60 ms and
  amber under 120 ms (`delayTone`, unchanged). The tier word now stands beside it.

Notes:

- These are app-level round trips over TCP, so a lost packet shows as a round-trip and jitter
  spike (a TCP retransmit), not as "loss". On TCP ways, loss only counts checks that went
  unanswered for a whole second.
- Y.1541 and G.114 talk about one-way delay. Round trip is about twice that, so these lines are
  stricter than theirs on purpose: the picture also waits for decoding and display.

### What the host should stream at, by tier

Remote Play streams at most 1080p, and the PS5 can't change the rate mid-stream.

- **Bitrate.** The engine's default is 15 Mbit/s at 1080p and 10 Mbit/s at 720p. A page may ask
  for anything from 2 to 50 Mbit/s (`rp_stream_api` `auto_bitrate_kbps`, `MIN/MAX_BITRATE_KBPS`).
- **The carry rule.** A way that carries C kbit/s fits a stream of C / 1.25 (`route::fit_kbps`,
  the engine's `fit_kbps` in `link.track`).

| Guest's tier (worst guest) | Suggested stream |
|---|---|
| Excellent | 1080p60, up to 30 Mbit/s (HEVC); more adds little at 1080p |
| Good | 1080p60, 15 Mbit/s (the default) |
| Playable | 1080p60 at what the way carries / 1.25, at least 10 Mbit/s; else 720p60, 10 Mbit/s |
| Poor | 720p60 at 6–8 Mbit/s and HEVC, and tell the guest (a cable, nearer the router) |

For internet guests on the video track, the engine already measures the carry with TWCC and
offers "Lower to N Mbps" (`pt_lower_for_all`). The tiers give the same advice in one word for
every guest, including those on a plain socket.

## 2. The ways

| Kind | Words on screen | How the app dials | Where it comes from |
|---|---|---|---|
| `lan` | Home network | The engine's LAN address | UDP discovery, the helper's mesh, remembered or typed |
| `tailscale` | Tailscale | The helper's forward on `127.0.0.1` (helper `/v1/tailnet/forward`), or a 100.x address the console can reach directly | The helper's mesh (tailnet engines) |
| `internet` | Internet, through your PS5Upload | This home's own PS5Upload engine (`/api/join/bridge`) | An internet invite's room and certificate (see §6) |

- The **shortest kind wins ties**: `lan` before `tailscale` before `internet`.
- A **better tier always beats a shorter kind**. A Poor home network, such as a far Wi-Fi
  extender, loses to an Excellent Tailscale.
- **Every address discovery found for the picked engine** (the same engine id, or the same name
  when an answer has no id) **becomes a way** (`route_candidates`).

**Internet direct (without the bridge) is not built, on purpose.** The guest socket is plain
WebSocket with no TLS. Over the internet it would carry the invite key and resume token in the
clear. A router port for it would also open the engine's listener to the world.

The bridge gives the internet the same protection WebRTC does:

- DTLS, checked against the link's certificate fingerprint;
- peer to peer;
- the hub only for the introduction.

### Why no relay

A relay through the hub would carry every picture. The owner decided (2026-11-27) to keep the
hub to getting people connected only, so nothing relays and nothing is built for it. For the
record, here is why:

- **Fly bills outbound data.** It costs $0.02/GB in North America and Europe and $0.04–0.12/GB
  elsewhere (<https://docs.fly.io/about/pricing>).
- **Each relayed guest is costly.** A 15 Mbit/s stream is about 6.75 GB an hour, so about
  $0.14 an hour for each relayed guest.
- **The free plan would run out fast.** Its $5 a month would cover about 37 guest-hours in all,
  then Pay As You Go would start with **no spending cap** (Fly documents none).
- **The machine would suffer too.** It would also need more than one shared CPU at 256 MB.

A guest whose network can't be crossed directly is told so ("couldn't connect"), as today.

## 3. At connect

`Guest::route_pick`, on the guest thread before the first hello:

1. **Every way that can be checked is checked at once**, one thread each, over the engine's
   checks socket `GET /api/join/probe`:
   - open it (1.5 s at most);
   - 5 timed checks (`{"p":n}` → `{"p":n,"us":…}`);
   - one burst of 256 KiB (`{"burst":n}`), timed into the meter.
2. **The race ends** after `kRaceMs` = 900 ms, when every way has answered, or as soon as a
   home-network way is already Excellent.
3. **The best way is joined.** Best means:
   - the highest tier;
   - then the shortest kind;
   - then the lowest round trip.
4. **The bridge is never raced.** It would cost a hub introduction. It is used only when no
   direct way answered.
5. **An engine without the checks socket still gets a reading.** This is an engine older than
   13.0.0, or one where Play Together is closed: it answers the upgrade with 404 or 409. The time
   that answer took gives one round-trip reading, and the join goes ahead.

The checks sockets of the ways not chosen stay open as warm backups.

## 4. During play

**The way in use** is measured passively:

- the guest socket's own ping every 2 s (`route_sent` / `route_answer`);
- each timed picture's arrival (`route_picture`, arrival jitter);
- any message counts as "heard";
- the status `skipped` counter (`route_status`, falling behind).

**Backups** are checked gently (`Guest::route_main`, every 100 ms):

- **How often:** one check every `kBackupEveryUs` = 2 s per backup.
- **The budget:** all backups together get at most `kBackupBytesPerS` = **256 bytes a second**
  on the wire. That is a token bucket of 3 checks (`Pacer`). A check and its answer are about
  160 bytes; a reopen counts as 600.
- **Never a burst.** The engine refuses bursts after a socket's first 30 s as well.
- **A dark backup** is reopened at most every 30 s.
- **The bridge** is never checked while it is not in use.

**When to change way** (`Selector::decide`):

| Trigger | When | Rules |
|---|---|---|
| **Lost** | The way in use stops answering for 3 s, or its socket closes | Change at once to the best other way (or the bridge if nothing else answers) |
| **Rescue** | The way in use is Poor and a backup is a tier better for 3 s | Ignores the dwell, obeys the budget |
| **Upgrade** | A backup is a tier better for 10 s (for example, coming home to the Wi-Fi from Tailscale) | Only after 20 s on the current way (`kDwellUs`) |

**No flapping:**

- Going back to a way left less than 2 minutes ago needs twice the hold.
- At most 3 changes by choice in 10 minutes (a lost way may always be left).
- Equal tiers never cause a change.
- A change that failed is not tried again at once (`SwitchBackoff`): the next change waits 5 s,
  then 10, 20, 40, 80 s, at most 2 minutes. A change that works starts it over.

The tests cover each rule, including two ways that trade places every 15 s for 30 minutes
(at most 3 changes in each 10 minutes).

**How it changes way** (`Guest::route_tick` and `route_switch_step`, make before break):

1. The route thread opens the guest socket on the new way and sends the hello with the **resume
   token**, the same one a reconnect uses. The guest thread never waits on it: the picture goes
   on through the old way meanwhile.
2. The engine hands the seat to the new socket: one socket per token. The old socket is told
   `{"bye":"replaced"}`. The pad, the console's controller slot and the player number stay.
3. On `joined`, the app takes the new connection over (`WsClient::adopt`), with anything already
   read. It closes the old socket and asks for a keyframe at once.
4. The picture decoder starts again at the new way's first keyframe, which is a **keyframe
   boundary**. Until then the last picture stays on screen (up to 1.5 s), and there is no
   "Connection lost" screen.
5. A change that fails (no `joined` within 3 s) leaves the old way in use. If the engine had
   already handed the seat over (the old socket heard `replaced` during the change, or within 5 s
   after it failed), the normal resume path takes over with the token; a change of way never ends
   the game.

**After a drop**, the reconnect goes to the best other way first (`route_lost`), not the same
address again. Every third try goes back to the same way.

### What the person sees

- **While playing, top right:**
  - "Home network · Excellent · 3 ms" for 6 s after joining;
  - "Now playing over Tailscale" for 6 s after a change;
  - "Slow connection: a cable or being nearer the router helps" for as long as the way is Poor.
  - The dot is green for Good or better, amber for Playable and red for Poor.
- **On the host screen** in the app, each guest's row says "Tailscale · Good".
- **In PS5Upload's Play Together panel**, each guest has a tier word next to their delay. The
  browser guest page shows "Home network · Good · 12 ms".

## 5. What the hub costs

The hub is signalling and rendezvous only: small JSON messages, short-lived sockets, one small
machine on the free plan. Nothing in this design uses it during play: no checks, no stats, no
media.

| Who | Messages | Bytes | Time connected |
|---|---|---|---|
| Bridge join, measured in the bridge e2e test (10 runs, loopback) | 4 out (join, offer, 1 candidate, leave), 2 in (joined, answer) | ≈ 1.1 KB out, 0.8 KB in | 92–105 ms; across the internet it is the ICE time, ≤ 15 s (the bridge gives up then) |
| Browser guest join | join, offer, 5–11 candidates out; joined, answer, a few candidates in; leave | 2–6 KB | Until the data channel opens (seconds), at most 3 min (the hub removes it) |
| Host (engine) | host, hosted; then one signal per guest message; a WebSocket ping every 20 s | Tens of bytes a minute while idle | While an internet invite is open |
| Changing way, a backup check, stats | none | 0 | 0 |
| The app itself | only easy join's waiting room (EASY_JOIN.md, "Hub load per join"); nothing more for an internet invite | 0 more | 0 more |

Per game session with one internet guest, that comes to about 10–20 messages, a few kilobytes
and a few seconds of a guest socket.

The hub now counts what is needed to watch this, as counts only:

- `hub_joins_app_total`: joins from an app's own client, which is the bridge;
- `hub_guests_left_total` and `hub_guest_connected_ms_total`: average guest time per join.

Limits already in place (PROTOCOL.md):

- per-room and per-client signal rates;
- guests removed 3 minutes after joining;
- hostless rooms removed after 2 minutes;
- idle sockets closed after 60 s;
- 10 s to say `host` or `join`.

## 6. Easy join's internet invites go through the bridge

**One hub connection.** The app's only connection to the hub is easy join's waiting room
(`net::HubLink`, `app/waiting_room.cpp`): its room for the phone's "Play on my PS5" and a host's
"Send invite". Playing an internet invite never opens another one: this home's PS5Upload talks to
the hub for the bridge.

**Where the invites come from.** An invite reaches the app three ways:

- the phone hand-off (the TV's QR code, then "Play on my PS5");
- a host's push ("Send invite to <friend>'s PS5");
- Play again with a remembered host.

**When an invite is internet-only** (none of its direct routes answers), the app joins it through
the bridge, exactly as below (`App::join_invite`, `App::join_again` in `app/easy.cpp`, with the
pure parts in `app/route_pick.cpp`):

```cpp
#include "app/route_pick.hpp"
char own[64]; std::uint16_t own_port;
const bool can_bridge = inv.room[0] && own_engine(found, n_found, own, sizeof own, &own_port);
route::Candidate ways[route::Selector::kMax];
// The invite's direct route (the waiting room kept the one that answered), then
// route_bridge(own, own_port, inv.room, inv.fp) last: used only when no direct way answers.
int n = invite_ways(inv, can_bridge ? own : nullptr, own_port, ways, route::Selector::kMax);
guest->set_routes(ways, n);   // the app adds discovery's other addresses for the same engine
guest->start(direct ? route_addr : own, direct ? route_port : own_port, "", name, version);
```

- **`room` and `fp`** are the internet link's `#room=…&fp=…` (`invite::Invite::room`, `fp`).
  The invite key `k` is still sent in the hello, inside the bridge's DTLS channel.
- **`own_engine`** is PS5Upload on the guest's own network: a UDP discovery answer, or an engine
  from the helper's mesh that is on this network (not over Tailscale). One that says 13.0.0 or
  later is preferred. An older one is still tried: it answers `/api/join/bridge` with 404, and
  the app says to update it.
- **The waiting room says yes** to an internet-only hand-off when this home has a PS5Upload
  (`WaitingRoom::set_bridge`), so the phone hears at once that the PS5 will play it.
- **No PS5Upload on this network:** the waiting room answers `invite_ok: false, why: route`. The TV
  then says what is needed (`inv_bridge_need`): "To play Alex's game over the internet, this PS5
  needs PS5Upload 13.0.0 or later open on a computer on your home network, with "PS5Upload Play
  can find this computer" on in PS5Upload › Settings › App › Play Together." It never fails
  quietly.
- **Bridge failures** arrive in the log as `failed (<why>)`. The reasons are `hub`, `no_room`,
  `room_full`, `no_direct`, `mismatch`, `bad`, `offer` and `busy` (join_net/bridge.rs). The app
  adds `old` (404) and `unreachable`. A join that fails there ends as `failed`, with why
  `bridge_<why>`, and a plain message:

| Why | What the TV says |
|---|---|
| `old` | That PS5Upload is too old to play over the internet: update it to 13.0.0 or later |
| `unreachable` | Couldn't reach that PS5Upload: is it open? |
| `hub` | Your PS5Upload couldn't reach the Play Together server |
| `no_room`, `mismatch` | Your friend's internet invite isn't open any more: ask for a new one |
| `room_full` | The game is full |
| `no_direct` | The two networks couldn't connect over the internet: try again, or use Tailscale |
| `busy` | PS5Upload is busy |
| any other | Your PS5Upload couldn't open the internet invite (`<why>`) |

- **Each way joins with the invite.** The engine's address list from discovery and the bridge go
  together: a guest on the host's Wi-Fi or tailnet then plays directly and keeps the bridge only
  as a last resort.

**Play again over the bridge.** The engine gives no friend pass in an internet room, and takes
none there (join.rs: passes are for the home network and Tailscale). So a remembered host also
keeps the last internet invite the app joined it with (`friends.txt`, a `link=` line: room,
certificate, key). That invite can come from the phone, a push, or the one link's `lan`/`ts`
pages naming that host.

Play again then uses:

- the host's own address (through the helper for Tailscale);
- the bridge with that room and certificate, last (`again_ways`).

The hello carries the pass on every way. On the bridge way only, it also carries the kept
invite's key (`JoinExtras::bridge_invite`), because the key is what asks there.

This works while that invite is open: rooms and certificates change with each invite and engine
run. An ended one says so plainly: `no_room` or `mismatch` from the bridge, or a `key` refusal,
after which the app lets the kept invite go and keeps the pass. A Tailscale host whose helper
forward fails goes straight to the bridge when it has one.

## 7. Diagnosis

The owner's rule: logs, API and a read-only MCP tool.

- **The app's log** (`/data/ps5upload-play/…`):
  - `route: picked <kind> after N ms; lan excellent rtt 2 jitter 0 head 900 …`, with every way's
    reading at connect;
  - `route: upgrade|rescue|lost from X to Y`;
  - `route: now on Y (why) in N ms`;
  - `route: through this home's PS5Upload: open|failed (why)`, or `… at ADDR:PORT: failed (old|unreachable, http N)`;
  - `room: an invite (…, over the internet, through this home's PS5Upload)` or `(…, an internet invite, and no PS5Upload on this network to play it through)`;
  - `app: joining ADDR:PORT (invite; N way(s), this home's PS5Upload as the last | only through this home's PS5Upload)`;
  - `app: kept this internet invite for Play again`, `app: the host's internet invite has ended: let it go`.
- **The engine's log:**
  - `join: Player N on <console> plays over <way> (<tier>, K backup(s))`;
  - `… changed way: a -> b (<tier>, n change(s) so far)`.
- **API:**
  - `GET /api/join?addr=` shows each guest's `route`, `tier` and `tier_from` (`guest` or `engine`),
    and `page.route_*`.
  - `GET /api/join/routes?addr=` gives the tier lines, each guest's way, tier, measured round
    trip, jitter and loss, changes and backups, and the checks socket's counts.
- **MCP:**
  - `together_routes` (read-only) returns the document above;
  - `together_status` carries `route` and `tier`.
- **Hub `/metrics`:** the counters in §5.

## 8. Tests

- **Unit tests** (`make -C apps/play unit`, `test_route`):
  - the tier table;
  - the meter: round trip, jitter (from checks and from pictures), loss, down after 3 s, fresh
    after coming back, burst with slow start;
  - choice at connect;
  - each switch rule;
  - no flap over 30 minutes;
  - the pacer (never over 256 B/s for any number of backups).
- **App e2e** (`make -C apps/play e2e`, §11, stand-in engine with ways that add round trip or go
  dark):
  - the better tier wins at connect, and the host hears way, tier and backup;
  - backups are checked gently (≤ 16 checks in 8 s, race included);
  - bursts happen only at connect;
  - a way that goes dark mid-game is left for the backup with the resume token beside the old
    socket: no reconnect screen, and pictures go on;
  - the bridge is used when nothing direct answers, and a failed bridge says why;
  - an engine without the checks socket still joins.
- **Easy join through the bridge** (`make -C apps/play e2e`, §12; the stand-in is the hub, this
  home's PS5Upload and the host's engine, and takes no pass through a bridge, as join.rs does):
  - the phone hands over an internet-only invite: the waiting room says yes, and the app joins
    through the bridge with the invite's room and certificate, asking with its key;
  - with no PS5Upload on this network the phone hears no, and the app says what it needs;
  - Play again over the bridge (the host's address dark): the kept room, certificate and key, with
    the pass; Play again whose host answers sends the pass alone, with no key and no bridge;
  - a kept invite that ended fails with the bridge's reason (`bridge_no_room`), or is refused
    (`key`);
  - a join with a friend pass changes way mid-game: resumed with the token, no reconnect screen,
    no refusal, no second pass.
- **Unit tests** (`test_core`): an invite's room, certificate and the host's first page; a host's
  kept invite read back from `friends.txt`.
- **Engine tests:**
  - `join_route` (the table, the engine's reading, checks socket limits and bursts, the routes
    document);
  - `a_report_says_its_way_and_tier_and_a_change_is_seen_once`;
  - `join_net::bridge` (an e2e with the real hub, the engine's answerer and a stand-in app).
- **Hub:** `joins_from_apps_and_guest_time_are_counted`.
- **Page:** `routeTier.test.ts`, `session.test.ts` (`withRoute`), `playTogetherState.test.ts`.

## 9. Not done, and why

- **The browser page doesn't change way mid-game by itself.** Its WebRTC connection already
  picks the shortest pair ICE has, and reconnects race the ways again (pathRace). Moving a live
  page from the plain socket to WebRTC would need the same make-before-break as the app; that is
  a small follow-up if wanted.
- **IPv6 for the app's own ways.** The PS5 side is IPv4 only today (`net::resolve`).
- **System Tailscale on the PS5 itself doesn't exist.** A `tailscale` way dialled directly only
  works if the guest's router routes 100.x addresses. The helper's forward is the normal path.
