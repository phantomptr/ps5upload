# Remote Play automation (local scripts and agents)

A script or AI agent on the same computer as the PS5Upload engine can start, watch and play a
Remote Play session, with or without the app showing it:

1. **start** (or use the session the app already runs);
2. **status**: which console is streaming, and its session id;
3. **frame**: the picture right now, as a PNG or JPEG;
4. **input**: hold a controller state (buttons, sticks, triggers) for a short time, then let go;
5. **frame** again, to see what happened.

Added in 10.6.0 for [#615](https://github.com/phantomptr/ps5upload/issues/615).

> **Not a stable API yet.** These endpoints can change between versions (see
> [#430](https://github.com/phantomptr/ps5upload/issues/430)). Check the engine version
> (`GET /api/version`) and pin your script to it.

## What it does, and what it never does

- **This computer only.** The frame and input endpoints and the live feed answer only to the
  engine's own computer (a loopback address: `127.0.0.1` or `::1`). `PS5UPLOAD_ALLOW_IP` alone does
  **not** open them to other machines: a listed machine gets `403 {"error": "local_only"}`, and any
  other machine is refused by the engine's guard with a `403` before that. The status endpoint is
  also open to listed machines, without the session id (see below).
- **Address it as localhost.** Requests must use `localhost`, `127.0.0.1` or `[::1]` as the host
  name (any port); any other name answers `403 {"error": "host_not_local"}`.
- **A self-hosted engine can open it.** Set `PS5UPLOAD_ALLOW_AUTOMATION=1` to let the addresses in
  `PS5UPLOAD_ALLOW_IP` use everything on this page. They may then name the engine by an IP address,
  or by a name listed in `PS5UPLOAD_ALLOW_HOST`. There is no password: only do this on a network
  you trust, since it lets those computers play on your console.
- **It never takes the picture over.** When an app page shows the stream, a frame request asks it
  for the picture already on its screen; nothing on screen changes, no second stream runs.
  Watch-party viewers and phones are never asked. With no page, the picture is decoded from the
  stream with the ffmpeg that Go live uses.
- **It is one more controller.** Input is merged with yours, like a phone joining as a
  controller: you can still play, and letting go of the script's buttons never lets go of yours.
- **It never sends anything twice.** A refused or failed request is not retried by the engine.

The engine listens on `http://127.0.0.1:19113` by default. If that port is taken, the desktop app
picks another and its log says which. A standalone engine takes the port from
`PS5UPLOAD_ENGINE_PORT`.

## Endpoints

### `GET /api/ps5/rp/session`: status

```json
{"host": "192.168.1.100", "session": 3, "playing": true, "attached": true, "passcode": false}
```

Asking here counts as use of a session started through this API, so polling keeps it past
`keep_alive_s`. Add `?passive=1` to look without that.

`host: null` (and `session: null`) means nothing is streaming; `last_end` then says how the last
session ended (its `code` can be `busy`, `asleep`, `keys`, `rp_off`, and so on). `attached` is whether
an app page shows the stream now. `picture` is false while the picture is off on that page (see
below; a frame needs it on). `session` changes every time a new stream starts; pass it to the
other endpoints so a script never drives a session it did not mean to.

The answer also has `stage` (how far the start got), `started_by` (`api` or `page`),
`passcode_retry`, `mic_enabled`, `mic_on`, `keyboard_enabled`, `keyboard` (`{open, text}`),
`keep_alive_s`, `alone_s` and `video` (codec, size, frame rate, bitrate).

### `POST /api/ps5/rp/start`: start a stream

```json
{"host": "192.168.1.100", "width": 1280, "height": 720, "fps": 60, "wait_s": 20}
```

| Field | Meaning |
| --- | --- |
| `host` | The console: its address, or its id or name on the list (`GET /api/consoles`). |
| `width`, `height`, `fps`, `bitrate_kbps` | As in the app. Anything above 1080p is asked for as 1080p. |
| `codec` | `h264`, `h265` or `h265_hdr`. |
| `keep_alive_s` | With no page attached, how long it runs after the last call here (default 300, at most 600). A reader of the live feed or Go live counts as use. |
| `mic`, `keyboard`, `dualsense` | Set the session up for those. |
| `replay_s` | Keep this many seconds for Instant Replay. |
| `wait_s` | Answer once the first picture came, the console asks for the passcode, or the start failed (at most 60). Without it the answer comes at once. |
| `regist_key`, `rp_key` | The keys, if the engine does not have them. |

The keys come from pairing: `POST /api/ps5/power/pair` stores them, and so does the app when you
pair there. Without keys the answer is `409 no_credentials`. A session for the same console is
joined rather than started (`"started": false`); a session for another console answers
`409 busy_other`.

```json
{"ok": true, "started": true, "host": "192.168.1.100", "session": 4, "keep_alive_s": 300,
 "playing": true, "passcode": false}
```

### `POST /api/ps5/rp/stop`: end it

`{"host": "192.168.1.100", "session": 4}`. The console holds its Remote Play slot for 12 to 30 s
after a session ends; a start in that time fails with `last_end.code` `busy` and `retry_s`. A start
right after a stop waits up to 5 s for the old session to go, then starts a new one, or answers
`409 busy` with `retry_s`. It never joins the session you stopped.

### `POST /api/ps5/rp/settings`: change it while it runs

`{"host", "session", "keyframe": true, "replay_s": 30}`. `keyframe` asks the console for one;
`replay_s` turns Instant Replay on (0 turns it off). Size, frame rate, bitrate and codec can only be
set when a session starts: asking for one answers `409 restart_needed` with `fields`.
`"picture": false` turns the picture off on the page, as below.

### `POST /api/ps5/rp/picture`: the picture on this device

```json
{"host": "192.168.1.100", "session": 3, "on": false}
```

Off, the page gets no picture or sound, sends no controller and runs no decoder, for this session.
Play Together guests, phones, the live feed (Go live, OBS) and instant replay carry on as before,
and while Go live runs, friends are in or an invite is out, the session no longer needs a page at
all. `on: true` brings it back without a restart: the page gets the codec headers again and the next
keyframe. `session` is optional. Answers `{"ok": true, "session": 3, "picture": false}`, or 404
`no_session`. While it is off, `GET /api/ps5/rp/frame` answers 409 `picture_off`.

### `POST /api/ps5/rp/passcode`: the sign-in passcode

When the status says `"passcode": true`: `{"host", "session", "digits": "1234"}`, or
`{"host", "session", "cancel": true}`. It goes to the console and is never kept or logged.

### `POST /api/ps5/rp/keyboard`: the console's on-screen keyboard

For a session started with `"keyboard": true`, while the keyboard is open (see the status):
`{"host", "session", "text": "the whole text"}`, `{"accept": true}` or `{"reject": true}` along
with `host` and `session`.

### `POST /api/ps5/rp/mic`: the microphone

For a session started with `"mic": true`: `{"host", "session", "on": true}`. With no page attached,
`"opus_b64": ["...", ...]` sends Opus packets (48 kHz stereo, 10 ms each).

### `PUT /api/ps5/vinput`: the console's keyboard and mouse, without a stream

`{"host", "on": true, "mouse": true, "keyboard": true, "lease_s": 30}` makes the console see a
keyboard and mouse. The answer says what it sees (`mouse`, `keyboard`) and why not (`err`). It lasts
`lease_s` (at most 300) from the last call; `{"host", "on": false}` ends it, and a Remote Play page
showing that console takes over (`409 rp_session_owns_input`). `GET /api/ps5/vinput?host=` says where
it stands.

### `POST /api/ps5/vinput/report`: press, release, move

`{"host", "report_hex": "<32 hex characters>"}` or `"reports_hex": [...]` (at most 256). A report
is the whole state at that moment, 16 bytes: mouse buttons (1 left, 2 right, 4 middle), the HID
modifier byte, mouse motion x and y (each a little-endian i16), wheel, horizontal wheel, then up to
8 HID usage IDs held. Send an all-zero report to let go. Without `PUT` first: `409 not_on`.

There is no error for a request that isn't local here: `session` is simply left out when the request
does not use a localhost name, or comes from another machine. If `host` is set and `session` is
missing, address the engine as `127.0.0.1`.

### `GET /api/ps5/rp/frame`: the current picture

| Query | Meaning |
| --- | --- |
| `host` | The console, as the session shows it. Required: without it the answer is `404 no_session`. `addr` and `console` mean the same. |
| `session` | The session id (recommended). A different one answers `409 stale_session`. |
| `format` | `png` (default) or `jpeg`. |
| `max_width` | Scale down to at most this many pixels wide (16 to 7680), keeping the shape. |
| `after` | A `frame_id` from an earlier answer: wait up to 1.5 s for a newer picture. |
| `json` | `1`: answer with JSON (image in base64) instead of the image itself. |

The answer is the image (`Content-Type: image/png` or `image/jpeg`) with headers:

| Header | Meaning |
| --- | --- |
| `X-Frame-Host`, `X-Frame-Session` | The console and session it came from. |
| `X-Frame-Width`, `X-Frame-Height` | The image's size. |
| `X-Frame-Id` | This picture, for a later `after=`. |
| `X-Frame-Captured-At` | When the page read it (ms since 1970, the page's clock). |
| `X-Frame-At` | When the page drew that picture (same clock). |
| `X-Frame-Age-Ms` | How old the picture was when read. A large age means the stream is stalled or the app window is hidden. |

With `json=1`:

```json
{
  "host": "192.168.1.100", "session": 3, "format": "png", "width": 1280, "height": 720,
  "captured_at_ms": 1793562413139, "frame_at_ms": 1793562413123, "frame_age_ms": 16,
  "frame_id": "2.4711", "engine_at_ms": 1793562413150, "image_base64": "iVBORw0KGgo..."
}
```

At most one frame request runs at a time, and at most five a second.

With no app page attached, or a page that gives no picture (a hidden window), the picture is the
stream's next keyframe, decoded with the ffmpeg Go live uses: at most one a second, since each makes
the console send a keyframe, and its `frame_id` starts with `0.`. It waits about a second for the
keyframe. HDR is not tone mapped, so use `h264` or `h265` for this. Without ffmpeg the answer is
`409 no_viewer` with a `message`; install it with `POST /api/stream/tools/ffmpeg/install`, or in the
app under Go live.

### `POST /api/ps5/rp/input`: hold a controller state

```json
{"host": "192.168.1.100", "session": 3, "buttons": ["right"], "duration_ms": 150}
```

| Field | Meaning |
| --- | --- |
| `host`, `session` | Required. `addr` and `console` also name the console. A missing or wrong console is `404 no_session`, a missing `session` is `400 bad_request`, and a stale session is refused (`409`). In each case nothing is sent. |
| `buttons` | Any of `cross`, `circle`, `square`, `triangle`, `up`, `down`, `left`, `right`, `l1`, `r1`, `l3`, `r3`, `options`, `create`, `touchpad`, `ps`. |
| `left`, `right` | Sticks as `[x, y]`, each from -1 to 1 (right and down are positive). |
| `l2`, `r2` | Triggers, 0 to 1. |
| `duration_ms` | How long to hold it, 1 to 5000 (default 100). Then it is let go by itself. |

The answer comes at once, when the state is handed to the stream:

```json
{"ok": true, "host": "192.168.1.100", "session": 3, "hold": 12,
 "sent_at_ms": 1793562413200, "release_at_ms": 1793562413350}
```

It says the input was sent, not that the game reacted: take a newer frame to check. A newer input
replaces the held one (its own timer then decides the release).

### `POST /api/ps5/rp/input/release`: let go now

```json
{"host": "192.168.1.100", "session": 3}
```

Lets go of everything the script holds. Your own controller is not touched. When the stream ends,
everything is let go with it.

## Errors

Every error is JSON. The endpoints' own refusals are `{"error": "<code>"}`:

| Status | `error` | Meaning |
| --- | --- | --- |
| 400 | `bad_request` | A field is missing or out of range (`field` or `button` says which). |
| 403 | `local_only` | Not from this computer. |
| 403 | `host_not_local` | The request used a name other than `localhost`, `127.0.0.1` or `[::1]`. |
| 404 | `no_session` | Nothing is streaming from that console. |
| 409 | `stale_session` | Another session runs now (`session` gives its id). |
| 409 | `no_viewer` | The stream runs, no app page shows it, and ffmpeg is not installed (`message` says what to do), or the page left while answering. |
| 409 | `no_credentials` | Start: the engine has no keys for this console. Pair it first. |
| 409 | `busy_other` | Start: another console is streaming (`host` says which). |
| 409 | `restart_needed` | Settings: `fields` can only be set when a session starts. |
| 409 | `no_passcode_asked` | The console is not asking for a passcode. |
| 409 | `keyboard_not_enabled` / `keyboard_closed` | Keyboard: not set up for this session / not open. |
| 409 | `mic_not_enabled` | Mic: not set up for this session. |
| 409 | `not_on` | Report: turn input on first. |
| 409 | `rp_session_owns_input` | A Remote Play page owns this console's keyboard and mouse. |
| 502 | `start_failed` | Start with `wait_s`: the session ended first (`end` says how). |
| 502 | `decode_failed` | ffmpeg could not make the picture (`detail`). |
| 429 | `busy` / `rate_limited` | A frame request is already running / too soon (`retry_ms`). |
| 503 | `no_picture` | The page has not shown a picture yet. |
| 503 | `viewer_busy` | The page's connection is full; try again shortly. |
| 504 | `timeout` | The page did not answer within 2 s. |
| 504 | `no_newer_frame` | No picture newer than `after` came within 1.5 s. |
| 502 | `page_error` | The page could not make the image. |
| 500 | `internal` | The engine could not build the answer. |

A request refused before it reaches these endpoints has text in `error` and the code in `code`:
`{"error": "<text>", "code": "<status name>"}`. That covers a machine the guard refuses, a body that
isn't JSON or lacks the `application/json` content type, and a query value of the wrong type
(`session=abc`). Decide by the HTTP status first.

## Examples

### curl

```sh
E=http://127.0.0.1:19113
curl -s $E/api/ps5/rp/session
# {"host":"192.168.1.100","session":3,"playing":true,"attached":true,"passcode":false}
curl -s -D - -o before.png "$E/api/ps5/rp/frame?host=192.168.1.100&session=3&max_width=1280" | grep -i x-frame-id
curl -s -X POST $E/api/ps5/rp/input -H 'content-type: application/json' \
  -d '{"host":"192.168.1.100","session":3,"buttons":["right"],"duration_ms":150}'
sleep 0.3
curl -s -o after.png "$E/api/ps5/rp/frame?host=192.168.1.100&session=3&max_width=1280&after=2.4711"
```

### Python (standard library only)

```python
import base64, json, time, urllib.error, urllib.request

E = "http://127.0.0.1:19113"

def call(path, body=None):
    req = urllib.request.Request(E + path, method="POST" if body else "GET",
                                 data=json.dumps(body).encode() if body else None,
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        raise RuntimeError(json.load(e))

s = call("/api/ps5/rp/session")
if not s["host"]:
    s = call("/api/ps5/rp/start", {"host": "192.168.1.100", "wait_s": 30})
host, session = s["host"], s["session"]

def frame(after=None):
    q = f"/api/ps5/rp/frame?host={host}&session={session}&max_width=1280&json=1"
    return call(q + (f"&after={after}" if after else ""))

before = frame()
open("before.png", "wb").write(base64.b64decode(before["image_base64"]))

call("/api/ps5/rp/input", {"host": host, "session": session,
                            "buttons": ["right"], "duration_ms": 150})
time.sleep(0.3)  # the hold, plus a moment for the game to draw

after = frame(after=before["frame_id"])
open("after.png", "wb").write(base64.b64decode(after["image_base64"]))
print("picture age", after["frame_age_ms"], "ms")

call("/api/ps5/rp/input/release", {"host": host, "session": session})
```

If a request fails partway (a timeout, a dropped connection), don't send the input again
blindly: take a frame first and decide from what the screen shows.

## Limits

- The picture is the one the app draws, after its picture settings (sharpening, crop): the
  stream's size (1080p at most), or larger with upscaling on. Use `max_width` to keep it small. In HDR sessions the image may come out blank on
  some systems; switch Video format to SDR for automation.
- If the app window is minimised or hidden, the browser may stop drawing: `frame_age_ms` grows.
  Keep the Play screen visible, or close it so frames come from ffmpeg.
- One automation controller per session: two scripts at once share it (the latest input wins).
- Unless `PS5UPLOAD_ALLOW_AUTOMATION` is set, only the status endpoint can be reached from the
  network, by a machine you listed in `PS5UPLOAD_ALLOW_IP`, and without the session id.

## The console list and settings

These work on every platform, whichever app is in use. The desktop and mobile apps mirror their
list and settings to their own engine, and pick up a change made here within a minute (at once
when the window gets focus).

### `GET /api/consoles`, `PUT /api/consoles`

```json
{"rev": 7, "consoles": [{"id": "e1f0…", "name": "Living room", "host": "192.168.1.100",
  "notes": null, "has_rp_credentials": true, "has_wake_credential": true}],
 "paired_hosts_not_listed": []}
```

`PUT` takes the whole list in order: `{"base_rev": 7, "consoles": [{"id", "name", "host", "notes"}]}`.
`base_rev` is optional; when it is stale the answer is `409` with the current list. A console without
`id` keeps the id of the entry with its address, or gets a new one. Keys can be set
(`regist_key`, `rp_key`, `wake_credential`) but are never shown: leaving them out keeps them, an
empty string clears them, and a console taken off the list loses them. They are stored encrypted.

Wherever a route takes `addr` or `host` for a console, it also takes a console's id or name from
this list. `POST /api/ps5/power/wake` and `/wake-login` use the stored keys when none are sent.

`GET /api/shared/consoles/schema` and `GET /api/shared/prefs/schema` describe the two documents
underneath, as JSON Schema.

### `GET /api/settings`, `PATCH /api/settings`

`GET` answers `{"rev", "settings": {"bandwidth_cap_mbps": 0, ...}, "set": [...], "schema": {...}}`:
every setting with its type, range and default. `PATCH` takes
`{"settings": {"bandwidth_cap_mbps": 50, "auto_resume": false}}`, all of it or nothing (`400` lists
what is wrong); `null` goes back to the default. These settings drive what the app's queue does.

### What stays in the app

Some things are kept by each app or browser on its own and have no route:

- **Schedules** run only while a window is open. For a schedule without one, use your system's
  scheduler with `curl`.
- **Play time** is worked out from what each app sees running, per browser or app.
- **Notifications** are per browser or app. `GET /api/event-journal` has the engine's own events.
- **The upload queue** runs in the app. Scripts use `/api/transfer/*` and `/api/pkg/install` directly.

### Which list is which

- `GET /api/ps5/apps/installed?addr=`: what is on the console now, read from it.
- `GET /api/pkg/installed?addr=&title_id=`: the package files one title has on the console.
- `GET /api/games/installed`: what every saved console had when it was last read, without asking it.
- `GET /api/ps5/process/list?addr=`: running processes with title ids, memory and kill support.
- `GET /api/ps5/proc/list?addr=`: a lighter list from the kernel's process table.

## The live feed (OBS, ffmpeg)

Added in 11.0.0 for [#635](https://github.com/phantomptr/ps5upload/issues/635). Same rules as
above: this computer only, addressed as localhost.

### `GET /api/ps5/rp/live.ts`: the running stream as MPEG-TS

The Remote Play session's picture and sound, as the console sent them (nothing re-encoded):
H.264 (stream type `0x1B`) or HEVC (`0x24`) video and Opus sound (stream type `0x06` with the
`Opus` registration descriptor, as ffmpeg writes it), for as long as the reader stays. Any
ffmpeg-based player reads it, for example OBS's Media Source (Input format `mpegts`) or

```sh
ffmpeg -i http://127.0.0.1:19113/api/ps5/rp/live.ts -c copy recording.ts
```

- It starts at a keyframe, and while anyone reads, the console is asked for one every 2 s.
- After a lost frame, the damaged frames that follow are held back until the next keyframe
  (asked for at once); the sound keeps going.
- A reader that falls behind skips to the next keyframe instead of building a backlog; it never
  slows the session or the app's own picture.
- Timestamps follow the console's frame numbers (one per frame at the session's frame rate,
  10 ms per sound frame), smoothed against arrival times, and always move forward.
- `?addr=<console>`: only that console's session (`host` and `console` mean the same). No session
  (or another console's): `503 {"error": "no_session"}` at once, so a player's own reconnect picks
  it up later. The status 503 is on purpose here, where the frame and input endpoints say 404.
- Up to 8 readers at once (`429 {"error": "too_many_readers"}` beyond that). The stream ends when
  the session does.

### `GET /api/ps5/rp/live/status`

```json
{"running": true, "host": "192.168.1.100", "consumers": 1, "go_live": 0, "codec": "h264",
 "width": 1920, "height": 1080, "fps": 60, "bitrate_kbps": 15000, "since_keyframe_s": 0.8,
 "frames_held_after_loss": 0, "losses": 0, "keyframe_every_s": 2, "readers": [ … ]}
```

`codec` is `h264`, `h265` or `h265_hdr`. `consumers` counts players reading `live.ts`; `go_live` is the app's own Go live, which reads
the same feed inside the engine. `{"running": false, "consumers": 0}` when nothing streams.

## Go live

The app's own broadcast to YouTube, Twitch, Kick, Facebook or any RTMP/SRT server. These answer on
the engine's usual guard: this computer, plus addresses in `PS5UPLOAD_ALLOW_IP` (wider than
`live.ts`, which is this computer only, because the web UI's Go live runs from elsewhere).

| Request | Does |
|---|---|
| `GET /api/stream/status` | ffmpeg, the session, each site's checks against it, the live run |
| `POST /api/stream/start` | `{"destinations": [{"key_id": "<id>"}], "audio_kbps": 160}` starts a run; a stop still finishing is waited for |
| `POST /api/stream/stop` | Stops the run; `phase` says `stopping` until ffmpeg has gone |
| `POST /api/stream/destinations` | Adds a site: `{preset, key, server?, url?, label?, bandwidth_test?}` |
| `POST /api/stream/destinations/{id}` | Changes a site: only what changes, for example `{"enabled": false}`. No `key` keeps the saved one |
| `DELETE /api/stream/destinations/{id}` | Removes a site |
| `POST /api/stream/tools/ffmpeg/install` | Downloads ffmpeg; progress is in the status |

Stream keys are never returned, only `key_hint` (the last four characters). `key_lost: true` means
the saved key can't be read any more: add the site again. A run's `phase` is `starting`, `live`,
`reconnecting`, `waiting_for_play`, `stopping`, `ended` or `failed`.

Refusals are `{"error": "<sentence>", "code": "<code>"}`. A run's and a destination's `error` have the
same `code`, and one of the failure codes.

| Code | HTTP | Meaning |
|---|---|---|
| `no_session` | 409 | No Remote Play session to stream |
| `already_live` | 409 | A run is going: stop it first |
| `ffmpeg_missing` | 412 | Install ffmpeg first |
| `no_destinations` | 400 | The start named no site |
| `codec_not_allowed` | 409 | A site doesn't take the session's video format; `destinations` says which |
| `destination_missing`, `not_found` | 404 | No such site |
| `preset_unknown` | 400 | Not one of the sites |
| `key_missing`, `key_invalid`, `key_lost` | 400 | No usable stream key |
| `server_missing`, `url_invalid` | 400 | The server address |
| `unsupported` | 501 | No ffmpeg download can run on this system: set `PS5UPLOAD_FFMPEG` |
| `disk`, `store` | 500 | This computer's disk, or a damaged saved file (named in `store_error` in the status) |
| `checksum`, `download`, `unpack` | in `ffmpeg.install` | The ffmpeg download |
| `unreachable`, `tls`, `rejected`, `codec_not_allowed`, `dropped`, `stopped` | in the run | Why a site stopped: can't be reached, certificate, refused key, wrong codec, dropped, other |
| `all_failed`, `ffmpeg_failed`, `play_ended` | in the run | No site took it, ffmpeg can't start, Remote Play stayed off for two minutes |

## An MCP adapter

PS5Upload ships one, inside the engine: see [MCP.md](MCP.md). The `rp_session`, `rp_start`, `rp_stop`,
`rp_frame` (the image plus its metadata), `rp_input` and `rp_input_release` tools are these endpoints,
under the same rules, and the `play_session` prompt walks an agent through watching and pressing. The
rest of the engine's routes are in [API.md](API.md) and `GET /api/openapi.json`.
