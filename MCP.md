# PS5Upload and AI agents (MCP)

PS5Upload's engine has an [MCP](https://modelcontextprotocol.io) server built in, so an AI agent such
as Claude, Cursor, Codex or Gemini can drive your PS5 with tools it already understands. There is
nothing extra to install: it is there whenever the engine runs, in the desktop app or the Docker web
UI.

```
http://127.0.0.1:19113/mcp
```

The agent acts with your consoles, so read [Safety](#safety) before you set one loose. It needs the
PS5Upload app open, or an engine running: the MCP server is part of the engine and never starts one
of its own.

## What it can do

More than 120 tools, all of them thin layers over [the engine's API](API.md):

| Area | Tools (prefix) | For example |
|---|---|---|
| The engine | `engine_*`, `logs_*` | Version, status, recent events; log levels per category and the newest log lines |
| Consoles | `consoles_*`, `tailnet_consoles` | The known ones, find them on the network or on Tailscale, how one stands and how its last status checks went, make sure the helper runs |
| Files | `files_*` | List, read, write, copy, move, delete, hash, extract, mount, eject |
| Moving things | `upload_*`, `queue_*`, `files_download`, `sources_*`, `jobs_*` | Send a file or folder or archive, line work up per console, follow it, pause or stop it, fetch from a NAS |
| Installs | `install_*` | Install a package by the best route, check first, wait for it |
| Games | `games_*`, `processes_*`, `saves_*`, `cheats_*` | Installed titles, launch and stop, back up saves, cheats |
| Hardware | `hw_*`, `power_*`, `notifications_*`, `health_*`, `disk_usage_*`, `cleanup_*`, `shell_run` | Temperatures, rest and wake, health checks and repairs, what fills the drive |
| Payloads | `payloads_*` | Catalogue, download, updates and rollback, send, store, autoload, Payload Manager sync |
| Remote Play | `rp_*` | Start and stop a session, see the picture, press buttons, type, answer a passcode; see what the controller sends and how pairing went |
| Friends and viewers | `together_*`, `golive_*`, `share_*` | Open Play Together and invite; go live; share an app's things with a friend |
| Everything else | `settings_*`, `devices_list`, `drive_*`, `api_catalog`, `api_describe`, `api_request` | Settings, Mesh Drive, and any route of the API |

**Play Together has one invite.** `together_invite` gives one link (`share.url`) that works on your
Wi-Fi and over the internet; a friend on your Wi-Fi stays on it. Without internet, or when the invite
service is down, it gives your Wi-Fi's link (`share.works: "wifi"`, `share.why: "offline"`). What
changed for agents:

- `together_invite` follows "Invites work" unless you pass `internet`. `internet: true` still asks
  for the internet link, and `internet: false` for your Wi-Fi's.
- `together_settings` takes `invites`: `anywhere` (the default) or `wifi`. The setting is kept
  across restarts. `internet_invites` still works and means the same: on is anywhere, off is Wi-Fi
  only.
- `together_status` gives each friend `link.path` (`lan`, `tailscale` or `internet`) and
  `link.rtt_ms`.

**Finding and checking consoles.** `tailnet_consoles` lists the PS5Upload consoles on this
device's Tailscale network (or probes one `addr`): tailnet and home address, whether its helper
answered, its identity key (`id`) and its entry on the console list. Asking pairs nothing and shows
nothing on the PS5. `consoles_connect_timings` gives the last status check of each console phase by
phase (session, connect, status call, path), for "why did it take so long to show as connected";
it sends nothing to a console. Both are reads, so they stay in read-only mode.

**Share sends an app's things to a friend.** `share_create` makes a link (homebrew apps only: a
retail game, its updates and add-ons are refused by the engine in any format; saves of any game,
screenshots and clips, cheats, controls, stream settings and a note can go too). The friend opens it
and asks; `share_decide` allows or declines each request, and only then do files go, directly from
this computer to theirs. `share_status` shows the shares, requests, receives and history without
the links; `share_cancel` stops one.

`api_catalog` finds a route, `api_describe` gives its fields and answers, and `api_request` calls it,
so what the other tools don't cover is still reachable. The only exceptions hold console keys, and
`api_request` refuses them: see [What never reaches a model](#what-never-reaches-a-model).

It also offers:

- **Resources:** `ps5upload://catalog` (the routes), `ps5upload://openapi` (the whole OpenAPI
  document), `ps5upload://docs/api`, `ps5upload://docs/mcp`, `ps5upload://docs/automation`,
  `ps5upload://consoles`, `ps5upload://consoles/{addr}/status`, `ps5upload://jobs`,
  `ps5upload://events`.
- **Prompts:** `install_game`, `diagnose_connection`, `free_up_space`, `play_session`.

Your client's own list (`tools/list`) is always the current one, and each tool says what it needs.

## Set it up

**The easy way:** open **MCP & API** in the app's sidebar. It finds Claude Code, Codex and OpenCode on
your computer and adds this server with one click, using the engine's real address. It also says if an
entry has gone stale because the engine moved port, shows every tool, and has the commands below ready
to copy.

### Keeping it up to date

- **When the app updates, do nothing.** Your agent's entry only holds the address, and the tools come from the engine. New tools show up the next time you start a session in the agent; a session already open keeps its old list. Ask the agent which version the engine is, to see what it's connected to.
- **If the engine's address changes,** for example because something else took port 19113, the app tells you when it starts and **MCP & API** shows **Update** on that agent. By hand, run the add command again with the new address (Claude Code first needs `claude mcp remove --scope user ps5upload`).
- **The app can't see other clients,** such as Gemini CLI, Cursor or Claude Desktop, so copy their setup again if the address changes. The stdio bridge starts the engine program from the app's folder, and an update replaces that program in place. If Claude Desktop stops connecting after an update on Windows, copy its setup again, as the program's name [can change there](#where-the-engine-program-is).

To do it by hand, the address above works with any client that speaks HTTP. For a client that can only start a program,
or that doesn't run on your computer, use the [stdio bridge](#the-stdio-bridge).

If the engine isn't on port 19113, change the port in what follows: see
[Reaching it](API.md#reaching-it).

### Claude Code

```sh
claude mcp add --transport http ps5upload http://127.0.0.1:19113/mcp
claude mcp list
```

`claude mcp list` should say `Connected`. To share it with a project, put this in `.mcp.json`:

```json
{ "mcpServers": { "ps5upload": { "type": "http", "url": "http://127.0.0.1:19113/mcp" } } }
```

### Claude Desktop

Not run here (see the table at the end). Use the stdio bridge, not a custom connector. Custom connectors run from Anthropic's cloud, so they
can't reach `localhost` on your computer.

Edit `claude_desktop_config.json` (**Settings → Developer → Edit Config**) and restart Claude Desktop:

```json
{
  "mcpServers": {
    "ps5upload": {
      "command": "<engine path>",
      "args": ["mcp"]
    }
  }
}
```

`<engine path>` is [where the app keeps its engine](#where-the-engine-program-is). On Windows, double
the backslashes in JSON.

### Cursor

Not run here (see the table at the end). In `.cursor/mcp.json` (one project) or `~/.cursor/mcp.json` (everywhere):

```json
{ "mcpServers": { "ps5upload": { "url": "http://127.0.0.1:19113/mcp" } } }
```

Or with the bridge:

```json
{ "mcpServers": { "ps5upload": { "type": "stdio", "command": "<engine path>", "args": ["mcp"] } } }
```

### Codex

```sh
codex mcp add ps5upload --url http://127.0.0.1:19113/mcp
```

Which writes this to `~/.codex/config.toml`:

```toml
[mcp_servers.ps5upload]
url = "http://127.0.0.1:19113/mcp"
```

Or with the bridge: `codex mcp add ps5upload -- "<engine path>" mcp`.

### OpenCode

```sh
opencode mcp add ps5upload --url http://127.0.0.1:19113/mcp
opencode mcp list
```

Which writes this under `"mcp"` in `~/.config/opencode/opencode.json` (or `.jsonc`, keeping its comments):

```json
{ "ps5upload": { "type": "remote", "url": "http://127.0.0.1:19113/mcp" } }
```

OpenCode has no remove command: to remove it, delete that entry.

### Gemini CLI

```sh
gemini mcp add --scope user --transport http ps5upload http://127.0.0.1:19113/mcp
gemini mcp list
```

Without `--scope user` it writes to the current folder, which must be a trusted one. Or with the
bridge: `gemini mcp add --scope user ps5upload "<engine path>" mcp`.

### MCP Inspector

For trying things by hand, or checking the server works:

```sh
npx -y @modelcontextprotocol/inspector --cli --transport http \
  --server-url http://127.0.0.1:19113/mcp --method tools/list
npx -y @modelcontextprotocol/inspector --cli --transport http \
  --server-url http://127.0.0.1:19113/mcp --method tools/call --tool-name engine_version
```

Leave out `--cli` for its web page.

### Any other client, or plain curl

```sh
curl -s -X POST http://127.0.0.1:19113/mcp -H 'content-type: application/json' \
  -H 'MCP-Protocol-Version: 2025-06-18' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

The server answers both the current protocol (2026-07-28) and the older `initialize`-based revisions
(2024-11-05 to 2025-11-25). It keeps no session, and `GET /mcp` is a `405` because it has no stream of
its own to offer.

## The stdio bridge

```sh
ps5upload-engine mcp [--url http://127.0.0.1:19113]
```

It passes each line of a client's stdin to the running engine's `/mcp`, and each answer back to
stdout. The address comes from `--url`, else `PS5UPLOAD_ENGINE_URL`, else `http://127.0.0.1` with the
port in `PS5UPLOAD_ENGINE_PORT` (19113 if unset).

It is only a bridge. If no engine answers, your client gets an error saying to open the app. It never
starts a second engine, because two would fight over the consoles.

### Where the engine program is

The desktop app unpacks its engine here the first time it runs, so open the app once first:

| System | Path |
|---|---|
| macOS | `~/Library/Application Support/com.phantomptr.ps5upload/engine/ps5upload-engine` (write out your home folder: a client won't expand `~`) |
| Windows | `%LOCALAPPDATA%\com.phantomptr.ps5upload\engine\ps5upload-engine.exe` (write out the path) |
| Linux | `~/.local/share/com.phantomptr.ps5upload/engine/ps5upload-engine` (or under `$XDG_DATA_HOME`) |
| Docker | `/ps5upload-engine` in the container, though the bridge only reaches an engine it can address: `docker exec -i <container> /ps5upload-engine mcp` |
| Built yourself | `engine/target/release/ps5upload-engine` |

On Windows, if an older engine held that file locked, the app uses `ps5upload-engine-<hash>.exe`
beside it. Phones have no such program: their engine lives inside the app.

## Safety

There is no password ([why](API.md#no-password-on-purpose)). `/mcp` is behind the same guards as the
API: it answers this computer and any address in `PS5UPLOAD_ALLOW_IP`, and a web page can't reach it.
**An agent connected to it can do whatever you can**, including deleting files on your console.

### Read-only mode

In the app, turn off **Agents can change things** under MCP & API › Connect an agent. It applies at
once, no restart: the tools that change something leave the tool list and refuse, and `api_request`
allows only `GET`. Reads go on working: an agent can still look at your files, read your save data's
contents and see the picture of a session. It applies to MCP only; the HTTP API itself doesn't change.
A client that listens (`subscriptions/listen` with `toolsListChanged`) hears
`notifications/tools/list_changed` when it flips.

An agent can't turn it back on: `settings_set` and `api_request` refuse the switch
(`/api/mcp/access`), however it is spelt. That stops agents using PS5Upload's tools; a program that
can run commands on this computer, an agent's own shell included, could still reach the engine
directly, since the API has no password.

To hold it on whatever the app says, set this in the engine's environment and restart it. The app
then shows the switch off and locked:

```sh
PS5UPLOAD_MCP_READ_ONLY=1
```

### Tools that can lose something or interrupt you

A client that follows the MCP annotations asks you before these (`destructiveHint: true`). Most
clients let you allow tools one by one, so keep these on "ask".

| Tool | What it can do |
|---|---|
| `files_delete`, `files_move`, `files_copy`, `files_write` | Delete or overwrite files on the console |
| `files_extract` | Unpack over what's already there |
| `files_unmount`, `files_eject` | Pull a drive out from under a running game |
| `upload_file`, `upload_dir`, `upload_archive`, `queue_add` | Overwrite what's at the destination |
| `saves_restore` | Replace save data with a backup |
| `games_unregister` | Remove a title from the console |
| `games_stop`, `processes_kill` | End a game, losing what isn't saved |
| `power_control` | Rest, reboot or shut down |
| `shell_run` | Run any command on the console, and read any file there, sign-in keys included |
| `payloads_send` | Run a program on the console, with the same reach as a shell |
| `payloads_store`, `payloads_delete`, `payloads_autoload`, `payloads_pm_sync`, `payloads_update` (with `sync_console`) | Change what the console keeps and runs at boot |
| `cleanup_apply`, `health_fix` | Delete what a scan found, and apply repairs |
| `rp_stop` | End the Remote Play session someone may be playing |
| `together_kick`, `together_close` | Send friends out of the game |
| `together_owner_access` | Let a friend play as your own PS5 profile (your saves and trophies): Player 1 on the stream's controller, or your profile for a guest player. `together_settings` refuses both |
| `golive_start` | Stream your session in public |
| `golive_destination_remove` | Forget a saved stream key |
| `share_create`, `share_decide` | Make a Share link, and send the files to a friend who asked |
| `share_cancel` | Stop a share and its transfers |
| `api_request` | Anything the API can do |

Some tools aren't marked destructive but still matter: `install_pkg`, `games_launch`,
`rp_input`, `rp_start`, `together_invite`, `together_decide`, `settings_set`,
`console_settings_set`.

**App settings and each console's own.** `settings_get` / `settings_set` are the app's settings.
Some settings belong to one console (Settings › This PS5): `wake_for_uploads`, `keep_awake_mode`
(off, transfers or always), `rest_after_upload`, `auto_bring_up`, `auto_redeploy_on_wake` and
`install_path_fallback`. `consoles_list` shows each console's own as `settings`, and
`console_settings_set` changes them for one console (null goes back to the app's default). The app
setting of the same name is that default: changing it with `settings_set` does not change a console
that has its own value.

### What never reaches a model

The keys that sign in to a console's users stay out. `api_request` refuses `/api/consoles/document`,
`/api/settings-backup/get`, `/api/mgmt/call` and `/api/ava1/identity`, and they are left out of the
catalog too. No tool reaches the console documents that hold keys (`state`, `consoles`, `drives` and the
backup slots), through `/api/mesh/doc` or by the helper's file path; the other documents stay readable.
Keys are taken out of every answer a tool gets: `POST /api/ps5/power/pair` still pairs and stores them,
and a Mesh Drive's password hash reads as `has_password`. `/api/consoles` says only whether a console has
keys. Stream keys for Go live show only their last four characters. A live Play Together invite (its key, code and
links) and the phone controller link (its QR address lets a phone on your network in without your
Allow) are taken out of the status reads, `together_status` and a `GET /api/join` or `GET
/api/ps5/rp/pad-link` through `api_request`, even when agents may only look: those answers keep
whether it is open, when it expires and who is in. `together_invite` still answers with the link,
because handing it out is what you asked for, and you still approve who joins. A Tailscale sign-in
link (`login_url`) never reaches a tool: whoever opens it first could put this computer or your
PS5 on a tailnet of their own. Sign in from the app. A Share link (its `url`, `lan_url` and `room`:
whoever has the room can ask you for the files) is taken out of every read of `/api/share`
(`share_status`, `api_request`), which says `link_hidden`; `share_create` still answers with it.
A console's identity key (`id` in `tailnet_consoles`, `key` from `GET /api/ava1/console`, the
device ids in `/api/mesh`) is public: every console shows it to anyone who connects, and it lets
nobody in, so it isn't taken out. The app's **Invite a device** link is made in the app from those
same public facts; no route hands one out. Allowing or declining a device that asks to connect
(`POST /api/mesh/device`), adding a console found on Tailscale (`POST /api/tailnet/consoles/add`) and
the Tailscale list's Allow (`POST /api/ps5/tailnet`) are POSTs, so read-only mode refuses them.

**Logs.** Engine log lines and journal events are cleaned where they are written, and again when an
older journal line is read: a password in a URL, a field or `name=value` named as a key, token, password
or secret, a bearer token, a pairing PIN, and the key at the end of an `rtmp://` address. The console's kernel log and
syslog are cleaned the same way when read, and so are the error messages in any tool's answer. This goes
by shape, so a key with no name beside it can't be recognised: the app never logs one.

**Looking into a problem.** `logs_level_get` shows how much each category (`rp`, `install`,
`transfer`, `files`, `cleanup`, `convert`, `pairing`, `wake`, `helper`, `connection` and the rest)
writes. `logs_level_set` raises one or more, for example `{"categories": {"rp": "trace"}, "merge":
true, "minutes": 30}`; anything above `info` falls back to `info` by itself when the minutes are up,
on the engine, in the app and on the console's helper. Then reproduce the problem and read
`logs_recent` with `level`, `category` and `minutes` (or `since_seq` to read on). `{"reset": true}`
puts everything back. The same is in the app under Settings › Logs and on the report screen.

**The console's own files.** File tools reach what the helper does, which includes everything under
`/user` and `/data`: saves, game data, the console's account data. That is your console's data, not this
app's, and a file read can't tell a secret in it from anything else, so it isn't filtered. This app keeps
no keys under `/user` apart from `/user/data`, which is `/data` under another name and is held to the
same rules (as is `/original_user/data`): the helper refuses file reads of its AVA1 identity and the saved shares' keys, though `stat` and `list-dir` still show their names and sizes, and no tool reads the PSN
sign-in copies in `np-backup`. Those rules go by path, so copying a whole folder to read it elsewhere is
a step around them; copying is a tool that asks for confirmation for that reason.

**A shell on the console has everything.** `shell_run`, and `api_request` calling `/api/ps5/shell/run`, can read any file on the console, including the files the rules above keep other tools from. Nothing filters what a command reads, and no filter could. `payloads_send` runs a program there, which can do the same. So these tools are marked destructive, and read-only mode refuses them. Keep them on "ask" and read each command before you allow it, or turn them off in your client.

## Things that need more than the engine

**Seeing the picture.** `rp_frame` takes it from the PS5Upload page showing the session. With no page
attached, or a hidden window, it decodes the stream with ffmpeg, at most once a second. Without ffmpeg
the answer says how to get it (`POST /api/stream/tools/ffmpeg/install`, or set `PS5UPLOAD_FFMPEG`). A
page gives the freshest picture, so keep the app's Play screen open if the agent is playing.

With the picture off on this device (`POST /api/ps5/rp/picture`), `rp_frame` answers `picture_off`
until it's back on.

**Starting a session.** `rp_start` needs a console paired for Remote Play in the app, because that is
where its keys live. A console can be paired as several PS5 users (one pairing each): `consoles_list`
shows their names as `rp_users`, and `rp_start` takes `user` to pick one. Left out, it uses the user
chosen in the app. `rp_session` says which user a stream signed in.

**Checking the controller and pairing.** `rp_controller` shows what the PS5's controller was last
sent in the running (or last) session, per controller: buttons, L2/R2 from 0 to 255, sticks, the
trigger peaks, and `triggers_analog` (false when only 0 or 255 ever came). For the app's own
controller it also says which controllers it reads and how. Ask the player to half-press L2 and look
for about 128. `rp_pair_attempts` lists the last 5 pairing tries with the PS5's refusal code and what
the console last said about its signed-in user and Remote Play switch. Both only read what the engine
already holds in memory; neither shows a PIN or a key.

**Going live.** `golive_start` needs ffmpeg and a running session. Add destinations and their stream
keys in the app.

**This computer only.** The Remote Play tools answer an agent on the engine's own computer. An agent
on another machine needs `PS5UPLOAD_ALLOW_AUTOMATION=1` as well as `PS5UPLOAD_ALLOW_IP`
([details](AUTOMATION.md#what-it-does-and-what-it-never-does)).

**Long jobs.** Uploads and installs return a job at once; `jobs_wait` and `install_wait` follow it
with a time limit. `queue_add` lines work up instead, `queue_wait` follows it, and [the queue's
known gaps](API.md#the-queue) apply.

**Not reachable at all.** Schedules, play time, the notification inbox, a phone as a controller, and a
friend's side of Play Together live in the app or on that device. See
[What the API can't do](API.md#what-the-api-cant-do).

## What was tested

Run against an engine on a private port, with no console attached, on macOS, 2026-11-04:

| Client | What ran | Result |
|---|---|---|
| Claude Code 2.1.318 | `claude mcp add --transport http`, the same with the bridge, `claude mcp list`, then `claude -p` calling `engine_version` | Connected both ways, and the call answered |
| Gemini CLI 0.66.0 | `gemini mcp add --scope user --transport http`, the same with the bridge, `gemini mcp list` | Connected both ways |
| MCP Inspector CLI | `tools/list`, `tools/call`, `resources/list` over HTTP; `tools/list` over the bridge | All answered |
| Codex 0.166.0 | `codex mcp add --url`, the same with the bridge, `codex mcp list`, then `codex exec` calling `engine_version` | Config saved, and the call answered |
| OpenCode 1.19.5 | `opencode mcp add --url`, `opencode mcp list` | Connected. No tool call: its model wasn't answering here |
| curl and the bare bridge | Both protocol eras, read-only mode, no engine running | As this page says |
| Claude Desktop, Cursor | Not installed here. Their setup above is untested | **Not run** |

Tools that need a console were not part of this run.
