# Firewall (VPS setup)

Open only these inbound ports, in the Hetzner Cloud Firewall and on the server (`ufw`):

| Port     | Why                                                                  |
|----------|----------------------------------------------------------------------|
| 22/tcp   | SSH, for you. Limit it to your own address if you can.               |
| 80/tcp   | Let's Encrypt HTTP challenge and Caddy's redirect to HTTPS.          |
| 443/tcp  | The hub: `wss://hub.phantomptr.com/ws` and `/health`.                |
| 3478/udp | STUN, **only if** you turned it on (`HUB_STUN_LISTEN`). Off by default. |

The hub's own port (8787) listens on 127.0.0.1 and must stay closed to the outside.

```sh
ufw default deny incoming
ufw default allow outgoing
ufw allow 22/tcp
ufw allow 80/tcp
ufw allow 443/tcp
# ufw allow 3478/udp   # only with STUN on
ufw enable
```
