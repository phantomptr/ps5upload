# Reverse proxy for the self-hosted web UI

A proxy with TLS is also how **Play works in a browser on another device**. Browsers only give a
page the video decoder on `https://` (or on `localhost`), so `http://<nas>:19113` can open the whole
app except Play. Over plain http, play with the desktop or phone app instead.

Use the published web UI image unchanged. `PS5UPLOAD_BASE_URL` is a **URL path prefix**,
such as `/ps5/`, not a hostname or a full URL. Leave it unset for a dedicated hostname. It is
read when the engine starts, so it needs no rebuild. A value that is not a path is ignored with a
warning in the log, and the engine serves at the root.

## Dedicated hostname (recommended)

Point your LAN DNS name at the reverse proxy. Start with `engine/compose.yaml`.
The engine and web UI share port 19113; proxy the whole site, including `/api/`.

Remote Play, the phone controller and Play Together all use **WebSockets**, and browsers only
offer Remote Play's video decoder on `https://` (or on the computer itself). So a proxy needs both
TLS and the WebSocket headers. An example nginx site (replace the hostname, the certificate and the
engine address):

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ""      close;
}

server {
    listen 443 ssl;
    server_name ps5upload.home;
    ssl_certificate     /etc/nginx/ps5upload.home.crt;
    ssl_certificate_key /etc/nginx/ps5upload.home.key;

    location / {
        proxy_pass http://192.168.1.10:19113;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_set_header Host $http_host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $http_host;
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
        client_max_body_size 0;
    }
}
```

Three of those lines matter more than they look:

- `Upgrade` and `Connection`: Play, the phone controller, watching and Play Together all run on
  WebSockets, and nginx does not forward those two headers unless told to. Without them the file
  screens work, and every socket fails with only a dropped connection on the page.
- `Host $http_host`, not `$host`: `$http_host` keeps the port. The engine compares the browser's
  `Origin` with `Host` and answers `403` with the two values when they differ, so `$host` breaks on
  any port other than 443 and 80. A proxy that has to rewrite `Host` can send what the browser asked
  for in `X-Forwarded-Host` instead.
- Use a certificate the browser trusts: your own CA installed on each device, or a name with a real
  certificate. One you only clicked past a warning for can still stop the WebSockets (not tested in
  every browser).

Other proxies that handle all this by themselves: Caddy (`reverse_proxy 192.168.1.10:19113`) and
`tailscale serve`.

Buffering off keeps the live answers live: the engine marks its event streams
`X-Accel-Buffering: no`, but uploads from the browser need `proxy_request_buffering off` too.
Browser-selected files can be large, so disable nginx's default 1 MiB body limit or choose your
own limit. If you add proxy authentication, apply it to the browser site and API together.
Do not expose the unauthenticated engine port outside your trusted LAN.

### A name on the public internet

The engine answers to loopback names, IP addresses, single-label names (`nas`) and the private
suffixes `.local`, `.lan`, `.home`, `.home.arpa`, `.internal`, `.intranet`, `.localdomain` and
`.ts.net`. Any other name gets a JSON 403 that names this setting:

```yaml
PS5UPLOAD_ALLOW_HOST: "ps5.mydomain.com"   # also *.mydomain.com, or * for any name
```

This stops a web page from reaching the engine by pointing its own domain at your address (DNS
rebinding).

### Who the engine thinks is asking

`PS5UPLOAD_ALLOW_IP` and the per-address limits check the direct TCP peer, so by default they see
the proxy, and they ignore `X-Forwarded-For`, which a client could write itself. Then:

- A proxy on another machine: allow its address. All guests share that one address, so Play
  Together's limits (10 connects and 5 wrong codes a minute) count them together.
- A proxy on the engine's own machine connects from loopback, which the engine always allows, so
  **everyone who reaches the proxy is allowed**, and Play Together's and Go live's host controls
  refuse a public name with `host_not_local`.

To fix both, name your proxy:

```yaml
PS5UPLOAD_TRUSTED_PROXY: "127.0.0.1"   # or 172.18.0.0/16; IPs and ranges, comma-separated
```

The engine then believes `X-Forwarded-For` from that address only, and the client's own address
is what the allowlist and the limits see. On a same-machine proxy, also list your name in
`PS5UPLOAD_ALLOW_HOST` for the host controls. An authenticating proxy also needs its own access
controls, and the backend port must not be reachable by clients that should have to sign in.

## Docker networking and PS5 package downloads

The browser proxy does not automatically proxy the package URL fetched by the
PS5 during a Stream install. The PS5 must reach the engine's package-serving port
without interactive proxy login. Keep that path LAN-only.

Host networking is convenient on Linux, but is not required. `--network host` makes connections
true loopback only from the same machine; other machines still need `PS5UPLOAD_ALLOW_IP`. For
bridge networking, replace `network_mode: host` in the example compose file with:

```yaml
ports:
  - "19113:19113"
environment:
  PS5UPLOAD_PKG_HOST_IP: "192.168.1.10" # Docker host LAN IP reachable from the PS5
  PS5UPLOAD_ALLOW_IP: "192.168.1.0/24,172.18.0.0/16" # use your actual proxy subnet
```

Merge these environment values with the existing configuration (PS5_ADDR,
PS5UPLOAD_BROWSE_ROOTS, etc.). `PS5UPLOAD_ALLOW_IP` is required whenever the web UI is used from
another device: the engine answers only loopback callers and the addresses in the allowlist, so a
proxy or a browser that is not listed gets a `403`. `PS5_ADDR` is your console's IP: the first
browser finds it already listed, and it is otherwise optional (add the console in the app).
`PS5UPLOAD_PKG_HOST_IP` also decides which address phone and friend links name when the container
cannot see the LAN itself. Publish the same engine port: the package URL uses that port. The proxy
must reach the published address or the container service name on their shared Docker network. The
PS5's own package fetches skip the allowlist, so it does not need to be listed. Persist `/data` and
mount your game directories as in the compose example.

## Hosting below a path

Set the prefix on the published image; nothing is rebuilt:

```yaml
environment:
  PS5UPLOAD_BASE_URL: "/ps5/"
```

Forward the prefix unchanged, with **no trailing slash** on nginx's `proxy_pass` URL:

```nginx
location = /ps5 { return 308 /ps5/; }
location /ps5/ {
    proxy_pass http://192.168.1.10:19113;
    # Include the same proxy headers (Upgrade and Connection too), buffering, size and timeout
    # settings as above.
}
```

Open `/ps5/`. The API is `/ps5/api/...`; the container healthcheck probes that same prefix.
`/pkg-host/` stays at the root, because the console fetches packages from it directly.

If you build the image yourself (`docker build -f engine/Dockerfile.webui .`), Send helper needs
the helper in the build: run `make payload` first, which needs the PS5 payload SDK, or point
`PS5UPLOAD_PAYLOAD_DIR` at a folder holding `ps5upload.elf`. The published images carry it.
`--build-arg PS5UPLOAD_BASE_URL=/ps5/` still bakes a prefix in, but it is no longer needed.
