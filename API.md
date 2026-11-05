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
| `host-checked` | Play Together's and Go live's host routes. This computer, addressed by a loopback name or one in `PS5UPLOAD_ALLOW_HOST`, and the `PS5UPLOAD_ALLOW_IP` addresses. |
| `lan-open` | `/pkg-host/*` (package bytes for the console), `/pad`, `/api/pad/ws`, `/join`, `/api/join/ws` (a phone or a friend, which check their own key or invite). |
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

The HearBridge route `POST /api/ps5/hearbridge/action` is the one exception: `addr` there means a
headset, and the console is `host` or `console`.

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
  `upload_file`, `upload_dir`, `upload_zip`, `upload_7z`, `upload_rar`, `upload_file_list`,
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

## Route families

Each operation's first tag is its family. `GET /api/openapi.json` has every route; these are the
places to start.

| Family | Routes | What it's for |
|---|---|---|
| `engine` | `/api/version`, `/api/events`, `/api/event-journal`, `/api/engine-logs`, `/api/diag/*`, `/api/metrics`, `/api/openapi.json`, `/api/mgmt/call` | The engine itself. `/api/mgmt/call` is the raw helper call, for when nothing else fits. |
| `consoles` | `/api/consoles`, `/api/discover`, `/api/ps5/status`, `/api/ps5/readiness`, `/api/ps5/volumes` | Which consoles there are and how they stand. |
| `console-files` | `/api/ps5/list-dir`, `/api/ps5/fs/*`, `/api/ps5/disk-usage`, `/api/ps5/console-shares` | Files on the console. |
| `transfer` | `/api/transfer/*`, `/api/files/upload`, `/api/link/*`, `/api/zip/*`, `/api/7z/*`, `/api/rar/*` | Uploads and downloads. |
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
| `workshop` | `/api/fpkg/*`, `/api/exfat/*`, `/api/folder/*`, `/api/fakelibs/*`, `/api/ampr/*`, `/api/backport/*` | Package conversion, images, backporting. |
| `collection`, `this-computer` | `/api/collection/*`; `/api/local/*` | Game folders and files on this computer. |
| `mesh` | `/api/mesh/*`, `/api/drive/*` | Devices sharing a console, and Mesh Drive. |
| `remote-sources` | `/api/remote/*` | NAS and other servers. |
| `settings`, `users`, `pairing` | `/api/settings`, `/api/shared/*`; `/api/profile/*`; `/api/ava1/*` | Settings, console users, pairing. |
| `console-tools`, `hearbridge`, `shadowmount` | `/api/ps5/shell/run`, `/api/ps5/klog`, `/api/ps5/net/*` and the rest of `/api/ps5/*`; `/api/ps5/hearbridge/*`; `/api/ps5/smp/*` | Everything else on the console. |

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

## What the API can't do

The API covers what the engine does. Some things in the app aren't the engine's, so no route reaches
them:

| Feature | Where it lives | What you can do instead |
|---|---|---|
| **Schedules** | In the app window, and they fire only while it's open. | Run your own timer and call the routes. |
| **Play time** | In each browser or app's own storage. | `GET /api/activity`, `GET /api/ps5/activity/get`, `GET /api/event-journal` have the raw material. |
| **The notification inbox** | Likewise per device. | `GET /api/ps5/notif/list` is the helper's log; `GET /api/event-journal` is the engine's. |
| **A phone as a controller** | `/pad` and `/api/pad/ws`, for a person holding the phone that scanned the code. | `POST /api/ps5/rp/input` drives the session from a script. `/api/ps5/rp/pad-link` opens and closes the link. |
| **A guest joining Play Together** | `/join` and `/api/join/ws`, for a person who has the invite. | `/api/join/*` is the host's side: open, invite, let in, let go. |
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
