# The PS5Upload API

Everything the app does with a console goes through its engine, and the engine is an HTTP server
you can call too: from a script, a home automation, or an AI agent ([MCP.md](MCP.md)).

The full reference is generated from the code:

```sh
curl -s http://127.0.0.1:19113/api/openapi.json -o ps5upload-openapi.json
```

**MCP & API** in the app's sidebar lists every route with a search and a curl line to copy.

That is an OpenAPI 3.1 document of every route, which you can open in Swagger UI, Scalar or any
other OpenAPI tool or generate a client from. This page explains what the document can't: how to
reach the engine, who it answers, and how errors, long calls and events work.

## Is it stable?

Routes can change between versions, and there is no version in the URL. `GET /api/version` says the
engine's version and what this build can do; `info.version` in the document says the same. Pin a
script to a version, and read the [changelog](CHANGELOG.md) when you update.

## Reaching it

| Where the engine runs | Address |
|---|---|
| The desktop app, on the same computer | `http://127.0.0.1:19113` while the app is open |
| A standalone engine or Docker image | `http://<host>:19113`, see [engine/README.md](engine/README.md) |
| Behind a reverse proxy | Your proxy's address, see [REVERSE_PROXY.md](REVERSE_PROXY.md) |

- **The port.** 19113 unless `PS5UPLOAD_ENGINE_PORT` says otherwise. If something else holds 19113
  when the desktop app starts, the app moves its engine to a free port; `lsof -iTCP -sTCP:LISTEN`
  or `netstat -ano` shows which.
- **A path prefix.** With `PS5UPLOAD_BASE_URL=/ps5/`, routes move to `/ps5/api/...`. `/pkg-host/`
  stays at the root, because the console fetches it by the address it was given.
- **Phones.** The Android and iPhone apps run the engine inside the app for the app's own use. Point
  scripts at an engine on a computer or NAS instead.

```sh
curl -s http://127.0.0.1:19113/api/version
```

## No password, on purpose

The API has no authentication. That is the owner's decision, not an oversight: the engine belongs
to you, on your computer or your LAN. What stands in for a password is a set of guards on who can
get a request through. Anyone a guard lets in can read, write and delete on your console, so never
expose the engine to the internet.

### The guards

Each request passes these in order. A refusal is a `403` in the [error shape](#errors) below.

| Guard | What it checks | How to open it |
|---|---|---|
| **Address** | The caller is this computer (a loopback address). | `PS5UPLOAD_ALLOW_IP`: addresses and CIDR ranges, comma-separated, like `192.168.1.20,10.0.0.0/24`. |
| **Host name** | The `Host` header is a loopback name, an IP address, a name with no dot, or a name under `.local`, `.lan`, `.home`, `.home.arpa`, `.internal`, `.intranet`, `.localdomain` or `.ts.net`. This stops a web page reaching the engine by pointing its own domain at it (DNS rebinding). Answers `code: "host_not_allowed"`. | `PS5UPLOAD_ALLOW_HOST`: names, `*.example.com`, or `*`. |
| **Browser origin** | A request from a web page must come from the same server, or from the app itself. Programs that send no `Origin` or `Sec-Fetch-Site` (curl, Python, agents) pass. | Serve the page from the engine, or from a proxy that forwards `Host`. |
| **This computer only** | Some routes answer only a loopback caller who addressed the engine as `localhost`, `127.0.0.1` or `[::1]`. Answers `host_not_local` or `local_only`. | `PS5UPLOAD_ALLOW_AUTOMATION=1` lets the `PS5UPLOAD_ALLOW_IP` addresses use them, by an IP address or a name in `PS5UPLOAD_ALLOW_HOST`. |

Behind a reverse proxy, the address the engine sees is the proxy's. `PS5UPLOAD_TRUSTED_PROXY` names
the proxies whose `X-Forwarded-For` it believes; without it that header is ignored.

### Which routes answer whom

Every operation in the document carries `x-ps5upload.access`:

| `access` | Who gets through |
|---|---|
| `loopback` | Most routes. This computer and the `PS5UPLOAD_ALLOW_IP` addresses. |
| `strict-local` | Also tagged `strict-local`. Remote Play start, stop, frame, input, passcode, keyboard, microphone, the live feed, virtual keyboard and mouse. This computer alone, unless `PS5UPLOAD_ALLOW_AUTOMATION=1`. |
| `host-checked` | Play Room's and Go live's host routes. This computer, addressed by a loopback name or one in `PS5UPLOAD_ALLOW_HOST`, and the `PS5UPLOAD_ALLOW_IP` addresses. |
| `lan-open` | `/pkg-host/*` (package bytes for the console), `/pad`, `/api/pad/ws`, `/join`, `/api/join/ws`, `/api/join/preview`, `/api/join/signal`, `/api/join/bridge`, `/api/play/app`, `/share`, `/api/share/ws` (a phone or a friend, which check their own key or invite; a Share serves only what the sender allowed; PS5Upload Play on the TV gets only "the engine is there" until it is paired; the bridge joins a friend's internet room for PS5Upload Play through the hub in the settings only, checked against the link's fingerprint). |
| `mcp` | `/mcp`. Like `loopback`, and a browser may reach it only from this computer. |

Two routes are left out of the document and refused by the MCP tools, because they hand out the
keys that sign in to a console's users: `/api/consoles/document` and `/api/settings-backup/get`. They
are for the app's own use. MCP also refuses a console's backup slots through `/api/mesh/doc`, which
hold the same keys.

## Naming a console

Most routes that touch a console take its address as `addr`: in the query for a `GET`, in the JSON
body for a `POST`.

- **Other names.** `console` and `host` work wherever `addr` does (`ps5_addr` on the install routes
  takes the same three). The document lists these as `x-aliases` on the field. The name the route
  itself uses is in `x-ps5upload.console.name`.
- **One name per field.** Sending two names for one field, say `addr` and `console`, is an error with
  `duplicate field`: a `422` in a JSON body, a `400` in a query. Send one.
- **Left out.** Reads fall back on the engine's default console, which `GET /api/version` shows as
  `default_console`. Anything that changes something refuses a missing address with a `400`; such a
  route has `x-ps5upload.console.required: true`.
- **A saved name.** `GET /api/consoles` lists the consoles the app has saved, without their keys.
  `POST /api/discover` looks for consoles on the network.

## Errors

Every failure of an `/api/` route is JSON, with the status it would have had anyway:

```json
{ "error": "no such route: /api/nope", "code": "not_found" }
```

- `error` is words for a person. Don't match on it.
- `code` is a short name. Where the route gave none, it comes from the status: `404` is `not_found`,
  `502` is `bad_gateway`.
- Some routes add fields of their own beside these: `reason` (the helper's own token), `field`,
  `setting`. Those are in the document where the source shows them.

The status tells you whose fault it was:

| Status | Means |
|---|---|
| `400` | The request was wrong: a missing address, a bad path. |
| `403` | A guard refused it. |
| `409` | The console or the engine is busy with something that conflicts. An operation you cancelled also ends here, with `cancelled`. |
| `422` | The JSON body could not be read as that route's fields, including two names for one field. |
| `499` | Only as an operation's `http_status`: see [abandoned operations](#long-calls). |
| `502`, `504` | The console or the server behind this did not answer as it should. |
| `507` | Not enough space. |

### Friends playing

While friends are in a Play Room, a request that would end their game is answered `409`
instead of done:

```json
{ "error": "friends_playing", "code": "friends_playing", "friends": ["Mia", "Lee"],
  "console": "192.168.1.20", "message": "Friends are playing on this PS5. Stop anyway?" }
```

`friends` names everyone in ("Friend" for someone who gave no name). `console` is the console the
request named. Ask the person, and if they say go ahead, send the same request again with any one of:

- the header `X-Confirm-Friends: 1`,
- the query `confirm_friends=1`,
- `"confirm_friends": true` in the JSON body.

With nobody in, these routes work as they always did. The engine's own work is never asked about.

| Route | Asked when |
|---|---|
| `POST /api/ps5/rp/stop` | always (ends the Remote Play session friends play on) |
| `POST /api/ps5/power/control` | `standby`, `reboot`, `shutdown` (not `tick`) |
| `POST /api/ps5/process/kill` | always |
| `POST /api/ps5/app/lifecycle` | `kill`, `suspend` (not `resume`, `list`) |
| `POST /api/ps5/app/launch`, `POST /api/play/app/open` | always (a launch replaces the running game) |
| `POST /api/ps5/helper/stop` | always (the helper runs friends' controllers) |
| `POST /api/ps5/helper/uninstall` | unless `check: true` |
| `POST /api/payload/send` | when the payload is the helper (the raw-body form takes the header or the query) |
| `POST /api/payload/send-check` | `helper: true` |
| `POST /api/pkg/payload-restore` | always (it sends the helper) |

The Remote Play page's socket (`GET /api/ps5/rp/ws`) does the same in its own way: `{"stop": true}`
with friends in is answered `{"stage": "confirm_friends", "friends": [...], "message": "..."}` and
nothing stops; `{"stop": true, "confirm_friends": true}` stops. The engine never updates itself, so
`GET /api/engine/update` lists who is playing (`friends_playing: [{console, friends}]`) for the app
to ask before an update or restart. The MCP tools for these actions take `confirm_friends` too.

A session with friends in keeps running with no page of the host's attached (a phone asleep, the
desktop closed), picture on or off, for up to 4 hours since the host's last page left; then it ends
as any session with no page does.

## Long calls

Many routes wait until the console has finished, which can take minutes, and a proxy or a client
timeout may cut them off. There are two ways out.

### Jobs

Uploads, downloads and conversions answer at once with a `job_id`, and the document marks them with
`x-ps5upload.long_running`.

```sh
curl -s -X POST http://127.0.0.1:19113/api/transfer/file \
  -H 'content-type: application/json' \
  -d '{"addr":"192.168.1.100","src":"/path/to/file.bin","dest":"/data/file.bin"}'
# {"job_id":"…"}
curl -s http://127.0.0.1:19113/api/jobs/<job_id>
```

- `GET /api/jobs/{id}`, and `POST /api/jobs/{id}/cancel`, `/pause`, `/resume`.
- `GET /api/jobs` lists transfers and installs together, newest first, the last 256 finished. Each
  row has `job_id`, `kind`, `status`, `seq` and `job`.
- **Following it without missing anything.** `?since_seq=<n>&limit=<m>` gives only what changed
  after `n`, oldest change first. The `x-ps5upload-last-seq` response header is the cursor for your
  next call, and the same numbers as `/api/events` ids. While nobody reads `/api/events`, running jobs report no progress ticks, so `since_seq` finds a job when its state changes; read `GET /api/jobs/{id}` for its bytes.
- **Tasks started on the console.** `GET /api/ps5/console-tasks?addr=` lists what someone started on
  the console page, with its progress, and `POST /api/ps5/console-tasks/cancel {addr, id}` stops one
  whose `cancellable` is true. A helper older than this feature answers `supported: false`.
  Each task has `elapsed_s` (seconds since it started on the console) when the helper knows it, and
  copies and installs waiting in the page's queue are listed with `state: "queued"` (read-only).

### `Prefer: respond-async`

Add this header to any `POST` under `/api/` and the engine runs the route in the background and
answers `202` at once:

```sh
curl -s -X POST http://127.0.0.1:19113/api/ps5/fs/copy \
  -H 'Prefer: respond-async' -H 'content-type: application/json' \
  -d '{"addr":"192.168.1.100","from":"/data/a","to":"/data/b"}'
```

The answer holds an `op_id`. Then `GET /api/ops/{id}`:

```json
{ "id": "…", "kind": "op", "status": "done", "http_status": 200, "method": "POST",
  "path": "/api/ps5/fs/copy", "started_at_ms": 0, "finished_at_ms": 0, "body": { "ok": true } }
```

- `status` is `running` or `done`. `http_status` and `body` are what the route itself would have
  answered, so a `done` can still be a failure.
- `GET /api/ops` lists operations, and also transfer jobs (`job:<id>`) and installs
  (`install:<id>`). The status and cancel routes take those ids too, so one poller covers all three.
- `POST /api/ops/{id}/cancel` uses the route's own cancel where it has one (console file operations,
  which also show `fs_op_id` for `GET /api/ps5/fs/op-status`).
- **An abandoned operation.** A console call that has no cancel can't be recalled. The engine stops
  waiting, answers `{"cancelled": false, "abandoned": true}`, and the operation ends with
  `http_status: 499`, `code: "abandoned"`. The work may still finish on the console.
- Finished operations are kept for an hour, at most 256 of them, and an answer over 4 MiB is cut
  short with `body_truncated: true`.

## The queue

`/api/queue` is the engine's own line of work: one line per console, kept on disk and shared by
everyone using this engine. An item is one call of an existing route, followed to its end.

```sh
curl -s -X POST http://127.0.0.1:19113/api/queue -H 'content-type: application/json' -d \
  '{"console":"192.168.1.100","kind":"upload_dir","request":{"src_dir":"/games/x","dest_root":"/data/x"}}'
curl -s 'http://127.0.0.1:19113/api/queue?console=192.168.1.100'
```

- **`kind`** picks the route, and `request` is that route's body without the console:
  `upload_file`, `upload_dir`, `upload_zip`, `upload_7z`, `upload_rar`, `upload_tar` (a tar,
  plain or compressed, or a lone `.zst`), `upload_file_list`,
  `link_download`, `download`, `install`. Also `label`, `position` and `stop_on_failure` (if this
  item fails, the ones after it are paused). An item's request can be at most 1 MiB; send a list of
  100,000 files to `/api/transfer/file-list` directly.
- **Order.** One at a time per console, in order; different consoles run side by side. The console is
  required here, and `addr` and `host` work too.
- **It waits.** While anything else uses the console (a job, an install, a hold, a task running on the
  console itself such as a Minion copy) or its helper doesn't answer, the line holds instead of failing,
  and `waiting_for` says why: `hold`, `other_work`, `console_task`, `console` or `settle`. A helper from
  before 11.1.0 can't report its tasks, so `console_task` never shows there. It never sends the helper or
  wakes a console.
- **Removing a running upload** (`cancel=true`) stops it. Its `<dest>.ava-part` stays until it is cleaned
  up by hand or the same upload is resumed, and so do an empty `<dest>` it created and part files inside a
  folder that was already there.
- **`status`** is `queued`, `running`, `paused`, `done`, `failed`, `cancelled` or `interrupted`.
- **No retries of its own.** A failure may have come after the bytes landed, so a failed item stays
  failed until `POST /api/queue/{id}/resume`, the only way an item runs twice.
- **After a restart.** An item that was running comes back `interrupted`, not restarted. Items that
  were waiting carry on.
- **Controls.** `POST /api/queue/{id}/pause`, `/resume`, `/move {position}`, and `/remove`. Removing
  a running upload or download needs `{"cancel": true}`. A running install can't be removed or paused:
  the console goes on with it.
- **Your own work on a console.** `POST /api/queue/hold {console, holder, ttl_s?}` keeps the queue from
  starting anything there until `POST /api/queue/release` or the hold lapses (30 seconds unless you
  say, 300 at most). It answers `409 queue_running` while an item runs; wait and ask again.
- **Events.** Changes arrive on `/api/events` as `event: queue`.

Known gaps:

- **The app's Files uploads don't wait for a queue item.** The queue waits for them, but `fsUploadQueue`
  in the app doesn't wait for the queue, so one can start while an item runs.
- **Direct calls aren't refused.** `POST /api/transfer/*` during a running item goes ahead. Take a hold
  first if you mean to share a console with the queue.
- **Secrets aren't saved.** RAR passwords and link tokens are kept in memory only. After a restart
  those items come back `interrupted` with the code `secret_not_kept`, and `resume` answers `409`: add
  them again.

## Events

`GET /api/events` is a server-sent event stream.

```sh
curl -N http://127.0.0.1:19113/api/events
```

- Every event has an `id:`, a number that rises across restarts.
- The default event (no `event:` line) is a job snapshot. Named events are `journal`, `install`,
  `op` and `queue`.
- **Reconnecting.** Send the last id as the `Last-Event-ID` header and you get what you missed.
- **`resync`.** If the engine no longer has what you missed (it keeps the last 1000), or you fell too
  far behind, you get `event: resync` with `{"resync": true, "dropped": <n>}`. Fetch `/api/jobs`
  again and carry on.

`GET /api/event-journal?since_seq=&limit=` reads the same journal as a list.

## Log levels

The app, the engine and the PS5 helper share one setting: a level (`error`, `warn`, `info`,
`debug`, `trace`) for everything, plus per-category overrides. The categories are `general`, `api`,
`connection`, `helper`, `rp` (Remote Play), `pairing`, `wake`, `transfer`, `install`, `cleanup`,
`files`, `convert`, `together`, `golive`, `mesh` and `ui`. A line is written when its level is at
most its category's.

Anything above `info` is a raise and ends by itself: after `minutes` (default 60) or at `until_ms`,
never more than a day. Levels at `info` or below stay until changed. The engine keeps the setting
on disk, the app follows it, and the engine hands it to each helper it talks to (a helper from
before 11.5 keeps its own fixed level).

```sh
curl http://127.0.0.1:19113/api/logs/level
# Remote Play and the helper at trace for 30 minutes, the rest unchanged:
curl -X PUT http://127.0.0.1:19113/api/logs/level -H 'Content-Type: application/json' \
  -d '{"categories":{"rp":"trace","helper":"trace"},"merge":true,"minutes":30}'
# Back to info everywhere:
curl -X PUT http://127.0.0.1:19113/api/logs/level -H 'Content-Type: application/json' -d '{"reset":true}'
# The engine's newest Remote Play lines at debug or more severe, from the last 10 minutes:
curl "http://127.0.0.1:19113/api/logs/recent?level=debug&category=rp&since_ms=$(( ($(date +%s) - 600) * 1000 ))"
```

`/api/logs/recent` answers `{lines: [{seq, ts_ms, level, cat, msg}], count, next_seq, levels}`; pass
`since_seq=<next_seq>` to read on. Every line has its secrets taken out where it is written
(passwords in URLs, secret-named fields, bearer tokens, pairing PINs). The engine's request log
(one `debug` line per call, category `api`) is always kept in `engine.log`, whatever the level, as
the trace for a crash. `POST /api/diag/log-level` (Detailed logs) still works: on raises the global
level to `debug`.

## Route families

Each operation's first tag is its family. `GET /api/openapi.json` has every route; these are the
places to start.

| Family | Routes | What it's for |
|---|---|---|
| `engine` | `/api/version`, `/api/events`, `/api/event-journal`, `/api/engine-logs`, `/api/logs/*`, `/api/diag/*`, `/api/metrics`, `/api/openapi.json`, `/api/mgmt/call`, `/api/support` | The engine itself. `/api/mgmt/call` is the raw helper call, for when nothing else fits. `/api/support` is Thanks & Support (the supporters' names and the support links) for PS5Upload Play and Minion: public, reachable from the home network. |
| `consoles` | `/api/consoles`, `/api/discover`, `/api/ps5/status`, `/api/ps5/readiness`, `/api/ps5/volumes`, `/api/ps5/connect-timings`, `/api/ps5/link-history` | Which consoles there are and how they stand. See [Consoles, Tailscale and new devices](#consoles-tailscale-and-new-devices). |
| `console-files` | `/api/ps5/list-dir`, `/api/ps5/fs/*`, `/api/ps5/disk-usage`, `/api/ps5/folder-sizes/*`, `/api/ps5/console-shares` | Files on the console; known folder sizes and measuring them. |
| `transfer` | `/api/transfer/*`, `/api/files/upload`, `/api/link/*`, `/api/zip/*`, `/api/7z/*`, `/api/rar/*`, `/api/tar/*` | Uploads and downloads. |
| `jobs` | `/api/jobs/*`, `/api/ops/*`, `/api/ps5/console-tasks`, `/api/activity` | Following and stopping work. |
| `queue` | `/api/queue/*` | The engine's own line of uploads, downloads and installs. |
| `install` | `/api/pkg/*`, `/api/ps5/pkg/*`, `/pkg-host/*` | Package installs and links. |
| `games` | `/api/games/*`, `/api/ps5/app/*`, `/api/ps5/apps/*`, `/api/ps5/appdb`, `/api/ps5/process/*`, `/api/ps5/dump`, `/api/ps5/sdk/*` | Titles, launching and stopping, processes. |
| `saves` | `/api/ps5/saves/*`, `/api/save-backup/*`, `/api/ps5/backup/*` | Save data and backups. |
| `cheats`, `payloads` | `/api/ps5/cheats/*`; `/api/payload*`, `/api/ps5/pldmgr/*`, `/api/ps5/helper/*` | Cheats; the helper and other payloads. |
| `hardware`, `power` | `/api/ps5/hw/*`, `/api/ps5/fan-boost`; `/api/ps5/power/*` | Temperatures and fan; rest, reboot and wake. |
| `health` | `/api/ps5/health/*`, `/api/ps5/cleanup/*`, `/api/net/*` | Checks, clean-up, network diagnosis. |
| `remote-play` | `/api/ps5/rp/*`, `/api/ps5/vinput/*`, `/api/ps5/remoteplay/*` | Sessions, picture, input. See [AUTOMATION.md](AUTOMATION.md). |
| `play-together`, `go-live` | `/api/join/*`; `/api/stream/*` | Friends joining; streaming to other sites. |
| `share` | `/api/share/*` | An app's things sent to a friend, and things received. See [Share](#share). |
| `workshop` | `/api/fpkg/*`, `/api/exfat/*`, `/api/folder/*`, `/api/fakelibs/*`, `/api/ampr/*`, `/api/backport/*` | Package conversion, images, backporting. |
| `collection`, `this-computer` | `/api/collection/*`; `/api/local/*` | Game folders and files on this computer. |
| `mesh` | `/api/mesh/*`, `/api/drive/*` | Devices sharing a console, and Mesh Drive. |
| `remote-sources` | `/api/remote/*` | NAS and other servers. |
| `tailnet` | `/api/tailnet`, `/api/tailnet/*` | This device on Tailscale, and the consoles on its tailnet. (The console's own node is `/api/ps5/tailnet`.) |
| `settings`, `users`, `pairing` | `/api/settings`, `/api/shared/*`; `/api/profile/*`; `/api/ava1/*` | Settings, console users, pairing. |
| `console-tools`, `shadowmount` | `/api/ps5/shell/run`, `/api/ps5/klog`, `/api/ps5/net/*` and the rest of `/api/ps5/*`; `/api/ps5/smp/*` | Everything else on the console. |

## Consoles, Tailscale and new devices

- `GET /api/ps5/connect-timings[?addr=]`: the last status check of each console, phase by phase
  (session reused or opened, a remembered verdict, a plain connect, the status call), with the path
  (`lan` or `tailnet`) and the helper version. It sends nothing to a console. A status read that
  times out while the helper is only busy answers `reason: "status_slow"`: hold what you knew.
- `GET /api/ps5/link-history?addr=[&read_link=1]`: how the console's connection has been holding
  up, from the app's own status checks and volume reads (this engine run, the last day): drops
  (`timeout` or `refused`), recoveries, a helper that started over, USB drives appearing; the
  console's link (`wired` / `wifi`) once it was read; and `hint` when it keeps dropping (`usb_wifi`
  when that started around when a USB drive appeared on a console on Wi-Fi). `read_link=1` asks the
  helper for its link when it is not known. No addresses in the answer.
- `GET /api/ava1/console?addr=`: which console answers at an address, as `{key, name}` (its AVA1
  identity key in hex, the same at every address), without pairing. 502 when nothing answers.
- `GET /api/tailnet/consoles[?refresh=1&all=1&addr=]`: the PS5Upload consoles on this device's
  Tailscale network, each with its tailnet and home address, whether its helper answered over
  Tailscale (`reachable`), its key (`id`) and its console-list entry (`listed`). `backend` says how
  this device is on Tailscale and `can_list` whether a device list could be read (not with only
  the system Tailscale app: pass `addr=` instead). Answered from the last minute unless
  `refresh=1`. Asking pairs nothing and shows nothing on the PS5.
- `POST /api/tailnet/consoles/add {addr, name?}`: puts a console found on Tailscale (or a typed
  `100.x` address or MagicDNS name) on the console list, one entry per console, at its home address
  when one is known. `action` is `added`, `linked`, `merged` or `exists`; 400 `not_tailnet` for an
  address that is neither.
- The console's own Tailscale (`GET/POST /api/ps5/tailnet`) lists `blocked_recent`: devices its
  list turned away. `op: "allow_add", ip` adds one to the list, `op: "blocked_ignore", ip` drops
  it from the notice. This device's `POST /api/tailnet {op: "test_path", host, secs}` sends that
  console through Tailscale for `secs` seconds (0 ends it); `GET /api/tailnet`'s
  `consoles[].tested_at` (unix ms, 0 never) records the last time it was reached that way.
- New devices: with `require_approval` on (`POST /api/mesh/settings`), a device that isn't on a
  console's list is listed in `asks` of `GET /api/mesh` instead of being let in. `POST
  /api/mesh/device {addr, op: "allow" | "decline", id}` answers it, from any device the console
  already trusts; an allowed device gets in on its next try, within ten minutes. With approval off,
  any device that reaches the helper is let in, as before. Each device in `devices` says how it was
  let in (`via`: `network`, `tailscale`, `approved`, or empty for paired with a code).

## Game frame rate

The running game's frame rate is sampled: the engine reads the console display's frame count once
a second and divides the frames by the time between reads (both taken on the console). It is the
frames shown in each second, not single-frame times.

- `GET /api/ps5/hw/fps?addr=`: `{state, fps, frames, interval_ms, at_ms, capture}`. Each call keeps
  the engine sampling for 15 seconds, so the first one may answer `state: "starting"` with no `fps`.
  `state` is `ok`, `no_game` (no game picture to count), `unsupported` (a helper older than this),
  `offline` (with `error`), `no_device` or `failed`.
- `POST /api/ps5/hw/fps/capture/start {addr}` and `.../capture/stop {addr}`: a capture keeps one row
  a second (and the console's temperatures every 5 seconds) in the engine's memory until the next
  start, for up to six hours. `capture.stats` has `min_fps`, `avg_fps` (all frames over all the
  time), `low_1pct_fps` (the slowest 1% of seconds) and `max_fps`.
- `GET /api/ps5/hw/fps/capture?addr=&since=N`: the rows from `N` on; `GET
  /api/ps5/hw/fps/capture/csv?addr=`: the same as CSV. 404 `no_capture` when there is none.

## How far to trust the document

It is built from the code, and CI fails when the two disagree. It never makes up a field, so it says
when it doesn't know. Look at `x-ps5upload.response_schema`:

| Value | The answer's schema comes from |
|---|---|
| `struct` | The Rust type the route answers with. Reliable. |
| `literal` | The keys of a JSON literal in the route. The keys are real and there may be more; a value's type is given only when the code shows it. |
| `mixed` | Some of each. |
| `json-unknown` | Nothing: the route passes on JSON it doesn't describe, usually what the console's helper sent. Try it and look. |
| `none` | Nothing: the answer is built in a way the generator can't read. |
| `binary`, `stream`, `page`, `websocket` | Not JSON. |

A request's fields come from the struct the route reads, so they are reliable, including which are
required, except where a field carries only `x-rust-type`: it has its own conversion, and the Rust
type doesn't tell you the JSON.

To change the document, change the code and regenerate it:

```sh
cd engine && cargo run -p ps5upload-apidoc
```

Words the source can't give (a summary for a handler with no doc comment) go in
`engine/crates/ps5upload-engine/api/overrides.json`.

## Share

Share sends an app's things to a friend: the app itself (homebrew only), its saves, screenshots and
clips, cheats and which are on, controls, stream settings, and a note. The sender makes a link; the
friend opens it and asks for some of the items; the sender allows or declines; only then do files go,
directly between the two computers (a WebRTC data channel, DTLS-encrypted; the signalling hub only
introduces them, and on the same network the friend may use the sender's `/api/share/ws`).

**Retail games are refused by the engine**, whatever the request says. It reads how the app was
built from its own PARAM (the folder's `sce_sys`, the package or the image; for an app installed on a
console, `/user/app/<Title ID>`, the copy in `/user/appmeta/<Title ID>/` when the app's own is
encrypted), never the Title ID or the publisher code, which homebrew often borrows, and never a field
the request sends. Updates and add-ons carry their game's build and are refused too. The answer is
403:

```json
{"error": "This is a retail game (PPSA01234). PS5Upload only shares homebrew.", "code": "retail", "title_id": "PPSA01234", "item": 0}
```

An app with no readable PARAM (an archive, an encrypted package) is refused the same way, with
`"error": "PS5Upload can't confirm this is homebrew, so it can't share it."` and no `title_id`. Each
app item in a share's view carries `build: {read, marks}` (the marks found), which the friend's
engine checks again.

The sender's side:

```sh
# Make a share: an app folder on the console, a save, a note. One friend, 24 hours.
curl -s localhost:19113/api/share -H 'content-type: application/json' -d '{
  "from": "Sam", "title": "My App", "title_id": "PSUP00001",
  "items": [
    {"kind": "app", "console": "192.168.1.20", "path": "/data/homebrew/PSUP00001"},
    {"kind": "save", "console": "192.168.1.20", "folder": "/user/home/10000000/savedata_prospero/PSUP00001"},
    {"kind": "note", "text": "Have fun"}
  ]}'
# {"id": "…", "state": "open", "url": "https://ps5upload.phantomptr.com/share/#room=…&fp=…&hub=…&lan=…", "works": "anywhere", …}

curl -s localhost:19113/api/share                      # shares, receives, history
curl -s localhost:19113/api/share/<id>/decide -d '{"request": 1, "allow": true}' -H 'content-type: application/json'
curl -s -X DELETE localhost:19113/api/share/<id>       # stop it
```

`POST /api/share/check` takes the same body and answers each item's verdict without making
anything. A share's `requests` say who asked (`name`), for what (`wants_names`), and how far it got
(`state`: asking, allowed, sending, done, declined, cancelled, left; `bytes_done`, `bytes_total`).
`expires_in_s` is 60 s to 7 days (default a day); `one_use` (default true) takes one Allow, after
which the link takes nobody new. The friend can only ask for the files listed when the share was
made, by item and path; a path with `..`, an absolute path, or a symbolic link is refused.

The receiver's side: `POST /api/share/receive {link, name}` → `{id}`; `GET /api/share/receive/{id}`
shows the offer (`state: offered`, `from`, `items`); `POST /api/share/receive/{id}/accept {items,
dest?}` asks the sender (the default folder is `Received` in the Collection's first folder);
`DELETE /api/share/receive/{id}` stops. When `state` is `done`, `received` says where each item is;
`POST /api/share/receive/{id}/use-save {addr, folder, user_id?, item}` puts a received save into a
user's own save of the same game (the save stays that user's: trophies and the account are not
carried over), and `GET /api/share/receive/{id}/data/{item}` reads the small data items.

Transfers show in `GET /api/activity` (kinds `share` and `receive`), and each change is an event
named `share` on `GET /api/events`. Read through MCP, `/api/share` leaves the links out.

## Play Rooms

Play Room (called Play Together before 13.0.0; the routes keep the `together`/`join` names) is the session for one console, the **room**. It starts Private (invite only, as
before) and can be made Friends (people you've played with see it's open) or Public (a card in
Rooms on the hub). Changing who can see a room never removes anyone, and invite links keep working.

**Host routes** (this computer, or a peer `PS5UPLOAD_ALLOW_IP` admits):

- `GET /api/join?addr=` adds `room`: `{name, name_custom, visibility, size, password (true when one
  is set; never the password), approval: {friends, password, household}, lock, chat, voice,
  start_muted, watchers, lang, region, nearby, preview_shows, public_ok, game, listed: {state, why},
  blocked: [{name, tag, at_unix, always}], many_wrong_passwords, open_for_s, host_tag, lobby,
  proto, caps}`. Each guest adds `came` (`invite`, `again`, `nearby`, `friends`, `rooms`), `tag`
  (their device's 4 characters), `from_rooms`, `proto`, `caps`; each ask adds `came`, `tag`,
  `password_ok`, `from_rooms`. `listed.state` is `off`, `listing`, `listed` (`until_unix`) or
  `not_listed` with `why`: `internet_off`, `hub`, `name`, `busy`, `limit`, `old_hub`, `off`,
  `locked`, `host_away`, `blocked`, `old`.
- `POST /api/join/room {addr, name?, visibility?, size? (2-4, you count), password? ("" removes),
  approval?, chat?, voice?, start_muted?, watchers?, lang?, region?, nearby?, preview?, lock?, game?
  ({title, id} or null), public_ok?}` → the state. 409 `name` (with `why`), `public_needs_anywhere`,
  `size_below_players` (with `in`), `password_short` (4 characters at least), `password_long`;
  400 `bad`. Removing the password turns "the password lets people in without asking" off.
- `POST /api/join/kick {addr, player, block?: "room" | "always"}`: removes a player; `room` keeps
  them out of this room (by device, address and hub id), `always` keeps their device out on this
  computer too. They hear `{"bye": "removed"}` (older pages `kicked`), never that they are blocked.
- `POST /api/join/unblock {addr?, tag}` → `{ok}`.
- `POST /api/join/seat {addr, player, to}`: `to` 2-4 moves them to another pad, `1` gives them
  Player 1 (needs "Friends may get Player 1", never for someone from Rooms), `"back"` takes Player
  1 back. 409 `not_free`, `stranger`, `no_stream_seat`, `no_guest`, `not_on_seat`, `failed`.
- `POST /api/join/close {addr, confirm_friends?}`: with friends in, 409 `friends_playing` (their
  names) unless confirmed (see Friends playing); the answer lists who left in `left`.
- `GET /api/join/rooms?<the hub's /rooms query>` (the Rooms list), `?status=<lobby ids>` (friends
  lookup), `?preview=<lobby id>` (Details through the hub): the hub in the settings, cached a few
  seconds. A hub's 429 comes back with `Retry-After`, and further asks wait it out here.
- Mutes and chat/voice: see Play Rooms chat and voice; `POST /api/join/room` hands the room's chat
  and voice modes over too.

**Who may ask** (every way is checked by this engine): locked rooms answer `locked` to everyone;
Rooms knocks need Public, Friends knocks Friends or Public, Nearby knocks "Show on this network";
a set password is needed by every way but your own invite; people you've played with (a friend pass)
get in without asking unless the host turns that off; "the password lets people in without asking"
and "PS5s of this household" are opt-in. Asks waiting don't count against the room size (four wait
at most, one per device). Blocked people hear `denied`. Refusals on the guest socket:
`{"stage": "refused", "why": …}` with `denied`, `full` (`allowed`, `in`), `locked`, `password`
(`tries_left`), `locked_out` (`wait_s`; 5 wrong passwords per address or hub id in 10 minutes),
`host_away` (no host page for 5 minutes), `not_listed`, `old`, `name_taken` (`suggest`),
`already_asking`, `expired` (a link this room gave out before), `code` (a wrong typed code).

**Guest hello** gains `device` (16 random bytes, base64url: the tag and blocks), `proto` (2),
`caps`, and for a knock from the home network or Tailscale `knock: {via, password?}`. Names are
unique in a room (case, spaces and look-alikes ignored); an older page that gives a taken name plays
as the suggestion. `joined` adds `lobby` (the room's lobby id for friends), `proto` and `caps`; the
status every second adds `room: {name, size, players, you: {rtt_ms, tier}, chat, voice, locked,
playing_for_min, proto, caps}`. People from the public list never get Player 1 or the host's own
profile, and PS & Create are always blocked for them once playing.

**Preview** (`GET /api/join/preview`, reachable like `/api/join/ws`, also on the Tailscale guest
port): what the host chose to show (players with seat, kind, tier and round trip; stream codec,
bitrate, HDR; session time; approval; language, region), never an address, a way, or a guest's
tag. One per address every 10 s and 30 a minute in all (429 `slow_down` with `Retry-After`); 404
`{"why": "no_room"}`.

**The lobby** (Friends/Public rooms, and Play again over the internet): a second hub room made from
the room's own secret. The engine publishes the card (with the hub's proof of work), renews it,
withdraws it when the room turns Private, locks, or the host's device is away, answers the hub's
relayed previews, and runs knocks before any WebRTC: `{"knock": {via, name, device?, proto?, caps?,
client?, remember?}}` → `{"knock": "challenge", c, pk}` when a password or friend pass is needed →
`{"knock": "sealed", blob}` (P-256 ECDH + HKDF-SHA256 + AES-128-GCM, PS5Upload Play's format; in
`room_seal.rs`) → `{"knock": "wait"}` / `{"knock": "no", why, …}` / `{"knock": "ok", token, friend?,
lobby}`. Only an approved guest's offer reaches WebRTC; on its data channel it says
`{"hello": {"resume": token}}`.

**PS5Upload Play** (`/api/play/app`, paired): the status's `room` carries the room (chat and voice
as modes and counts, never lines) and `session.user` (the PS5 user in front); intents `room {set:
{…}}` (or the fields themselves), `kick {player, block?}`, `mute {player | all, voice?, chat?}`,
`seat {player, to}`, `unblock {tag}`, `chat {quick | text, notice?}`, `decide {ask, allow: true |
false | "group"}`, and `close {confirm_friends}` (with friends in, without it: `friends_playing`).
On the guest socket a knock that needs the password and didn't say it gets `{"knock":
"challenge", "c", "pk"}` and answers `{"knock": "sealed", "blob"}`. PS5Upload Play reaches an
internet room's lobby through this home's engine: `GET /api/join/bridge` with `{"bridge": {"v": 2,
"lobby", "knock"}}` relays the knock (`challenge` with the bridge's `gid`, `wait`, `no`, `ok` with
`token`), then opens the connection as v1 does.

**On the console**: while a room is open the engine tells the helper about it (`play.room`, every
change and every 30 s; `{room: null}` when it closes) for Minion and PS5Upload Play.

**Limits that leave people alone**: the guest socket, the route probe, the preview, the bridge,
PS5Upload Play's socket and the LAN discovery probe take per-client token buckets (an IPv4 address
or IPv6 /64) with a burst well above what a person does; past one the answer is 429 with
`Retry-After`; only a client that keeps going for 30 s after being told is blocked for 2 minutes,
doubling up to 30. This computer is never limited.

## Play Rooms chat and voice

People in a room can type, send quick messages and talk. Everything goes through the host's engine
over each person's existing connection: a guest's `/api/join/ws` socket (or its internet data
channel), and the host page's `/api/ps5/rp/ws` socket. Nothing goes through the hub. Nothing is
recorded or stored: chat lines live in memory for the room's life (the last 50), voice only for the
60 ms a frame may wait. Bug reports carry counts only.

Nobody sends or gets chat or voice until they accept the responsibility notice. The client sends the
version it accepted; until then the engine refuses with `"why": "notice"` and the version, and the
client shows the notice. Rooms with chat and voice off never need it.

**Client → engine** (text JSON; on the host page's socket the mic key is `room_mic`, because `mic`
there is already the console's own microphone):

| Message | Meaning |
|---|---|
| `{"notice": 1}` | The person accepted the notice of this version. |
| `{"chat": {"quick": 1-16}}` | A quick message by id. Each client shows it in its own language: Ready? · Ready! · GG · One more? · Brb · Wait · Let's go · Thanks · Nice! · Sorry · Pause please · Lag on my side · Can't hear you · Switch seats? · Bye · 👍 |
| `{"chat": {"text": "…"}}` | Typed text: one line of 1–140 characters, emoji allowed. Links keep their words with dots shown as `·` (never clickable); in a Public room invite codes become `···`. |
| `{"mic": "on" \| "off" \| "ptt_down" \| "ptt_up"}` | The mic switch (off on every join) and the push-to-talk key. `ptt_down` needs the mic on. |
| `{"flag": sid}` | "Tell the host": the host's view gets one line, once per person per room. |
| binary `[0x0A][seq u16 BE][level u8][Opus 1–255 B]` | One voice frame: Opus, voice, mono, wideband, 20 ms (40 ms at 12 kb/s), DTX and in-band FEC on. `level` is 0 (silence) to 255. Send nothing while silent. |

An internet guest's page may open a second data channel labelled `voice` (unordered,
`maxRetransmits: 0`) and send its frames there; the engine then sends the bundles there too.
Without it, voice rides the `join` channel and a bundle that would wait behind pictures is dropped.

**Engine → client:**

| Message | Meaning |
|---|---|
| `{"comms": {"notice": 1, "accepted": bool, "chat": "off\|quick\|on", "voice": "off\|ptt\|open", "caps": ["chat", "voice"]}}` | On joining and whenever the host changes a setting. |
| `{"voice": {"mode", "you": sid, "mic": "on\|off", "ptt": bool, "paused": null \| "poor", "stepped": bool, "kbps": 24\|12\|0, "frame_ms": 20\|40\|0, "speaking": [sid], "people": [{"sid", "name", "player", "host", "muted", "paused", "mic"}]}}` | The full voice state, on joining and every change. Encode at `kbps` and `frame_ms`. |
| `{"voice": {"speaking": [sid]}}` | Only who is speaking changed (at most 4 a second). |
| `{"muted": {"voice": bool, "chat": bool}}` | The host's mutes on you. |
| `{"chat": {"id", "sid", "from", "player", "host", "t", "quick" \| "text", "x"?}}` | A line, to everyone who accepted (the sender too). `x` > 1: the same line again from the same person; replace the line with this `id`. |
| `{"chat_history": [line, …]}` | The last 20 lines, after accepting the notice. |
| `{"comms_refused": {"what": "chat\|voice\|mic\|notice", "why", "notice"?, "retry_ms"?}}` | `why`: `notice`, `off`, `quick_only`, `muted`, `slow_down` (with `retry_ms`), `empty`, `too_long`, `bad_quick`, `ptt_only`, `mic_off`, `poor`. At most one per reason a second (voice: every 5 s). |
| `{"flagged": {"by", "by_sid", "who", "who_sid"}}` | Host views only. |
| binary `[0x0B][n u8]` then `n` × `[sid u8][seq u16 BE][len u8][Opus]` | One bundle per listener: every speaker they hear in this tick. |

`sid` is a speaker id for the room's life (the host is 0); the `people` list names them. The engine
forwards frames as they came and never mixes: each listener hears at most 3 speakers (a fourth
waits until one stops) and never themselves, and mixes on their own device. Bundles go every 20 ms,
or every 40 ms when the listener's route is Playable or pictures are waiting; a bundle goes at once
when it already holds a frame from everyone the listener hears.

Voice follows each person's route tier: Excellent and Good 24 kb/s at 20 ms, Playable 12 kb/s at
40 ms, Poor paused both ways. Going back up needs 10 s at the better tier; if the tier fell within
10 s of voice starting, voice steps down one more level for 30 s (5 minutes the second time). The
room's voice upload is capped at 0.8 Mb/s, the quietest speaker dropped first. On every link the
order is controller input, then sound and pictures, then voice, then chat.

Typed lines: a burst of 5, then one every 3 s. Voice frames: a burst of 25, then 60 a second.

**Host routes** (this computer, or `PS5UPLOAD_ALLOW_IP`):

```sh
# Room settings: chat off | quick | on, voice off | ptt | open, new players start muted.
curl -s localhost:19113/api/join/comms -H 'content-type: application/json' \
  -d '{"addr": "192.168.1.20", "chat": "quick", "voice": "ptt", "start_muted": true}'
# Mute Player 2's voice (or {"sid": 3}, or {"all": true}); "chat": true mutes their chat.
curl -s localhost:19113/api/join/mute -H 'content-type: application/json' \
  -d '{"addr": "192.168.1.20", "player": 2, "voice": true}'
# The host's own quick message, and mic switch ("notice" once, when accepted on this device).
curl -s localhost:19113/api/join/chat -H 'content-type: application/json' \
  -d '{"addr": "192.168.1.20", "quick": 3, "notice": 1}'
curl -s localhost:19113/api/join/voice -H 'content-type: application/json' \
  -d '{"addr": "192.168.1.20", "mic": "on"}'
```

Refusals are 409 with `{"error": why, …}` as above (`not_open` when no room is open); `mute`
answers 404 `no_player` and 409 `host`. `GET /api/join` adds, under `room`, `chat`, `voice`,
`start_muted`, `notice`, `voice_kbps: {in, out}`, `speaking`, `chat_log` (the last 50 lines) and
`host_voice`, and on each guest `muted: {voice, chat}` and `voice: {sid, mic, speaking, paused,
stepped, kbps, in_kbps, out_kbps, frames_in, frames_out, dropped, accepted}`.

## What the API can't do

The API covers what the engine does. Some things in the app aren't the engine's, so no route reaches
them:

| Feature | Where it lives | What you can do instead |
|---|---|---|
| **Schedules** | In the app window, and they fire only while it's open. | Run your own timer and call the routes. |
| **Play time** | In each browser or app's own storage. | `GET /api/activity`, `GET /api/ps5/activity/get`, `GET /api/event-journal` have the raw material. |
| **The notification inbox** | Likewise per device. | `GET /api/ps5/notif/list` is the helper's log; `GET /api/event-journal` is the engine's. |
| **A phone as a controller** | `/pad` and `/api/pad/ws`, for a person holding the phone that scanned the code. | `POST /api/ps5/rp/input` drives the session from a script. `/api/ps5/rp/pad-link` opens and closes the link. |
| **A guest joining a Play Room** | `/join` and `/api/join/ws`, for a person who has the invite; `/api/join/bridge`, for PS5Upload Play on a PS5 joining an internet invite through this engine. | `/api/join/*` is the host's side: open, invite, let in, let go; `/api/join/friends` lists the guests' PS5s that may ask to play again, forgets one, and sends one the invite. |
| **PS5Upload Play on the host's PS5** | `/api/play/app`, for the app on the console that paired with a code shown on the TV, or with a one-time grant the console's helper handed it (the helper gets it from this engine over AVA1, `play.grant`). | `POST /api/play/app/pair` confirms that code typed in; `POST /api/play/app/pair_confirm {console}` confirms it with one click (the `play` event `pair_waiting` and `GET /api/play/app/paired` show the code to compare); `POST /api/play/app/moved {console_old?, addr, ok}` confirms a paired console that moved to a new address (the `play` event `moved`; a console whose AVA1 key proves it moves by itself); `GET /api/play/app/paired` and `POST /api/play/app/unpair` list and remove paired consoles. `GET /api/play/app/installed`, `POST /api/play/app/install`, `/remove` and `/open` put the app this build carries on a PS5, update it, take it off and start it. |
| **A friend receiving a Share in a browser** | `/share` and `/api/share/ws`, for a person with the link on the same network. | `/api/share/receive` receives one engine to another. |
| **Your file picker, drag and drop, keep-awake, game mode, screenshots of the app, controller haptics, the app's updater** | The device itself. | Name paths directly. `GET /api/engine/update` says if there's a new release. |

Other limits worth knowing:

- **The Remote Play socket.** `/api/ps5/rp/ws` is the app's own protocol. Use `POST /api/ps5/rp/start`
  and the routes in [AUTOMATION.md](AUTOMATION.md).
- **The picture needs a decoder.** `GET /api/ps5/rp/frame` takes the picture from an open app page.
  With no page attached it decodes the stream with ffmpeg, which `POST /api/stream/tools/ffmpeg/install`
  can fetch. Without either you get an error saying so.
- **Picture off on this device.** `POST /api/ps5/rp/picture {host, session?, on}`, or `picture` on
  `POST /api/ps5/rp/settings`, stops the page's picture, sound and controller for the session while
  guests, phones and the live feed carry on. `GET /api/ps5/rp/session` says `picture`, and
  `GET /api/ps5/rp/frame` answers `409 picture_off` until it's back on.
- **Keys don't come back.** No route hands a console's Remote Play or wake key to anyone but the app.
  Stream keys for Go live show only their last four characters.
