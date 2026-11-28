# Easy join and host (PS5Upload Play)

Goal (owner, 2026-11-27): a friend's PS5 joins a game, and a host invites, with as little typing
as possible. Principles: no typing in the common cases; at most a 6-character code otherwise; the
host always approves (TV, phone or computer); works without the helper where it can; nothing ever
listens on a PS5 (the app only dials out).

Press counts start with PS5Upload Play open on its home screen ("✕" = one press of a button,
"move" = one d-pad step). Opening the app from the PS5 home screen is one more ✕ in every row.

## Every way a guest PS5 and a host can meet

| # | Way | Guest steps (presses / typed characters) | Needs | Notes |
|---|---|---|---|---|
| 1 | **Same home network, nearby list** | the host's game is already on the app's home screen: ✕ (1 press, 0 typed) | host's engine answers the UDP probe (`play_discovery`, on by default); Play Together open | the host approves; no code. Before this change: Join › pick computer › "ask without a code" (3 presses + reading). |
| 2 | **Tailscale, helper mesh** | same as 1 once the helper lists the engine (1 press) | helper running on the guest PS5 (its forward), host on system Tailscale | the helper's mesh says nothing about Play Together being open, so these engines show in "Join a friend's game" (2 presses), not as a ready-made row. |
| 3 | **Play again (remembered friend)** | "Play again with Alex": ✕ (1 press) | the guest joined that host before from this app (it keeps the host's friend pass); the host's engine reachable (LAN, or Tailscale through the helper) | works even when Play Together is closed: the engine opens it for the console being played when the host says yes. The host still approves every time. |
| 4 | **Phone-assisted, TV-login pattern ("Play on my PS5")** | the app's home screen shows a QR code. Scan it with the phone (0 presses on the PS5), tap "Play on my PS5" on the phone (1 tap). The app starts joining by itself. | the app reaches the hub over TLS (`wss://hub.phantomptr.com/ws`, BearSSL in the app); the invite must be playable from this PS5 (see "What the app can play") | the phone page keeps the last invite link opened on it (30 min), so the order doesn't matter: open the friend's link, then scan, or scan, then open the link. |
| 5 | **Host pushes to a remembered friend's PS5** | the friend's TV shows "Alex invites you to play": ✕ (1 press) | the friend's app open (its waiting room is on the hub only while it runs); the host's PS5Upload has that friend's PS5 from an earlier game | "Send invite to <friend>'s PS5" in PS5Upload › Play › Play Together. The friend's app asks before joining: a push is not the person in front of the TV. |
| 6 | **Invite link or QR on the host's TV, scanned by a phone** | phone joins in its browser (already existed) | — | the host app's Host screen already shows the invite code and QR; the guest page now also offers "Play on my PS5" (way 4). |
| 7 | **Short code** | Join a friend's game › computer (✕) › "Enter a code" (✕) › 6 characters on the code keyboard (≈ 4 moves + ✕ each, ≈ 30 presses) | the host's engine reachable | the fallback. The keyboard only has the code's 31 characters in a big 8 × 4 grid and jumps to Join after the 6th. |
| 8 | **Typed engine address** | address keypad (12–21 presses) | — | last resort; kept. |
| 9 | **Guest's own PS5Upload pushes to its PS5** ("Play on my PS5" in the guest's desktop app) | 1 click on the computer | the guest's engine would need the app's waiting-room id | not built: the phone page covers the same need (the link usually arrives on the phone). Later: the app can tell nearby engines its room. |
| 10 | **Internet invite played by the app itself** | — | WebRTC (or another internet route) in the app | **not in this branch**: the play-routes branch adds the native internet route. Here the app plays an internet link through its `lan` / `ts` hints and says plainly when it can't. |

### What each needs and what crosses the network

| Way | Helper | Hub | TLS in the app | Engine change |
|---|---|---|---|---|
| 1 Nearby list | no | no | no | discovery answer says `together` (how many Play Togethers are open) |
| 2 Tailscale | yes (forward) | no | no | none |
| 3 Play again | no (yes over Tailscale) | no | no | friend passes (`remember` / `again` in the hello) |
| 4 Phone | no | yes (the app hosts a room) | **yes** | none (hub protocol v1 rooms as they are) |
| 5 Host push | no | yes | yes | `POST /api/join/friends/send` (engine joins the friend's room as a guest) |
| 7 Code | no | no | no | none |

## Chosen set

1. **Nearby games on the home screen** (way 1): one press. The app probes the network while the
   home screen is up; an engine whose answer says `together > 0` becomes a row "Join <name>'s
   game" at the top, selected. ✕ asks the host (a "nearby" hello: no code needed when exactly one
   Play Together is open there; the host decides).
2. **Play again** (way 3): one press, also when Play Together is closed (the host is still asked).
3. **Phone-assisted "Play on my PS5"** (way 4): zero presses on the PS5, one tap on the phone.
4. **Host pushes an invite to a remembered friend's PS5** (way 5): one click for the host, one
   press for the friend.
5. **Code fallback** (way 7): kept as it is.

Why not the others: a QR the host's TV shows and the friend's *PS5* reads is impossible (no camera
on the PS5); the guest's own desktop pushing to its PS5 (9) needs the app's room id first and the
phone covers it; internet play (10) belongs to the play-routes work.

### What the app can play from an invite it was handed

An invite link is `http://<engine>:<port>/join#i=<key>` (the home network's) or the one link for
everywhere, `https://ps5upload.phantomptr.com/join/#room=…&k=<key>&fp=…&hub=…[&lan=<page>][&ts=<page>]`.
The app tries, in order: the home network's page (`lan`, or the link itself) with the key on
`/api/join/ws`; the Tailscale page (`ts`) through the helper's forward; else it says the invite
needs the host on the same network or on Tailscale (the play-routes branch replaces this with its
internet route: `App::join_link` is the one place to change).

## Flows (as built)

**Same network.** Host: PS5Upload › Play › Play Together › Open (or the host app). Guest: open
PS5Upload Play → "Join Alex's game" is first → ✕ → "Waiting for your friend to let you in" →
the host sees the ask in PS5Upload, PS5Upload Play and as a TV notification → Let in → playing.

**Play again.** After any game joined from the app, the host is remembered with its friend pass
(Settings › "Forget remembered computers" forgets them too). Next time the home screen shows
"Play again with Alex" (with the address it was played at); ✕ sends `{"hello": {"again": pass}}`.
The engine finds the console it was for: Play Together open → a normal ask; closed but the console
is being played → it opens Play Together, asks, and closes it again if the host says no; not being
played → "Your friend isn't playing right now".

**Phone.** The app's home screen shows a QR code (right side) "Got an invite on your phone? Scan
this, then tap Play on my PS5". The QR is `https://ps5upload.phantomptr.com/join/#ps5=<room>&t=<nonce>&hub=<hub>`.
The phone page: with an invite opened in the last 30 minutes on that phone it shows "Play
<host>'s game on your PS5" (one tap); otherwise "Open your friend's invite link on this phone",
and the invite page later offers "Play on my PS5" while the scan is fresh (10 minutes). The page
joins the app's room on the hub and signals the link; the app answers and starts joining.

**Host push.** Each time a guest app joins with `remember`, it also gives its waiting room
(`inbox: {room, hub}`). PS5Upload › Play › Play Together lists "Friends' PS5s"; "Send invite"
joins that room on the hub and sends the current invite (the one link). The friend's app shows
"Alex invites you to play" with ✕ Join / ○ Not now.

## Interfaces

### App: hub client (`src/net/hub.hpp`)

A small JSON-over-WebSocket client for the hub, TLS by BearSSL (`src/net/tls.hpp`), reusable by
other work (the play-routes signalling):

```cpp
namespace play::net {
class HubLink {
  public:
    // url: "wss://hub.phantomptr.com/ws" (TLS, the ISRG roots) or "ws://host:port/ws" (tests).
    bool open(const char *url, unsigned timeout_ms) noexcept;
    void close() noexcept;
    bool is_open() const noexcept;
    bool send_json(const char *text) noexcept;              // one text frame
    enum class Got { message, timeout, closed };
    Got next(unsigned timeout_ms) noexcept;                  // answers pings itself
    const char *text() const noexcept; std::size_t size() const noexcept; // the last message
    const char *error() const noexcept;
};
}
```

`src/app/waiting_room.hpp` builds on it: the app's own room (hub protocol v1 `host` with a bound
room id from a secret kept in `/data/ps5upload-play/room.txt`), the hand-off messages below, and
reconnects with backoff.

### Hand-off message (in a hub `signal`'s `data`, guest → the app's room)

```json
{"invite": {"link": "<join link>", "name": "<host name, optional>", "t": "<nonce, phone only>"}}
```

Answer (app → that guest): `{"invite_ok": true}` or `{"invite_ok": false, "why": "link" | "busy"}`.
`t` equal to the nonce in the QR on the TV means "the person in front of this TV sent it": the app
joins at once. Without it (a host's push) the app asks first. Data is capped at 2 KiB by the app
(the hub caps it at 12 KiB); a link that is not a Play Together link is answered `link`.

### Engine

* Discovery answer (UDP 19261): `"together": n` (Play Togethers open here whose host is playing).
* Guest hello: `"remember": true` → `"joined"` carries `"friend": "<pass>"` (43 base64url
  characters, kept by the engine only as SHA-256 in `play_together_friends.json`, mode 0600, at
  most 64, forgotten after 180 days unused); `"inbox": {"room", "hub"}` stored with it;
  `"again": "<pass>"` asks again (one ask per pass every 20 s at most, else `slow_down`).
* Host API (this computer / `PS5UPLOAD_ALLOW_IP` only): `GET /api/join/friends`, `POST
  /api/join/friends/forget {id}`, `POST /api/join/friends/send {id, addr}`.

## Security

* **Nothing listens on a PS5.** The app dials out to the engine and to the hub; its waiting room
  is a hub room it hosts.
* **Who can push what to whom.** The phone page and a host's engine can put an invite *link* in
  front of the app (no code runs, nothing is installed, no setting changes). A pushed invite needs
  ✕ on the TV unless it carries the nonce only the TV shows (QR); the nonce changes every app start.
  The worst a stranger who learned a room id can do is show "X invites you" on that TV, which the
  person can ignore; the hub's join and signal limits (30 joins, 60 signals a minute) bound it.
* **The host always approves.** "again" and "nearby" are asks; the host's Let in is required.
  "again" while Play Together is closed may open it, only for the console being played, and closes
  it again when the host says no. The TV notification is the existing one (6 a minute per console,
  one per address every 30 s).
* **Friend passes.** 256-bit random, only their SHA-256 kept, never logged; given only to a guest
  the host let in that asked for one; bound to one console; forgettable (`forget`, Settings in the
  app). A stolen pass gets an ask, never a seat.
* **Invites expire.** Play Together invites keep their 30-minute life; the phone page keeps a link
  30 minutes and a scanned PS5 10 minutes; the app drops a pending push after 5 minutes.
* **TLS.** The app checks the hub's certificate against the ISRG roots (Let's Encrypt) and the
  name; the link it receives is checked by the app (only `http(s)` Play Together links, private or
  Tailscale addresses for the direct routes). An invite's `k` key is no more powerful than
  before: it only lets a guest ask.
* **No new unauthenticated host powers.** The new engine routes are host-only; the discovery
  answer adds one number, stays no larger than the probe and only answers home networks.

## Touchpoints (one press to the next step)

| Touchpoint | Next step offered | State |
|---|---|---|
| App home screen | "Join <host>'s game", "Play again with <host>", QR for the phone | built |
| Guest join page (phone) | "Play on my PS5" (with the TV's QR scanned, or a hint how) | built |
| App host screen | invite code + QR (phones join in the browser or send to their PS5) | existed |
| Desktop Play Together | "Friends' PS5s" › Send invite | built |
| Helper TV notification for an ask | says "Answer in PS5Upload or PS5Upload Play" | existed |
| Guest join page: PS5Upload Play not on that PS5 | "Install PS5Upload Play" via the friend's PS5Upload or Minion | later (guidance pass) |
| Minion | "Open PS5Upload Play" / "Join a friend" | later (guidance pass) |
| "What's new" / discovery copy | — | later (separate pass) |

## Hub

No hub change is needed: the waiting room and the hand-off use protocol v1 rooms and signals as
they are. A 6-character code for the phone (typing instead of scanning) would need a hub change
(a short alias for a room); it is not built: the QR is the way in, and the phone page can take a
pasted link.
