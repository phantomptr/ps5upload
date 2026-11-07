#!/bin/sh
# tailnet test bed: a local Headscale plus one stock Linux tailscaled peer, in Docker.
# No Tailscale account is involved. See README.md in this directory.
#
#   tb.sh up              start Headscale and the Linux peer (joined with a pre-auth key)
#   tb.sh down            stop and remove both containers (state in .state/ is kept)
#   tb.sh authkey         print a fresh reusable pre-auth key for user "tnbed"
#   tb.sh register ID     approve an interactive login (the id at the end of the AuthURL)
#   tb.sh nodes           headscale nodes list
#   tb.sh status          tailscale status, as seen by the Linux peer
#   tb.sh hs ARGS...      run any headscale CLI command
#   tb.sh peer ARGS...    run any command in the peer container
#   tb.sh reset           down, then move .state/ aside (to .state.old-<time>) for a clean bed
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
STATE="$HERE/.state"
NET=tnbed
HS=tnbed-headscale
PEER=tnbed-peer
# Pinned images (multi-arch). Bump deliberately; Headscale's minimum capver rises with releases.
HS_IMAGE=docker.io/headscale/headscale:0.29.1
TS_IMAGE=docker.io/tailscale/tailscale:v1.104.4
HS_PORT=${TNBED_PORT:-18080}
USER_NAME=tnbed

# Docker's credential helper can hang on this Mac: give docker an empty config.
mkdir -p "$STATE/docker"
[ -f "$STATE/docker/config.json" ] || echo '{}' >"$STATE/docker/config.json"
export DOCKER_CONFIG="$STATE/docker"

hs() { docker exec "$HS" headscale "$@"; }

running() { [ "$(docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null || true)" = "true" ]; }

user_id() {
	hs users list -o json | tr -d ' \n\t' |
		sed -n "s/.*{\"id\":\([0-9]*\),\"name\":\"$USER_NAME\".*/\1/p"
}

authkey() {
	uid=$(user_id)
	[ -n "$uid" ] || { hs users create "$USER_NAME" >/dev/null; uid=$(user_id); }
	hs preauthkeys create --user "$uid" --reusable --expiration 24h -o json |
		tr -d ' \n\t' | sed -n 's/.*"key":"\([^"]*\)".*/\1/p'
}

wait_hs() {
	i=0
	until curl -fsS "http://127.0.0.1:$HS_PORT/health" >/dev/null 2>&1; do
		i=$((i + 1)); [ $i -lt 60 ] || { echo "headscale did not come up" >&2; docker logs "$HS" | tail -20; exit 1; }
		sleep 1
	done
}

up() {
	mkdir -p "$STATE/headscale/lib" "$STATE/peer"
	docker network inspect "$NET" >/dev/null 2>&1 || docker network create "$NET" >/dev/null
	if ! running "$HS"; then
		docker rm -f "$HS" >/dev/null 2>&1 || true
		sed "s#http://127.0.0.1:18080#http://127.0.0.1:$HS_PORT#" "$HERE/headscale.yaml" >"$STATE/headscale/config.yaml"
		docker run -d --name "$HS" --network "$NET" \
			-p "127.0.0.1:$HS_PORT:8080" \
			-v "$STATE/headscale/config.yaml:/etc/headscale/config.yaml:ro" \
			-v "$STATE/headscale/lib:/var/lib/headscale" \
			--tmpfs /var/run/headscale \
			"$HS_IMAGE" serve >/dev/null
	fi
	wait_hs
	echo "headscale: http://127.0.0.1:$HS_PORT ($HS_IMAGE)"
	if ! running "$PEER"; then
		docker rm -f "$PEER" >/dev/null 2>&1 || true
		key=$(authkey)
		docker run -d --name "$PEER" --hostname linux-peer --network "$NET" \
			--cap-add NET_ADMIN --cap-add NET_RAW --device /dev/net/tun \
			-v "$STATE/peer:/var/lib/tailscale" \
			-e TS_STATE_DIR=/var/lib/tailscale -e TS_USERSPACE=false \
			-e TS_AUTHKEY="$key" -e TS_HOSTNAME=linux-peer \
			-e TS_EXTRA_ARGS="--login-server=http://$HS:8080" \
			"$TS_IMAGE" >/dev/null
	fi
	i=0
	until docker exec "$PEER" tailscale ip -4 >/dev/null 2>&1; do
		i=$((i + 1)); [ $i -lt 60 ] || { echo "peer did not join" >&2; docker logs "$PEER" | tail -20; exit 1; }
		sleep 1
	done
	echo "peer: linux-peer $(docker exec "$PEER" tailscale ip -4) ($TS_IMAGE)"
}

down() {
	for c in "$PEER" "$HS"; do
		if docker inspect "$c" >/dev/null 2>&1; then docker stop "$c" >/dev/null; docker rm "$c" >/dev/null; echo "stopped $c"; fi
	done
	docker network rm "$NET" >/dev/null 2>&1 || true
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case "$cmd" in
up) up ;;
down) down ;;
authkey) authkey ;;
register) [ $# -eq 1 ] || { echo "usage: tb.sh register AUTH_ID" >&2; exit 2; }
	hs auth register --user "$USER_NAME" --auth-id "$1" ;;
nodes) hs nodes list ;;
status) docker exec "$PEER" tailscale status ;;
hs) hs "$@" ;;
peer) docker exec "$PEER" "$@" ;;
reset) down; [ -d "$STATE" ] && mv "$STATE" "$STATE.old-$(date +%Y%m%d%H%M%S)"; echo "state moved aside" ;;
*) sed -n '2,14p' "$0"; exit 2 ;;
esac
