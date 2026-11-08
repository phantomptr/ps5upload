#!/bin/sh
# One-time VPS setup for the hub (Debian 12 / Ubuntu 24.04, run as root on the server).
# Expects, in the current directory, the files scp'd by README.md step 4:
#   ps5upload-hub  ps5upload-hub.service  ps5upload-hub.env  Caddyfile
# Safe to run again: it replaces the binary and restarts the service, and keeps an edited
# /etc/ps5upload-hub.env.
set -eu

for f in ps5upload-hub ps5upload-hub.service ps5upload-hub.env Caddyfile; do
  [ -f "$f" ] || { echo "missing $f in $(pwd)"; exit 1; }
done

# Caddy from its official Debian repository.
if ! command -v caddy >/dev/null 2>&1; then
  apt-get update
  apt-get install -y debian-keyring debian-archive-keyring apt-transport-https curl gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
    | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
    > /etc/apt/sources.list.d/caddy-stable.list
  apt-get update
  apt-get install -y caddy
fi

install -m 0755 ps5upload-hub /usr/local/bin/ps5upload-hub
[ -f /etc/ps5upload-hub.env ] || install -m 0644 ps5upload-hub.env /etc/ps5upload-hub.env
install -m 0644 ps5upload-hub.service /etc/systemd/system/ps5upload-hub.service
install -m 0644 Caddyfile /etc/caddy/Caddyfile

systemctl daemon-reload
systemctl enable ps5upload-hub
systemctl restart ps5upload-hub
systemctl reload caddy || systemctl restart caddy

# Firewall (FIREWALL.md). Add `ufw allow 3478/udp` yourself if you turn STUN on.
if command -v ufw >/dev/null 2>&1; then
  ufw allow 22/tcp
  ufw allow 80/tcp
  ufw allow 443/tcp
  ufw --force enable
fi

sleep 1
curl -fsS http://127.0.0.1:8787/health && echo "hub is up"
