# Deploying the Play Together hub

The hub only introduces a host's engine to a guest's browser (WebRTC signalling). Media goes
peer to peer and never through it, and it keeps nothing on disk, so it needs very little: one
small machine, one name (`hub.phantomptr.com`), HTTPS.

**Recommended: Fly.io.** See [FLY.md](FLY.md). No server to look after, HTTPS and the
certificate are handled for you, and the hub needs no UDP or dedicated IPv4 there because STUN is
off by default (clients use public STUN servers).

The rest of this page is the **alternative**: your own VPS with Caddy in front.

| File | What |
|------|------|
| `../Dockerfile`, `../fly.toml`, `FLY.md` | Fly.io (recommended) |
| `workspace.toml`, `Cargo.lock`, `refresh-lock.sh` | the one-crate build the Dockerfile uses |
| `ps5upload-hub.service` | systemd unit, runs as a throwaway non-root user |
| `ps5upload-hub.env` | settings for the unit (`HUB_*`) |
| `Caddyfile` | HTTPS for `hub.phantomptr.com`, proxies `/ws`, `/health`, `/metrics` (localhost only) |
| `FIREWALL.md` | the ports to open |
| `setup.sh` | one-time server setup |

## VPS steps (Hetzner + GoDaddy)

1. **Rent the server.** In the Hetzner Cloud console: New project, Add server, location
   Falkenstein, Nuremberg or Helsinki (EU), image Ubuntu 24.04, type **CX22** (shared x86, 2 vCPU,
   4 GB, about €4-5 a month; take the smallest shared x86 type if CX22 is no longer offered). Add
   your SSH key. Keep the IPv4 and IPv6 (both are included). Note the server's IPv4 and IPv6.
2. **Point the name at it.** In GoDaddy: My Products, phantomptr.com, DNS, Add record:
   - type `A`, name `hub`, value the server's IPv4, TTL 600;
   - type `AAAA`, name `hub`, value the server's IPv6, TTL 600.
   Check from your computer: `dig +short hub.phantomptr.com A` and `... AAAA` show the
   addresses (can take a few minutes).
3. **Build the Linux binary** on your computer, from the repository root (needs Docker):
   ```sh
   docker build --platform linux/amd64 -t ps5upload-hub engine/crates/ps5upload-hub
   docker create --name hub-extract ps5upload-hub
   docker cp hub-extract:/usr/local/bin/ps5upload-hub ./ps5upload-hub
   docker rm hub-extract
   ```
   (`ps5upload-hub` is a static x86-64 Linux binary, about 1-2 MB.)
4. **Copy it and the deploy files up**, then run the setup as root:
   ```sh
   cd engine/crates/ps5upload-hub/deploy
   scp ../../../../ps5upload-hub ps5upload-hub.service ps5upload-hub.env Caddyfile setup.sh root@<server-ip>:/root/
   ssh root@<server-ip> 'cd /root && sh setup.sh'
   ```
   It installs Caddy, the binary, the unit and the settings, opens 22, 80 and 443 with `ufw`,
   starts everything and prints `ok` / `hub is up`.
5. **Hetzner firewall** (optional, on top of `ufw`): allow inbound 22/tcp, 80/tcp, 443/tcp only
   ([FIREWALL.md](FIREWALL.md)).
6. **Check:** `curl https://hub.phantomptr.com/health` prints `ok` (the first request can take a
   few seconds while Caddy gets the certificate). `/metrics` answers 403 from outside; on the
   server, `curl -s http://127.0.0.1:8787/metrics`.

Updating later: steps 3 and 4 again (`setup.sh` keeps your edited `/etc/ps5upload-hub.env`).
Logs: `journalctl -u ps5upload-hub -f` (counts and error kinds only, never addresses).

Turning STUN on: uncomment `HUB_STUN_LISTEN=[::]:3478` in `/etc/ps5upload-hub.env`,
`ufw allow 3478/udp` (and in the Hetzner firewall), `systemctl restart ps5upload-hub`.
