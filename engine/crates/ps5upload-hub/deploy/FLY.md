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
