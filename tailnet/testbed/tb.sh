#!/bin/sh
# tailnet test bed: a local Headscale, a local derper (DERP + STUN over TLS with a
# throwaway CA), one stock Linux tailscaled peer, and a Linux box for our tnctl.
# All in Docker; no Tailscale account, no public DERP. See README.md here.
#
#   tb.sh up              start everything (the peer joins with a pre-auth key)
#   tb.sh down            stop and remove the containers (state in .state/ is kept)
#   tb.sh authkey         print a fresh reusable pre-auth key for user "tnbed"
#   tb.sh register ID     approve an interactive login (the id at the end of the AuthURL)
#   tb.sh nodes           headscale nodes list
#   tb.sh status          tailscale status, as seen by the Linux peer
#   tb.sh hs ARGS...      run any headscale CLI command
#   tb.sh peer ARGS...    run any command in the peer container
#   tb.sh tn ARGS...      run any command in the tnctl container (tailnet/ is at /src/tailnet)
#   tb.sh tn2 ARGS...     the same in the second box (the engine's side in m4-check.sh)
#   tb.sh build-linux     build tnctl (ASan+UBSan) and tnecho for Linux in the tn container
#   tb.sh test-linux      the unit tests (ASan+UBSan) on Linux/glibc in the tn container
#   tb.sh block on|off    drop/allow UDP between the peer and the tn container (forces DERP)
#   tb.sh reset           down, then move .state/ aside (to .state.old-<time>) for a clean bed
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
STATE="$HERE/.state"
NET=tnbed
SUBNET=172.31.77.0/24
DERP_IP=172.31.77.10
HS=tnbed-headscale
PEER=tnbed-peer
DERPER=tnbed-derper
TN=tnbed-tn
TN2=tnbed-tn2
# Pinned images (multi-arch). Bump deliberately; Headscale's minimum capver rises with releases.
HS_IMAGE=docker.io/headscale/headscale:0.29.1
TS_IMAGE=docker.io/tailscale/tailscale:v1.104.4
TN_IMAGE=docker.io/library/gcc:14
HS_PORT=${TNBED_PORT:-18080}
USER_NAME=tnbed
# derper is built from a local clone of github.com/tailscale/tailscale (BSD-3-Clause).
TAILSCALE_SRC=${TAILSCALE_SRC:-$HOME/workspace/github.com/tailscale/tailscale}

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

# A throwaway CA and a derper certificate (DNS:derper, IP:$DERP_IP), valid 30 days.
make_ca() {
	CA="$STATE/ca"
	[ -f "$CA/derper.crt" ] && openssl x509 -checkend 86400 -noout -in "$CA/derper.crt" >/dev/null 2>&1 && return
	mkdir -p "$CA"
	openssl ecparam -name prime256v1 -genkey -noout -out "$CA/ca.key" 2>/dev/null
	openssl req -x509 -new -key "$CA/ca.key" -sha256 -days 30 -subj "/CN=tnbed derp CA" \
		-addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
		-out "$CA/ca.pem" 2>/dev/null
	openssl ecparam -name prime256v1 -genkey -noout -out "$CA/derper.key" 2>/dev/null
	openssl req -new -key "$CA/derper.key" -subj "/CN=derper" -out "$CA/derper.csr" 2>/dev/null
	printf "subjectAltName=DNS:derper,IP:%s\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n" "$DERP_IP" >"$CA/ext.cnf"
	openssl x509 -req -in "$CA/derper.csr" -CA "$CA/ca.pem" -CAkey "$CA/ca.key" -CAcreateserial -days 30 -sha256 \
		-extfile "$CA/ext.cnf" -out "$CA/derper.crt" 2>/dev/null
}

build_derper() {
	[ -x "$STATE/bin/derper" ] && return
	mkdir -p "$STATE/bin"
	arch=$(docker info --format '{{.Architecture}}')
	case "$arch" in aarch64 | arm64) goarch=arm64 ;; x86_64 | amd64) goarch=amd64 ;; *) echo "arch $arch?" >&2; exit 1 ;; esac
	[ -d "$TAILSCALE_SRC/cmd/derper" ] || { echo "derper source missing: set TAILSCALE_SRC to a tailscale clone" >&2; exit 1; }
	echo "building derper ($goarch) from $TAILSCALE_SRC"
	GOOS=linux GOARCH=$goarch CGO_ENABLED=0 go -C "$TAILSCALE_SRC" build -o "$STATE/bin/derper" ./cmd/derper
}

up() {
	mkdir -p "$STATE/headscale/lib" "$STATE/peer"
	make_ca
	build_derper
	if ! docker network inspect "$NET" >/dev/null 2>&1; then
		docker network create --subnet "$SUBNET" --ip-range 172.31.77.128/25 "$NET" >/dev/null
	fi
	if ! running "$DERPER"; then
		docker rm -f "$DERPER" >/dev/null 2>&1 || true
		mkdir -p "$STATE/derper-certs"
		cp "$STATE/ca/derper.crt" "$STATE/derper-certs/derper.crt"
		cp "$STATE/ca/derper.key" "$STATE/derper-certs/derper.key"
		docker run -d --name "$DERPER" --hostname derper --network "$NET" --ip "$DERP_IP" \
			-v "$STATE/bin/derper:/usr/local/bin/derper:ro" -v "$STATE/derper-certs:/certs:ro" \
			--entrypoint /usr/local/bin/derper "$TS_IMAGE" \
			-hostname derper -certmode manual -certdir /certs -a :443 -http-port -1 -stun-port 3478 >/dev/null
	fi
	hs_new=0
	if ! running "$HS"; then
		hs_new=1
		docker rm -f "$HS" >/dev/null 2>&1 || true
		sed "s#http://127.0.0.1:18080#http://127.0.0.1:$HS_PORT#" "$HERE/headscale.yaml" >"$STATE/headscale/config.yaml"
		cat >"$STATE/headscale/derp.yaml" <<EOF
regions:
  900:
    regionid: 900
    regioncode: tnb
    regionname: tnbed local
    nodes:
      - name: 900a
        regionid: 900
        hostname: derper
        ipv4: $DERP_IP
        stunport: 3478
        derpport: 443
EOF
		docker run -d --name "$HS" --network "$NET" \
			-p "127.0.0.1:$HS_PORT:8080" \
			-v "$STATE/headscale/config.yaml:/etc/headscale/config.yaml:ro" \
			-v "$STATE/headscale/derp.yaml:/etc/headscale/derp.yaml:ro" \
			-v "$STATE/headscale/lib:/var/lib/headscale" \
			--tmpfs /var/run/headscale \
			"$HS_IMAGE" serve >/dev/null
	fi
	wait_hs
	echo "headscale: http://127.0.0.1:$HS_PORT ($HS_IMAGE); derper: https://derper ($DERP_IP), STUN 3478"
	# A stock tailscaled whose control server was recreated can stay stuck on "forcing port
	# 443 dial due to recent noise dial" for an http:// URL, even across its own restart:
	# re-join it with fresh state (the old state is moved aside, not deleted).
	if [ $hs_new = 1 ] && docker inspect "$PEER" >/dev/null 2>&1; then
		docker rm -f "$PEER" >/dev/null
		[ -d "$STATE/peer" ] && mv "$STATE/peer" "$STATE/peer.old-$(date +%Y%m%d%H%M%S)"
		mkdir -p "$STATE/peer"
	fi
	sleep 3 # Headscale answers /health before its Noise endpoint; a peer that dials too
	        # early wedges ("forcing port 443"): give it a moment, and retry once fresh.
	for attempt in 1 2; do
		if ! running "$PEER"; then
			docker rm -f "$PEER" >/dev/null 2>&1 || true
			key=$(authkey)
			docker run -d --name "$PEER" --hostname linux-peer --network "$NET" \
				--cap-add NET_ADMIN --cap-add NET_RAW --device /dev/net/tun \
				-v "$STATE/peer:/var/lib/tailscale" -v "$STATE/ca/ca.pem:/ca.pem:ro" \
				-v "$STATE/bin:/tnbin:ro" \
				-e SSL_CERT_FILE=/ca.pem \
				-e TS_STATE_DIR=/var/lib/tailscale -e TS_USERSPACE=false \
				-e TS_AUTHKEY="$key" -e TS_HOSTNAME=linux-peer \
				-e TS_EXTRA_ARGS="--login-server=http://$HS:8080" \
				"$TS_IMAGE" >/dev/null
		fi
		i=0
		until docker exec "$PEER" tailscale ip -4 >/dev/null 2>&1 || [ $i -ge 45 ]; do i=$((i + 1)); sleep 1; done
		docker exec "$PEER" tailscale ip -4 >/dev/null 2>&1 && break
		echo "peer did not join (attempt $attempt); re-joining with fresh state" >&2
		docker rm -f "$PEER" >/dev/null
		[ -d "$STATE/peer" ] && mv "$STATE/peer" "$STATE/peer.old-$(date +%Y%m%d%H%M%S)"
		mkdir -p "$STATE/peer"
	done
	if docker exec "$PEER" tailscale ip -4 >/dev/null 2>&1; then
		echo "peer: linux-peer $(docker exec "$PEER" tailscale ip -4) ($TS_IMAGE)"
	else
		echo "peer: linux-peer did not join (m2/m3 checks need it; m4 does not)" >&2
	fi
	if ! running "$TN"; then
		docker rm -f "$TN" >/dev/null 2>&1 || true
		docker run -d --name "$TN" --hostname tn-box --network "$NET" \
			-v "$REPO:/src" -v "$STATE/ca/ca.pem:/ca.pem:ro" \
			--entrypoint sleep "$TN_IMAGE" infinity >/dev/null
	fi
	echo "tn box: $TN $(docker inspect -f '{{(index .NetworkSettings.Networks "'$NET'").IPAddress}}' "$TN") ($TN_IMAGE)"
	if ! running "$TN2"; then
		docker rm -f "$TN2" >/dev/null 2>&1 || true
		docker run -d --name "$TN2" --hostname tn-box2 --network "$NET" \
			-v "$REPO:/src" -v "$STATE/ca/ca.pem:/ca.pem:ro" \
			--entrypoint sleep "$TN_IMAGE" infinity >/dev/null
	fi
}

build_linux() {
	# Separate build dirs so host (macOS) objects are never mixed in.
	docker exec -w /src/tailnet "$TN" make -s SAN=1 BUILD=build/linux-asan build/linux-asan/tnctl >/dev/null
	docker exec -w /src/tailnet "$TN" sh -c 'mkdir -p testbed/.state/bin && cc -O2 -static -pthread -o testbed/.state/bin/tnecho testbed/tnecho.c'
	echo "built build/linux-asan/tnctl and .state/bin/tnecho"
}

block() {
	tnip=$(docker inspect -f '{{(index .NetworkSettings.Networks "'$NET'").IPAddress}}' "$TN")
	case "$1" in
	on)
		docker exec "$PEER" iptables -I INPUT -s "$tnip" -p udp -j DROP
		docker exec "$PEER" iptables -I OUTPUT -d "$tnip" -p udp -j DROP
		echo "UDP between linux-peer and $tnip is dropped" ;;
	off)
		docker exec "$PEER" iptables -D INPUT -s "$tnip" -p udp -j DROP 2>/dev/null || true
		docker exec "$PEER" iptables -D OUTPUT -d "$tnip" -p udp -j DROP 2>/dev/null || true
		echo "UDP between linux-peer and $tnip is allowed" ;;
	*) echo "usage: tb.sh block on|off" >&2; exit 2 ;;
	esac
}

down() {
	for c in tnbed-eng "$TN2" "$TN" "$PEER" "$HS" "$DERPER"; do
		if docker inspect "$c" >/dev/null 2>&1; then docker stop -t 2 "$c" >/dev/null; docker rm "$c" >/dev/null; echo "stopped $c"; fi
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
tn) docker exec "$TN" "$@" ;;
tn2) docker exec "$TN2" "$@" ;;
build-linux) build_linux ;;
test-linux) docker exec -w /src/tailnet "$TN" make -s SAN=1 BUILD=build/linux-asan run-tests ;;
block) block "${1:-}" ;;
reset) down; [ -d "$STATE" ] && mv "$STATE" "$STATE.old-$(date +%Y%m%d%H%M%S)"; echo "state moved aside" ;;
*) sed -n '2,21p' "$0"; exit 2 ;;
esac
