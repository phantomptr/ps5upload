# Deploying the hub on Fly.io (recommended)

What you get: one small machine (`shared-cpu-1x`, 256 MB) running the hub's container, stopped
by Fly while nobody is connected and started again (about a second) by the next connection,
HTTPS on `hub.phantomptr.com` with a certificate Fly renews, and nothing else. STUN is off
(clients use stun.cloudflare.com and stun.l.google.com), so no UDP service and no dedicated IPv4
are needed. Everything below runs on your computer.

The app must stay on **one machine**: rooms live in that machine's memory, and a second machine
would not see them. Fly creates two machines by default; the steps below prevent that.

## 1. Install flyctl and sign in

```sh
brew install flyctl
fly auth login
```

## 2. Create the app (no deploy yet)

From this crate's directory (it holds `fly.toml` and the `Dockerfile`):

```sh
cd engine/crates/ps5upload-hub
fly launch --copy-config --no-deploy
```

Answer the prompts by keeping what `fly.toml` says: app name `ps5upload-hub` (if that name is
taken, pick another; flyctl writes it into `fly.toml`), region `iad`. Say **no** to any
database, Redis or other add-on it offers.

## 3. Deploy

```sh
fly deploy --ha=false
fly scale count 1 --yes
fly status
```

`fly deploy` builds the image on Fly's builders (no Docker needed on your computer).
`fly status` should show one machine, `started`, with a passing check. Quick test:

```sh
curl https://ps5upload-hub.fly.dev/health      # prints ok
```

Later deploys: just `fly deploy --ha=false` from the same directory. The hub and the app may be
updated in either order: a newer hub takes older apps' rooms as before, and an older hub takes a
newer app's. Only rooms of an updated app on an updated hub are bound to their host (nobody else
can open them after a restart; PROTOCOL.md, Identifiers).

## 4. The hub.phantomptr.com name

```sh
fly certs add hub.phantomptr.com
```

flyctl prints the DNS records it wants. In GoDaddy (My Products, phantomptr.com, DNS,
Add record), add what it asks for. Usually that is:

- type `CNAME`, name `hub`, value `ps5upload-hub.fly.dev` (your app name + `.fly.dev`), TTL 600.

If flyctl asks for `A`/`AAAA` records instead, add those: type `A`, name `hub`, the IPv4 it
shows, and type `AAAA`, name `hub`, the IPv6 it shows (`fly ips list` shows them too; the shared
IPv4 costs nothing). If it also asks for a `_acme-challenge.hub` CNAME, add that as well.

Then check until the certificate is issued (a few minutes after DNS is in place):

```sh
fly certs show hub.phantomptr.com
curl https://hub.phantomptr.com/health          # prints ok
```

## Logs

```sh
fly logs                 # live
fly logs --no-tail       # recent
```

The hub logs only counts and error kinds, never addresses, room ids or payloads, and only a few
lines: `started`, `stopped`, and an hourly `hub: summary rooms=... errors=... refused=... strays=...` when
something happened in that hour (an idle hub logs nothing). Bad frames and refusals are counted
in `/metrics`, not logged one by one.

## Cost

Fly bills a machine by the second while it runs (`shared-cpu-1x` 256 MB: $2.19 per 30 days if it
never stops, docs.fly.io/about/pricing). `fly.toml` lets Fly stop it when no socket is open
(`auto_stop_machines = "stop"`, `min_machines_running = 0`), so the bill follows the hours in
which someone has an internet invite open. A host's engine starts it again when it connects; a
guest never waits for that, because a guest only joins a room whose host is connected. The
shared IPv4 and the IPv6 are free; do not add a dedicated IPv4 ($2/month): the hub needs none.
Outbound data is $0.02/GB and the hub sends a few kilobytes per guest.

To keep it always on instead (no start delay for hosts, the full $2.19), set
`auto_stop_machines = "off"` and `min_machines_running = 1`.

## Metrics

`/metrics` is not on the public port. Fly scrapes it over the private network (the `[metrics]`
section of `fly.toml`; it shows in the Fly dashboard's Metrics / Grafana). To read it yourself:

```sh
fly proxy 9091           # leave running, then in another terminal:
curl -s http://127.0.0.1:9091/metrics
```

Reading it:

- `hub_hosts_total` counts rooms opened, not connections: every Play Together internet invite,
  phone link and Share opens one. A host that reconnects within 2 minutes takes its room back
  (`hub_reclaims_total`); rooms go when their host closes them (`hub_rooms_closed_total`) or
  stays away 2 minutes (`hub_rooms_expired_total`: a computer asleep, an app quit).
- `hub_limited_total{limit=...}` says which limit refused something, beside the
  `hub_errors_total{kind="rate_limited"}` a client got.
- `hub_errors_total{kind="no_room"}` is a guest opening a link whose room is gone (an old
  invite, a host that closed Play Together or is asleep). The page tells them to ask for a new link.
- `hub_http_strays_total{class=...}`: plain HTTP requests that were not a WebSocket to `/ws`,
  by class of path only. These are the 404s in Fly's HTTP status chart: the hub's name is in
  public certificate logs, so scanners and crawlers ask for `/`, `/.env`, `/phpinfo` and so on
  all day (Fly's proxy logs show some as `client problem: invalid authority`, which it answers
  itself). The apps and the pages only ever open `/ws`. `/robots.txt` tells crawlers to stay
  away; nothing else is worth doing about them.

## Settings

Limits and the rest are `HUB_*` variables (see `src/config.rs` and `deploy/ps5upload-hub.env`).
Change them in the `[env]` section of `fly.toml` and run `fly deploy --ha=false`.

`fly.toml` trusts the `Fly-Client-IP` header from every peer. That is safe because the only way
to the hub from the internet is Fly's proxy, which sets that header; only other apps in your own
Fly organization could reach the machine directly.

## Scaling or moving later

- Bigger machine (the hub is tiny; only if `/metrics` shows thousands of connections):
  `fly scale vm shared-cpu-1x --memory 512`
- Another region (move, not add: still one machine). For example to Frankfurt:
  ```sh
  fly scale count 1 --region fra --yes     # start one in the new region
  fly scale count 0 --region iad --yes     # then remove the old one
  fly status                               # exactly one machine, in fra
  ```
  then set `primary_region = "fra"` in `fly.toml` so later deploys keep it there. Rooms are in
  memory, so a move ends the open rooms; hosts reconnect and reclaim theirs.
- Never `fly scale count 2` or more: see the top of this page.
- Restart: `fly machine restart <id>` (ids from `fly status`).
- Stop paying: `fly apps destroy ps5upload-hub` (and remove the GoDaddy record).

## Play Rooms (13.0.0)

Rooms need no new machine, volume or IP: the same app, redeployed (ask the owner first; it is a
redeploy of the existing app). Steps:

1. Locally: `cd engine && cargo nextest run -p ps5upload-hub`, then the load test
   (`cargo test -p ps5upload-hub --release --test load -- --ignored --nocapture`); it must pass
   (memory, list latency, no host dropped).
2. Optional, to switch things at run time from your computer: an admin token (a Fly secret; the
   admin routes exist only with it, and only on the private metrics port):
   ```sh
   fly secrets set HUB_ADMIN_TOKEN="$(openssl rand -hex 24)" --stage
   ```
3. Optional: newest-version nudges in the list: `HUB_APP_LATEST = "13.0.0"` under `[env]` in
   `fly.toml` (change it with each release, or set it at run time, below).
4. `fly deploy` from this folder, then check that `curl -s https://hub.phantomptr.com/rooms`
   answers `{"you":{},"proto":{"min":2,"latest":2},"app":{...},"rooms":[],"next":null}`.

Run-time switches and the blocklist, no redeploy. They last until the machine restarts; to keep
them, also put them in `fly.toml` `[env]` (`HUB_BLOCKLIST = "tag 4F2A; ip 203.0.113.0/24"`,
`HUB_ROOMS`, `HUB_PREVIEW`, `HUB_KNOCKS`, `HUB_MIN_PROTO`).

```sh
fly proxy 9091 &                     # the private metrics port
T=<the admin token>
curl -s -H "authorization: Bearer $T" localhost:9091/admin/control          # what is in force
curl -s -H "authorization: Bearer $T" --data-binary 'rooms off' localhost:9091/admin/control
printf 'tag 4F2A\nip 203.0.113.0/24\nword badword\n' |
  curl -s -H "authorization: Bearer $T" --data-binary @- localhost:9091/admin/control
# a POST replaces the run-time set; an empty body clears it
```

Lines: `rooms on|off` (cards, list, friends lookup), `preview on|off`, `knocks on|off` (lobby
rooms), `min_proto N`, `latest_proto N`, `app_latest 13.0.0`, `cutback auto|on|off`,
`tag XXXX`, `ip <address or CIDR>`, `word <word>` (room names). On a VPS the same lines can go in
a file named by `HUB_CONTROL_FILE`, re-read within 2 s of a change.

Countries (optional): `HUB_GEO_FILE` names a CSV of `start,end,CC` ranges (the CC BY 4.0
"IP to Country Lite" monthly file has this form). It would have to be in the image (a `COPY` in
the Dockerfile) and credited in About. Without it, cards show the region each host's app sends
(from its time zone) and estimates use that.

Watch in `/metrics`: `hub_ads_listed`, `hub_rooms_list_total{result}`, `hub_previews_total`,
`hub_knocks_total`, `hub_ads_refused_total{why}`, `hub_out_bytes_total{class}`,
`hub_out_projected_bytes_month`, `hub_cutback`, `hub_resident_bytes`, `hub_throttled_total`,
`hub_clients_blocked`, `hub_temp_blocks_total`. Cut-back mode starts by itself past 200 MiB
resident or a projected 100 GB a month out, and logs one line (`hub: cutback on=1 why=...`).

### Floods: what Fly gives and what it does not

- Fly's edge takes network-level floods (SYN and UDP floods, volumetric attacks) for every app
  and terminates TLS, so what reaches the hub is real HTTP from real addresses. Fly does not
  rate-limit or filter requests per client for us (no WAF), and it bills the bytes the hub sends
  back, refusals included.
- The hub's own layers: connection caps per client, per IPv6 /48 and in all; a 10 s hello and a
  60 s silence timeout; frame and message size caps; a message budget per socket; token buckets
  per client per kind of request (people never reach them; robots get 429 with `Retry-After`);
  a flood that goes on 30 s or more gets a temporary block (2 minutes, doubling to 30), answered
  with an empty 429 and the connection closed; the list from a shared 5 s cache; proof of work
  only on card creation; cut-back mode; the switches and the blocklist above.
- Measured locally (load test): 5 robots at about 94,000 list requests a second for 40 s used
  84 % of the hub's one thread; people's list requests stayed at a 3.4 ms p99, no host socket was
  dropped, memory stayed at 30 MiB. What a flood costs is egress: even an empty 429 is about
  150 bytes, so a long, large flood is a bill, not an outage. If
  `hub_out_bytes_total{class="refused"}` climbs fast: block the sources (`ip ...` above); if it is
  spread over many addresses, turn rooms off for a while (`rooms off`). The lasting answer would
  be a filtering proxy in front (not set up; it would change `HUB_CLIENT_IP_HEADER`).
