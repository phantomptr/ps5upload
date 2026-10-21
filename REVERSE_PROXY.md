# Reverse proxy for the self-hosted web UI

For a dedicated DNS name such as `ps5upload.home`, use the published web UI image
unchanged. `PS5UPLOAD_BASE_URL` is a **URL path prefix**, such as `/ps5/`, not a
hostname or full URL. Leave it unset for a dedicated hostname. It is baked into
the browser assets at image build time; changing only the runtime environment
does not rebuild those assets.

## Dedicated hostname (recommended)

Point your LAN DNS name at the reverse proxy. Start with `engine/compose.yaml`.
The engine and web UI share port 19113; proxy the whole site, including `/api/`.
Example nginx server block (replace the hostname and engine address):

```nginx
server {
    listen 80;
    server_name ps5upload.home;

    location / {
        proxy_pass http://192.168.1.10:19113;
        proxy_http_version 1.1;
        proxy_set_header Host $http_host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
        client_max_body_size 0;
    }
}
```

Streaming responses (job events) need buffering off. Browser-selected files can
be large, so disable nginx's default 1 MiB body limit or choose your own limit.
If you add proxy authentication, apply it to the browser site and API together.
Do not expose the unauthenticated engine port outside your trusted LAN.

`PS5UPLOAD_ALLOW_IP` checks the direct TCP peer, not `X-Forwarded-For`. Allow the
proxy's actual address; do not make it trust client-supplied forwarding headers.
An authenticating proxy also needs its own access controls, and the backend port
must not be reachable by clients that should have to sign in.

## Docker networking and PS5 package downloads

The browser proxy does not automatically proxy the package URL fetched by the
PS5 during a Stream install. The PS5 must reach the engine's package-serving port
without interactive proxy login. Keep that path LAN-only.

Host networking is convenient on Linux, but is not required. For bridge networking,
replace `network_mode: host` in the example compose file with:

```yaml
ports:
  - "19113:19113"
environment:
  PS5UPLOAD_PKG_HOST_IP: "192.168.1.10" # Docker host LAN IP reachable from the PS5
  PS5UPLOAD_ALLOW_IP: "192.168.1.0/24,172.18.0.0/16" # use your actual proxy subnet
```

Merge these environment values with the existing configuration (PS5_ADDR,
PS5UPLOAD_BROWSE_ROOTS, etc.). Publish the same engine port: the package URL uses
that port. The proxy must reach the published address or the container service
name on their shared Docker network. Include the PS5's IP in the engine allowlist
for direct package fetches. Persist `/data` and mount your game directories as
in the compose example.

## Hosting below a path

Build the web UI image with a leading/trailing slash path prefix:

```sh
docker build -f engine/Dockerfile.webui \
  --build-arg PS5UPLOAD_BASE_URL=/ps5/ \
  -t ps5upload-webui-prefixed .
```

Keep that same prefix at runtime; the Dockerfile sets it automatically. Forward
the prefix unchanged, with **no trailing slash** on nginx's `proxy_pass` URL:

```nginx
location = /ps5 { return 308 /ps5/; }
location /ps5/ {
    proxy_pass http://192.168.1.10:19113;
    # Include the same proxy headers, buffering, size and timeout settings above.
}
```

Open `/ps5/`. The API is `/ps5/api/...`; the container healthcheck probes that
same prefix. A healthcheck failure with a full URL in PS5UPLOAD_BASE_URL is a
configuration error: use `/` for a hostname, or rebuild with `/ps5/` for a path.

