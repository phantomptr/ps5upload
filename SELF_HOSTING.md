# Hosting Play Together yourself

An internet invite is a link to a small **guest page** (`/join/`, `/pad/` for a phone controller,
`/share/` for Share links). The page and the host's engine are introduced by a **hub**, a
signalling server, and then talk to each other directly (peer to peer). The hub never sees the
picture, the sound or the controls, and the link pins the engine's certificate, so a hub cannot
listen in. There is no relay server.

By default the pages are `https://ps5upload.phantomptr.com/` and the hub is
`wss://hub.phantomptr.com/ws`. You don't need to change anything. Host them yourself only if you
want invites to go through your own name.

**The engine API has no password. Never put it on the internet.** Only the hub (below) may be
published.

## Two ways

1. **The hub built into the engine** (simplest, when the engine already runs on a server with a
   reverse proxy and HTTPS): one public name, such as `play.example.com`, serves the hub and the
   pages. See [The built-in hub](#the-built-in-hub).
2. **The standalone hub** ([engine/crates/ps5upload-hub](engine/crates/ps5upload-hub/deploy/README.md))
   on any small server, with the guest pages (`site/join/`, `site/pad/`, `site/share/` of the same
   release) on any HTTPS web host. Then point the engine at both, in Settings › Play Together ›
   Advanced or with `PS5UPLOAD_HUB_URL` and `PS5UPLOAD_PAGE_BASE`. The hub only accepts browsers
   from the origins in `HUB_ALLOWED_ORIGINS`; setting it replaces the list, so name all of them:
   your pages' origin and the app's (`tauri://localhost,http://tauri.localhost,https://tauri.localhost`).

The default hub only accepts its own pages and the app. With your own pages, use your own hub too:
Settings warns when the pages are yours and the hub is still the default one.

## The built-in hub

Off unless you turn it on. It listens apart from the engine API and serves only `/ws` (the hub),
`/join/`, `/pad/`, `/share/` (the pages of the engine's own version) and `/health`. Every other
path, the engine API included, answers 404.

| Variable | What it does |
|---|---|
| `PS5UPLOAD_HUB_LISTEN` | Turns it on: where it listens, such as `127.0.0.1:19115`, a port, or `on` for `0.0.0.0:19115` |
| `PS5UPLOAD_HUB_PUBLIC_URL` | Its public address, such as `https://play.example.com` (no path). Invites then use `wss://play.example.com/ws` and `https://play.example.com/join/` |
| `PS5UPLOAD_HUB_STUN` | Optional: also answer STUN on this UDP port (`3478`, or `on`), first in the STUN servers. A reverse proxy does not carry UDP: forward the port to the server itself |
| `PS5UPLOAD_HUB_URL`, `PS5UPLOAD_PAGE_BASE` | Use another hub or other pages than the public address gives |
| `PS5UPLOAD_TRUSTED_PROXY` | A proxy that is not on the same machine (loopback is believed already), so the hub's limits count each friend, not the proxy |

The hub's limits are its `HUB_*` settings, as for the standalone hub. The engine reaches its own
hub on the machine itself; links carry the public address. A setting saved in Settings › Play
Together wins over these variables, and Settings shows which one is in force.

### Example: engine and Caddy

Caddy gets the certificate and forwards WebSockets by itself. Both use host networking, so Caddy
reaches the hub on `127.0.0.1` and the hub listens there only.

`compose.yaml` (start from [engine/compose.yaml](engine/compose.yaml) and add the two hub lines):

```yaml
services:
  ps5upload:
    image: ghcr.io/phantomptr/ps5upload-engine-webui:latest
    network_mode: host
    restart: unless-stopped
    environment:
      PS5_ADDR: "192.168.1.50"
      PS5UPLOAD_ALLOW_IP: "192.168.1.0/24"    # the engine API: your LAN only
      PS5UPLOAD_HUB_LISTEN: "127.0.0.1:19115" # the built-in hub, for Caddy only
      PS5UPLOAD_HUB_PUBLIC_URL: "https://play.example.com"
    volumes:
      - ps5upload-data:/data

  caddy:
    image: caddy:2
    network_mode: host
    restart: unless-stopped
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy-data:/data

volumes:
  ps5upload-data:
  caddy-data:
```

`Caddyfile`, next to it. It names the hub's port only, never the engine's `19113`:

```
play.example.com {
    reverse_proxy 127.0.0.1:19115
}
```

Point `play.example.com` at your public address and forward TCP 80 and 443 on your router to this
server. Do not forward 19113. Friends still connect to the engine directly for the game itself, as
with the default hub (the router port in Settings › Play Together).

With nginx instead, forward the WebSocket headers:

```nginx
location / {
    proxy_pass http://127.0.0.1:19115;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $http_host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_read_timeout 3600s;
}
```

### Check it

- `curl https://play.example.com/health` prints `ok`.
- `curl -i https://play.example.com/api/join` answers `404`: the engine API is not there.
- In the app, Settings › Play Together › Advanced shows the hub and the guest page address as set
  on the engine's server. A new invite starts with `https://play.example.com/join/`.
