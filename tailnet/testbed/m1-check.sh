#!/bin/sh
# M1 acceptance against the local test bed (never Tailscale's control server):
#   1. tnctl login --authkey registers; the node is in `headscale nodes list`;
#      the Linux peer's `tailscale status` lists it; our netmap lists the peer's 100.x IP.
#   2. pause/resume (rest mode hook) reconnects and gets a new netmap.
#   3. the interactive login-URL flow, approved with `headscale auth register`.
#   4. the same over TLS (BearSSL) through a local TLS relay with a throwaway CA.
# Exit status 0 = all passed. Leaves the test bed running (tb.sh down stops it).
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
TN="$HERE/.."
TB="$HERE/tb.sh"
RUN="$HERE/.state/m1-$(date +%Y%m%d-%H%M%S)"
TNCTL="$TN/build/host/tnctl"
CONTROL=http://127.0.0.1:18080
fails=0
pids=""

mkdir -p "$RUN"
export DOCKER_CONFIG="$HERE/.state/docker"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; fails=$((fails + 1)); }

# wait_for FILE PATTERN SECONDS
wait_for() {
	i=0
	while [ $i -lt $(($3 * 5)) ]; do
		grep -q "$2" "$1" 2>/dev/null && return 0
		sleep 0.2
		i=$((i + 1))
	done
	return 1
}

cleanup() {
	for p in $pids; do kill "$p" 2>/dev/null; done
}
trap cleanup EXIT INT TERM

echo "== build"
make -C "$TN" -s >/dev/null || { echo "build failed"; exit 1; }
echo "== test bed"
"$TB" up || exit 1
PEER_IP=$("$TB" peer tailscale ip -4)

echo "== 1. pre-auth key"
KEY=$("$TB" authkey)
"$TNCTL" login --control "$CONTROL" --authkey "$KEY" --state "$RUN/authkey" --hostname tn-m1-authkey -v \
	>"$RUN/authkey.log" 2>&1 &
P1=$!
pids="$pids $P1"
if wait_for "$RUN/authkey.log" "^netmap #" 30; then pass "registered with a pre-auth key; netmap received"; else fail "no netmap (authkey)"; fi
if grep -q "peer linux-peer.*$PEER_IP " "$RUN/authkey.log"; then pass "our netmap lists linux-peer $PEER_IP"; else fail "linux-peer $PEER_IP not in our netmap"; fi
SELF_IP=$(sed -n 's/^netmap #[0-9]*: self [^ ]* \([0-9.]*\) .*/\1/p' "$RUN/authkey.log" | head -1)
"$TB" hs nodes list >"$RUN/nodes.txt" 2>&1
if grep -q "tn-m1-authkey" "$RUN/nodes.txt"; then pass "headscale nodes list has tn-m1-authkey"; else fail "not in headscale nodes list"; fi
sleep 2 # let the peer receive its netmap update
"$TB" status >"$RUN/peer-status.txt" 2>&1
if grep -q "tn-m1-authkey" "$RUN/peer-status.txt" && grep -q "^$SELF_IP " "$RUN/peer-status.txt"; then
	pass "linux peer's tailscale status lists tn-m1-authkey at $SELF_IP"
else
	fail "linux peer does not list our node"
fi

echo "== 2. pause / resume"
before=$(grep -c "^netmap #" "$RUN/authkey.log")
kill -USR1 "$P1"
if wait_for "$RUN/authkey.log" "state: paused" 5; then pass "paused"; else fail "did not pause"; fi
kill -USR2 "$P1"
i=0
while [ $i -lt 100 ] && [ "$(grep -c "^netmap #" "$RUN/authkey.log")" -le "$before" ]; do sleep 0.2; i=$((i + 1)); done
if [ "$(grep -c "^netmap #" "$RUN/authkey.log")" -gt "$before" ]; then pass "resumed with a fresh map session"; else fail "no netmap after resume"; fi
kill "$P1"
wait "$P1" 2>/dev/null

echo "== 3. interactive login URL"
"$TNCTL" login --control "$CONTROL" --state "$RUN/interactive" --hostname tn-m1-interactive -v \
	>"$RUN/interactive.log" 2>&1 &
P2=$!
pids="$pids $P2"
if wait_for "$RUN/interactive.log" "^login URL: " 20; then
	URL=$(sed -n 's/^login URL: //p' "$RUN/interactive.log" | head -1)
	pass "login URL issued: $URL"
	AUTH_ID=${URL##*/}
	"$TB" register "$AUTH_ID" >"$RUN/register.txt" 2>&1 || cat "$RUN/register.txt"
	if wait_for "$RUN/interactive.log" "^netmap #" 30; then pass "followup completed after approval; netmap received"; else fail "no netmap after approval"; fi
	if grep -q "peer linux-peer.*$PEER_IP " "$RUN/interactive.log"; then pass "interactive node sees linux-peer $PEER_IP"; else fail "interactive node: peer missing"; fi
	if "$TB" hs nodes list 2>&1 | grep -q "tn-m1-interactive"; then pass "headscale nodes list has tn-m1-interactive"; else fail "interactive node not listed"; fi
else
	fail "no login URL"
fi
kill "$P2"
wait "$P2" 2>/dev/null

echo "== 4. TLS (BearSSL) through a local relay"
CA="$RUN/ca"
mkdir -p "$CA"
openssl ecparam -name prime256v1 -genkey -noout -out "$CA/ca.key" 2>/dev/null
openssl req -x509 -new -key "$CA/ca.key" -sha256 -days 2 -subj "/CN=tnbed test CA" \
	-addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
	-out "$CA/ca.pem" 2>/dev/null
openssl ecparam -name prime256v1 -genkey -noout -out "$CA/leaf.key" 2>/dev/null
openssl req -new -key "$CA/leaf.key" -subj "/CN=localhost" -out "$CA/leaf.csr" 2>/dev/null
printf "subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n" >"$CA/ext.cnf"
openssl x509 -req -in "$CA/leaf.csr" -CA "$CA/ca.pem" -CAkey "$CA/ca.key" -CAcreateserial -days 2 -sha256 \
	-extfile "$CA/ext.cnf" -out "$CA/leaf.pem" 2>/dev/null
python3 "$HERE/tlsproxy.py" "$CA/leaf.pem" "$CA/leaf.key" 18443 18080 >"$RUN/tlsproxy.log" 2>&1 &
P3=$!
pids="$pids $P3"
wait_for "$RUN/tlsproxy.log" "tlsproxy:" 5
"$TNCTL" login --control https://localhost:18443 --ca-file "$CA/ca.pem" --authkey "$KEY" --state "$RUN/tls" \
	--hostname tn-m1-tls -v --once --timeout 30 >"$RUN/tls.log" 2>&1
if grep -q "dialing localhost:18443 (tls)" "$RUN/tls.log" && grep -q "^netmap #" "$RUN/tls.log"; then
	pass "/key and ts2021 over TLS 1.2 (BearSSL, private CA); netmap received"
else
	fail "TLS run"
fi
"$TNCTL" login --control https://localhost:18443 --authkey "$KEY" --state "$RUN/tls-noca" --hostname tn-m1-tls-noca \
	--once --timeout 8 >"$RUN/tls-noca.log" 2>&1
if grep -q "TLS error" "$RUN/tls-noca.log" && ! grep -q "^netmap #" "$RUN/tls-noca.log"; then
	pass "without --ca-file the private CA is rejected (only ISRG roots are trusted)"
else
	fail "untrusted certificate was not rejected"
fi
kill "$P3"

echo "== logs in $RUN"
for f in authkey interactive tls; do
	echo "---- $f.log"
	# endpoint IPs are masked here (the raw logs keep them)
	grep -v "^log: map keep-alive" "$RUN/$f.log" | sed -E 's/(ep |,)[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+:/\1<ip>:/g' | head -40
done
echo "---- headscale nodes list"
"$TB" nodes 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | cut -c1-200
echo "---- linux peer: tailscale status"
cat "$RUN/peer-status.txt"
echo
if [ $fails -eq 0 ]; then echo "M1 CHECK: ALL PASSED"; else echo "M1 CHECK: $fails FAILED"; fi
exit $fails
