# Remote Play automation (local scripts and agents)

A script or AI agent on the same computer as the PS5Upload engine can watch and play a running
Remote Play session while you keep watching it in the app:

1. **status**: which console is streaming, and its session id;
2. **frame**: the picture the app shows right now, as a PNG or JPEG;
3. **input**: hold a controller state (buttons, sticks, triggers) for a short time, then let go;
4. **frame** again, to see what happened.

Added in 10.6.0 for [#615](https://github.com/phantomptr/ps5upload/issues/615).

> **Not a stable API yet.** These endpoints can change between versions (see
> [#430](https://github.com/phantomptr/ps5upload/issues/430)). Check the engine version
> (`GET /api/version`) and pin your script to it.

## What it does, and what it never does

- **This computer only.** Every endpoint here answers only to the engine's own computer
  (a loopback address: `127.0.0.1` or `::1`). `PS5UPLOAD_ALLOW_IP` does **not** open them to
  other machines; from anywhere else they answer `403 {"error": "local_only"}`.
- **Address it as localhost.** Requests must use `localhost`, `127.0.0.1` or `[::1]` as the host
  name (any port); any other name answers `403 {"error": "host_not_local"}`.
- **It never starts a stream.** Start Remote Play in the app first (Play › Start). The session is
  the one the app runs.
- **It never takes the picture over.** The engine does not decode video. A frame request asks the
  app page that shows the stream for the picture already on its screen; nothing on screen
  changes, no second stream or decoder runs. Watch-party viewers and phones are never asked.
- **It is one more controller.** Input is merged with yours, like a phone joining as a
  controller: you can still play, and letting go of the script's buttons never lets go of yours.
- **It never sends anything twice.** A refused or failed request is not retried by the engine.

The engine listens on `http://127.0.0.1:19113` by default (the app may pick another port if that
one is taken; the app's log says which).

## Endpoints

### `GET /api/ps5/rp/session`: status

```json
{"host": "192.168.1.100", "session": 3, "playing": true, "attached": true, "passcode": false}
```

`host: null` (and `session: null`) means nothing is streaming. `attached` is whether an app page
shows the stream now (a frame needs one). `session` changes every time a new stream starts; pass
it to the other endpoints so a script never drives a session it did not mean to.

### `GET /api/ps5/rp/frame`: the current picture

| Query | Meaning |
| --- | --- |
| `host` | The console, as the session shows it (required). |
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

### `POST /api/ps5/rp/input`: hold a controller state

```json
{"host": "192.168.1.100", "session": 3, "buttons": ["right"], "duration_ms": 150}
```

| Field | Meaning |
| --- | --- |
| `host`, `session` | Required. A stale session is refused (`409`), nothing is sent. |
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

Every error is JSON, `{"error": "<code>"}`:

| Status | `error` | Meaning |
| --- | --- | --- |
| 400 | `bad_request` | A field is missing or out of range (`field` or `button` says which). |
| 403 | `local_only` | Not from this computer. |
| 404 | `no_session` | Nothing is streaming from that console. |
| 409 | `stale_session` | Another session runs now (`session` gives its id). |
| 409 | `no_viewer` | The stream runs but no app page shows it (left the Play screen), or the page left while answering. |
| 429 | `busy` / `rate_limited` | A frame request is already running / too soon (`retry_ms`). |
| 503 | `no_picture` | The page has not shown a picture yet. |
| 503 | `viewer_busy` | The page's connection is full; try again shortly. |
| 504 | `timeout` | The page did not answer within 2 s. |
| 504 | `no_newer_frame` | No picture newer than `after` came within 1.5 s. |
| 502 | `page_error` | The page could not make the image. |

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
if not s["host"] or not s["attached"]:
    raise SystemExit("start Remote Play in the app first")
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
  Keep the Play screen visible.
- One automation controller per session: two scripts at once share it (the latest input wins).
- The engine's own access rules are unchanged: nothing here is reachable from the network.

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
- `?addr=<console>`: only that console's session. No session (or another console's):
  `503 {"error": "no_session"}` at once, so a player's own reconnect picks it up later.
- Up to 8 readers at once (`429 {"error": "too_many_readers"}` beyond that). The stream ends when
  the session does.

### `GET /api/ps5/rp/live/status`

```json
{"running": true, "host": "192.168.1.100", "consumers": 1, "go_live": 0, "codec": "h264",
 "width": 1920, "height": 1080, "fps": 60, "bitrate_kbps": 15000, "since_keyframe_s": 0.8,
 "frames_held_after_loss": 0, "losses": 0, "keyframe_every_s": 2, "readers": [ … ]}
```

`consumers` counts players reading `live.ts`; `go_live` is the app's own Go live, which reads
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

These endpoints map directly to tools an MCP (Model Context Protocol) server could expose:
`status`, `frame` (returning the image plus its metadata), `input` and `release_all`. A thin
local stdio adapter around the HTTP calls above is enough; PS5Upload does not ship one.
