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

## An MCP adapter

These endpoints map directly to tools an MCP (Model Context Protocol) server could expose:
`status`, `frame` (returning the image plus its metadata), `input` and `release_all`. A thin
local stdio adapter around the HTTP calls above is enough; PS5Upload does not ship one.
