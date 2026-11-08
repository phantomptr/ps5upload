#!/bin/sh
# M4 acceptance: the engine's side and the console's side, both our own node.
#   box tn  ("PS5"):    tnctl --serve tcp:9295,tcp:9120,udp:9296,udp:9302, with local echo
#                       services on 127.0.0.1 standing in for Remote Play and the helper
#   box tn2:            tnctl --map, the C core mapping 127.0.0.1 ports to the console's
#                       tailnet address, the engine's role (sections 1-3)
#   box eng:            the real engine (ps5upload-engine built for Linux in rust:1), signed in
#                       through POST /api/tailnet; its router maps an away console (192.0.2.77,
#                       where nothing answers) to the console node (section 4; ENGINE=0 skips it)
# Traffic through the engine's local mapping, shaped like a game session:
#   TCP 9295 (session round trips), UDP 9296 (1200-byte datagrams at 1000/s, ~10 Mbit/s
#   each way), TCP 9120 (the helper: round trips and a 20 MiB transfer),
# first direct, then DERP only (the engine node with --no-direct).
# Exit status = number of failed checks. Stops nothing (tb.sh down does).
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
TB="$HERE/tb.sh"
RUN="$HERE/.state/m4-$(date +%Y%m%d-%H%M%S)"
B=/src/tailnet/testbed/.state/bin
CONTROL=http://tnbed-headscale:8080
fails=0
mkdir -p "$RUN"
export DOCKER_CONFIG="$HERE/.state/docker"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; fails=$((fails + 1)); }

echo "== bed + Linux build"
"$TB" up || exit 1
"$TB" build-linux || exit 1
KEY=$("$TB" authkey)
"$TB" tn sh -c "pkill -x tnctl; pkill -x tnecho; ($B/tnecho serve-tcp 9295 127.0.0.1 &); ($B/tnecho serve-tcp 9120 127.0.0.1 &)
	($B/tnecho serve-udp 9296 127.0.0.1 &); true"
"$TB" tn2 sh -c 'pkill -x tnctl; pkill -x ps5upload-engine; true'
"$TB" tn sh -c "cd /src/tailnet && (timeout 900 build/linux-asan/tnctl login --control $CONTROL --authkey $KEY \
	--state /tmp/m4-ps5 --hostname tn-ps5 --ca-file /ca.pem -v --serve tcp:9295,tcp:9120,udp:9296,udp:9302 \
	>/tmp/m4-ps5.log 2>&1 &)"
i=0
until "$TB" tn grep -q "^netmap #" /tmp/m4-ps5.log 2>/dev/null || [ $i -ge 30 ]; do sleep 1; i=$((i + 1)); done
PS5=$("$TB" tn sh -c "sed -n 's/^netmap #[0-9]*: self [^ ]* \([0-9.]*\) .*/\1/p' /tmp/m4-ps5.log | head -1")
echo "   console node at $PS5"

engine_up() { # $1 = extra tnctl flags (e.g. --no-direct)
	"$TB" tn2 sh -c 'pkill -x tnctl; true'
	sleep 1
	"$TB" tn2 sh -c "cd /src/tailnet && (timeout 600 build/linux-asan/tnctl login --control $CONTROL --authkey $KEY \
		--state /tmp/m4-engine --hostname tn-engine --ca-file /ca.pem -v $1 \
		--map tcp:19295:$PS5:9295,udp:19296:$PS5:9296,tcp:19120:$PS5:9120 >/tmp/m4-engine.log 2>&1 &)"
	i=0
	until "$TB" tn2 grep -q "^netmap #" /tmp/m4-engine.log 2>/dev/null || [ $i -ge 30 ]; do sleep 1; i=$((i + 1)); done
	sleep 3
}

session() { # $1 = label, $2 = expected path word
	o=$("$TB" tn2 $B/tnecho tcp 127.0.0.1 19295 50); echo "   $o"
	case "$o" in *"50/50 round trips"*) pass "$1: Remote Play session (TCP 9295) through the local mapping" ;; *) fail "$1: TCP 9295" ;; esac
	if "$TB" tn2 $B/tnecho flood 127.0.0.1 19296 3000 1000 >"$RUN/flood-$2.txt" 2>&1; then
		echo "   $(cat "$RUN/flood-$2.txt")"; pass "$1: Remote Play stream (UDP 9296, 1000 x 1200 B/s), <= 1% loss"
	else echo "   $(cat "$RUN/flood-$2.txt")"; fail "$1: UDP 9296 stream"; fi
	o=$("$TB" tn2 $B/tnecho tcp 127.0.0.1 19120 50); echo "   $o"
	case "$o" in *"50/50 round trips"*) pass "$1: helper (TCP 9120) round trips" ;; *) fail "$1: TCP 9120" ;; esac
	o=$("$TB" tn2 $B/tnecho bulk 127.0.0.1 19120 20); echo "   $o"
	case "$o" in *"20971520 of 20971520 bytes echoed in"*) pass "$1: 20 MiB through the helper port" ;; *) fail "$1: bulk 9120" ;; esac
	p=$("$TB" tn2 sh -c "grep '^path: ' /tmp/m4-engine.log | tail -1")
	echo "   engine node: $p"
	case "$p" in *"$2"*) pass "$1: path is $2" ;; *) fail "$1: path not $2" ;; esac
}

echo "== 1. direct"
engine_up ""
"$TB" tn2 grep "^map: " /tmp/m4-engine.log | sed 's/^/   /'
session "direct" "direct"

echo "== 2. DERP only (engine node --no-direct)"
engine_up "--no-direct"
session "relayed" "relayed"

echo "== 3. nothing else is reachable (a service listens on the console's port 22)"
"$TB" tn sh -c "($B/tnecho serve-tcp 22 127.0.0.1 &); true"
"$TB" tn2 sh -c 'pkill -x tnctl; true'
o=$("$TB" tn2 sh -c "cd /src/tailnet && timeout 60 build/linux-asan/tnctl login --control $CONTROL --authkey $KEY \
	--state /tmp/m4-engine --hostname tn-engine --ca-file /ca.pem --map tcp:19022:$PS5:22 --timeout 12 >/tmp/m4-22.log 2>&1 &
	sleep 8; $B/tnecho tcp 127.0.0.1 19022 1")
# (the local listener always accepts; what matters is that nothing answers behind it)
echo "   TCP 22 through a mapping: $o"
case "$o" in *"1/1 round trips"*) fail "port 22 reachable through a mapping" ;; *) pass "a non-allow-listed port answers nothing, even through a mapping" ;; esac

"$TB" tn2 sh -c 'pkill -x tnctl; true'

if [ "${ENGINE:-1}" = 1 ]; then
	echo "== 4. the real engine: its own node, its router, its mappings"
	REPO=$(cd "$HERE/../.." && pwd)
	ENG=tnbed-eng
	AWAY=192.0.2.77 # TEST-NET: the console's "home" address, which never answers here
	echo "   building ps5upload-engine for Linux (rust:1)..."
	if ! docker run --rm -v "$REPO:/src" -w /src/engine -e CARGO_TARGET_DIR=/src/engine/target/docker-linux \
		docker.io/library/rust:1 cargo build -q -p ps5upload-engine >"$RUN/engine-build.log" 2>&1; then
		tail -20 "$RUN/engine-build.log"
		fail "engine build"
	else
		eng_api() { docker exec "$ENG" curl -fsS "$@"; }
		eng_post() { eng_api -X POST -H 'Content-Type: application/json' http://127.0.0.1:19113/api/tailnet -d "$1"; }
		eng_up() { # $1 = PS5UPLOAD_TAILNET_NO_DIRECT
			docker rm -f "$ENG" >/dev/null 2>&1
			docker run -d --name "$ENG" --hostname tn-engine-app --network tnbed -v "$REPO:/src:ro" \
				-v "$HERE/.state/ca/ca.pem:/ca.pem:ro" -e PS5UPLOAD_DATA_DIR=/tmp/eng \
				-e PS5UPLOAD_TAILNET_CA_FILE=/ca.pem -e PS5UPLOAD_TAILNET_NO_DIRECT="$1" \
				--entrypoint /src/engine/target/docker-linux/debug/ps5upload-engine docker.io/library/rust:1 >/dev/null
			i=0
			until eng_api http://127.0.0.1:19113/api/tailnet >/dev/null 2>&1 || [ $i -ge 60 ]; do sleep 1; i=$((i + 1)); done
			eng_post "{\"op\":\"enable\",\"control_url\":\"$CONTROL\",\"hostname\":\"tn-engine-app\",\"authkey\":\"$KEY\"}" >/dev/null
			i=0
			until eng_api http://127.0.0.1:19113/api/tailnet 2>/dev/null | grep -q '"state":"running"' || [ $i -ge 60 ]; do sleep 1; i=$((i + 1)); done
			eng_post "{\"op\":\"remember\",\"host\":\"$AWAY\",\"ip\":\"$PS5\"}" >/dev/null
			sleep 4
		}
		eng_route() { # port udp -> the local port
			eng_api "http://127.0.0.1:19113/api/tailnet/route?addr=$AWAY&port=$1&udp=$2" |
				sed -n 's/.*"dial":"127.0.0.1:\([0-9]*\)".*/\1/p'
		}
		eng_session() { # label, expected via
			S=$(eng_route 9295 false)
			U=$(eng_route 9296 true)
			H=$(eng_route 9120 false)
			echo "   engine maps $AWAY: 9295/tcp -> :$S, 9296/udp -> :$U, 9120/tcp -> :$H"
			if [ -n "$S" ] && [ -n "$U" ] && [ -n "$H" ]; then pass "$1: the engine routes the away console through its node"; else fail "$1: no route"; fi
			o=$(docker exec "$ENG" $B/tnecho tcp 127.0.0.1 "$S" 50); echo "   $o"
			case "$o" in *"50/50 round trips"*) pass "$1: Remote Play session (TCP 9295) via the engine" ;; *) fail "$1: engine TCP 9295" ;; esac
			if docker exec "$ENG" $B/tnecho flood 127.0.0.1 "$U" 3000 1000 >"$RUN/eng-flood-$2.txt" 2>&1; then
				echo "   $(cat "$RUN/eng-flood-$2.txt")"; pass "$1: Remote Play stream (UDP 9296) via the engine, <= 1% loss"
			else
				echo "   $(cat "$RUN/eng-flood-$2.txt")"; fail "$1: engine UDP 9296"
			fi
			o=$(docker exec "$ENG" $B/tnecho bulk 127.0.0.1 "$H" 20); echo "   $o"
			case "$o" in *"20971520 of 20971520 bytes echoed in"*) pass "$1: 20 MiB through the helper port via the engine" ;; *) fail "$1: engine bulk 9120" ;; esac
			v=$(eng_api "http://127.0.0.1:19113/api/tailnet/via?addr=$AWAY")
			echo "   connection chip: $v"
			case "$v" in *"\"via\":\"$2\""*) pass "$1: the chip says via Tailscale ($2)" ;; *) fail "$1: via is not $2" ;; esac
		}
		eng_up 0
		eng_session "engine direct" "direct"
		eng_api http://127.0.0.1:19113/api/tailnet >"$RUN/engine-status.json" 2>&1
		eng_up 1
		eng_session "engine relayed" "relayed"
		v=$(eng_api "http://127.0.0.1:19113/api/tailnet/route?addr=192.0.2.78&port=9120&udp=false")
		case "$v" in *'"dial":null'*) pass "a console the engine knows nothing about is dialed as it is" ;; *) fail "unknown console routed: $v" ;; esac
		docker logs "$ENG" >"$RUN/engine.out" 2>&1
		docker rm -f "$ENG" >/dev/null 2>&1
	fi
fi

"$TB" tn sh -c 'pkill -INT -x tnctl; true'
sleep 2
"$TB" tn cat /tmp/m4-ps5.log >"$RUN/ps5.log" 2>&1
"$TB" tn2 cat /tmp/m4-engine.log >"$RUN/engine.log" 2>&1
"$TB" tn sh -c 'pkill -x tnecho; true'
if grep -q "Sanitizer\|runtime error" "$RUN/ps5.log" "$RUN/engine.log"; then fail "sanitizer report"; else pass "no ASan/UBSan report on either node"; fi
echo "== logs in $RUN"
if [ $fails -eq 0 ]; then echo "M4 CHECK: ALL PASSED"; else echo "M4 CHECK: $fails FAILED"; fi
exit $fails
