#!/bin/sh
# M3 acceptance: our node as a forwarder, exactly as the PS5 helper runs it
# (tnctl --serve tcp:9295,tcp:9120,udp:9296,udp:9302 -> 127.0.0.1), with the stock
# tailscaled peer as the remote player. Linux, ASan+UBSan build.
#   1. TCP 9120 through lwIP to a local echo: round trips, 50 MiB bulk; UDP 9296/9302 relay
#   2. a served port with nothing listening locally answers "refused" (RST), then works
#   3. not on the allow-list: TCP 22, UDP 9297 dropped even with a local listener
#   4. four TCP connections at once
#   5. the tailnet ACL (Headscale policy) is enforced on top of the allow-list
#   6. the same over DERP (UDP between the nodes dropped)
#   7. pause/resume: connections drop, new ones work after resume
# Exit status = number of failed checks. Leaves the bed running (tb.sh down stops it).
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
TB="$HERE/tb.sh"
RUN="$HERE/.state/m3-$(date +%Y%m%d-%H%M%S)"
POL="$HERE/.state/headscale/lib"
fails=0
mkdir -p "$RUN"
export DOCKER_CONFIG="$HERE/.state/docker"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; fails=$((fails + 1)); }
peer() { "$TB" peer /tnbin/tnecho "$@"; }
policy() { # FILE: install a Headscale ACL policy (database mode)
	"$TB" hs policy set -f "/var/lib/headscale/$1" >/dev/null 2>&1 || echo "   (policy set $1 failed)"
}

echo "== bed + Linux build"
"$TB" up || exit 1
"$TB" build-linux || exit 1
"$TB" block off >/dev/null
cat >"$POL/policy-allow.json" <<'EOF'
{"acls": [{"action": "accept", "src": ["*"], "dst": ["*:*"]}]}
EOF
cat >"$POL/policy-22only.json" <<'EOF'
{"acls": [{"action": "accept", "src": ["*"], "dst": ["*:22"]}]}
EOF
policy policy-allow.json
KEY=$("$TB" authkey)
"$TB" tn sh -c 'pkill -x tnctl; pkill -x tnecho; B=/src/tailnet/testbed/.state/bin
	($B/tnecho serve-tcp 9120 127.0.0.1 &); ($B/tnecho serve-udp 9296 127.0.0.1 &)
	($B/tnecho serve-udp 9302 127.0.0.1 &); ($B/tnecho serve-udp 9297 127.0.0.1 &)
	($B/tnecho serve-tcp 22 127.0.0.1 &); true'
"$TB" tn sh -c "cd /src/tailnet && (timeout 600 build/linux-asan/tnctl login --control http://tnbed-headscale:8080 \
	--authkey $KEY --state /tmp/m3 --hostname tn-ps5 --ca-file /ca.pem -v \
	--serve tcp:9295,tcp:9120,udp:9296,udp:9302 >/tmp/serve.log 2>&1 &)"
i=0
until "$TB" tn grep -q "^netmap #" /tmp/serve.log 2>/dev/null || [ $i -ge 30 ]; do sleep 1; i=$((i + 1)); done
IP=$("$TB" tn sh -c "sed -n 's/^netmap #[0-9]*: self [^ ]* \([0-9.]*\) .*/\1/p' /tmp/serve.log | head -1")
echo "   forwarder node tn-ps5 at $IP"
sleep 3

echo "== 1. served ports"
o=$(peer tcp "$IP" 9120 50); echo "   $o"
case "$o" in *"50/50 round trips"*) pass "TCP 9120 via lwIP to the local service" ;; *) fail "TCP 9120" ;; esac
o=$(peer bulk "$IP" 9120 50); echo "   $o"
case "$o" in *"52428800 of 52428800 bytes echoed in"*) case "$o" in *CORRUPT*) fail "bulk corrupt" ;; *) pass "50 MiB bulk through TCP 9120, intact" ;; esac ;; *) fail "bulk 9120" ;; esac
o=$(peer udp "$IP" 9296 20); echo "   $o"
case "$o" in *"20/20 answered"*) pass "UDP 9296 relayed" ;; *) fail "UDP 9296" ;; esac
o=$(peer udp "$IP" 9302 5); echo "   $o"
case "$o" in *"5/5 answered"*) pass "UDP 9302 relayed" ;; *) fail "UDP 9302" ;; esac

echo "== 2. served port, nothing listening locally"
o=$(peer connect "$IP" 9295); echo "   TCP 9295: $o"
[ "$o" = refused ] && pass "closed local port answers RST (refused)" || fail "9295 should be refused"
"$TB" tn sh -c '(/src/tailnet/testbed/.state/bin/tnecho serve-tcp 9295 127.0.0.1 &); true'
sleep 1
o=$(peer tcp "$IP" 9295 10); echo "   $o"
case "$o" in *"10/10 round trips"*) pass "TCP 9295 works once the local service listens" ;; *) fail "TCP 9295 after listen" ;; esac

echo "== 3. not on the allow-list"
o=$(peer connect "$IP" 22); echo "   TCP 22: $o"
[ "$o" = timeout ] && pass "TCP 22 dropped (a local service is listening on it)" || fail "TCP 22 not dropped"
o=$(peer udp "$IP" 9297 3); echo "   $o"
case "$o" in *"0/3 answered"*) pass "UDP 9297 dropped (a local service is listening on it)" ;; *) fail "UDP 9297 not dropped" ;; esac

echo "== 4. four TCP connections at once"
for n in 1 2 3 4; do (peer tcp "$IP" 9120 100 >"$RUN/par$n.txt" 2>&1 &); done
sleep 1
i=0
while [ "$(cat "$RUN"/par*.txt 2>/dev/null | grep -c 'round trips')" -lt 4 ] && [ $i -lt 30 ]; do sleep 1; i=$((i + 1)); done
ok=$(grep -l "100/100 round trips" "$RUN"/par*.txt | wc -l | tr -d ' ')
sed 's/^/   /' "$RUN"/par1.txt
[ "$ok" = 4 ] && pass "4 parallel connections, 100 round trips each" || fail "parallel: $ok of 4"

echo "== 5. tailnet ACL enforced (policy: only port 22)"
policy policy-22only.json
sleep 4
o=$(peer connect "$IP" 9120); echo "   TCP 9120 under the ACL: $o"
[ "$o" = timeout ] && pass "ACL denies TCP 9120" || fail "ACL did not deny TCP 9120"
o=$(peer udp "$IP" 9296 3); echo "   $o"
case "$o" in *"0/3 answered"*) pass "ACL denies a new UDP 9296 flow" ;; *) fail "ACL did not deny UDP 9296" ;; esac
policy policy-allow.json
sleep 4
o=$(peer tcp "$IP" 9120 10); echo "   after restoring the ACL: $o"
case "$o" in *"10/10 round trips"*) pass "allowed again when the ACL allows" ;; *) fail "not allowed after ACL restore" ;; esac

echo "== 6. over DERP"
"$TB" block on
sleep 8
o=$(peer tcp "$IP" 9120 20); echo "   $o"
case "$o" in *"20/20 round trips"*) pass "TCP 9120 over DERP" ;; *) fail "TCP over DERP" ;; esac
o=$(peer bulk "$IP" 9120 10); echo "   $o"
case "$o" in *"10485760 of 10485760 bytes echoed in"*) pass "10 MiB bulk over DERP" ;; *) fail "bulk over DERP" ;; esac
o=$(peer udp "$IP" 9296 10); echo "   $o"
case "$o" in *"10/10 answered"*) pass "UDP 9296 over DERP" ;; *) fail "UDP over DERP" ;; esac
"$TB" block off
"$TB" tn grep "^path: " /tmp/serve.log | tail -3 | sed 's/^/   /'

echo "== 7. pause / resume"
(peer bulk "$IP" 9120 400 >"$RUN/pause-bulk.txt" 2>&1 &)
sleep 1
"$TB" tn pkill -USR1 -x tnctl
sleep 4
"$TB" tn pkill -USR2 -x tnctl
i=0
while ! grep -q "bulk" "$RUN/pause-bulk.txt" 2>/dev/null && [ $i -lt 30 ]; do sleep 1; i=$((i + 1)); done
echo "   during pause: $(cat "$RUN/pause-bulk.txt")"
sleep 3
o=$(peer tcp "$IP" 9120 10); echo "   after resume: $o"
case "$o" in *"10/10 round trips"*) pass "new connections work after resume" ;; *) fail "after resume" ;; esac

"$TB" tn pkill -INT -x tnctl
sleep 2
"$TB" tn cat /tmp/serve.log >"$RUN/serve.log" 2>&1
"$TB" tn sh -c 'pkill -x tnecho; true'
if grep -q "Sanitizer\|runtime error" "$RUN/serve.log"; then fail "sanitizer report in the forwarder log"; else pass "no ASan/UBSan report in the forwarder"; fi
echo "== logs in $RUN"
if [ $fails -eq 0 ]; then echo "M3 CHECK: ALL PASSED"; else echo "M3 CHECK: $fails FAILED"; fi
exit $fails
