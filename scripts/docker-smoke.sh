#!/usr/bin/env bash
# Start a built engine image and check what a self-hoster hits first.
#
#   scripts/docker-smoke.sh <image> [--webui]
#
# CI used to build the images and never run them, so an image that answered
# every request with 403, had no writable /tmp for uploads and no home for its
# state shipped for several releases. Each check below is one of those.
set -euo pipefail

image="$1"
webui="${2:-}"
name="ps5upload-smoke-$$"
port="${SMOKE_PORT:-19313}"
base="http://127.0.0.1:${port}"

cleanup() {
  if [ "${ok:-0}" != 1 ]; then docker logs "$name" 2>&1 | tail -40 || true; fi
  docker rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

# 0.0.0.0/0 because the runner reaches the container through Docker's NAT,
# not loopback — exactly the case PS5UPLOAD_ALLOW_IP exists for.
docker run -d --name "$name" -p "${port}:19113" \
  -e PS5_ADDR=192.0.2.1:9113 -e PS5UPLOAD_ALLOW_IP=0.0.0.0/0 "$image" >/dev/null

for _ in $(seq 1 30); do
  curl -fsS -m 2 "$base/api/jobs" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS -m 5 "$base/api/jobs" >/dev/null || fail "engine never answered GET /api/jobs"
echo "ok: engine answers through the published port"

# Any Host header is served (the engine has no hostname allowlist): reaching it by a name works.
curl -fsS -m 5 -H 'Host: nas.example' "$base/api/jobs" >/dev/null || fail "a hostname Host header was refused"
echo "ok: the engine answers when reached by a hostname"

tmp="$(mktemp -d)"
head -c 65536 /dev/urandom > "$tmp/smoke.pkg"
code="$(curl -sS -m 30 -o "$tmp/upload.json" -w '%{http_code}' -F "pkg=@$tmp/smoke.pkg" "$base/api/pkg/upload")"
[ "$code" = 200 ] || fail "package upload returned HTTP $code: $(cat "$tmp/upload.json")"
upload_id="$(sed -n 's/.*"upload_id":"\([^"]*\)".*/\1/p' "$tmp/upload.json")"
[ -n "$upload_id" ] || fail "upload response had no upload_id: $(cat "$tmp/upload.json")"
curl -fsS -m 10 -X DELETE "$base/api/pkg/upload/$upload_id" >/dev/null || fail "upload cleanup failed"
echo "ok: a package uploads to the engine's /tmp and cleans up"

# The artwork cache is created under HOME at first use; a read-only home
# logs "Permission denied" and silently disables it.
curl -fsS -m 5 "$base/api/cache/artwork" >/dev/null || fail "artwork cache route failed"
if docker logs "$name" 2>&1 | grep -q "Permission denied"; then
  fail "the engine could not write its state (Permission denied in the log)"
fi
echo "ok: the engine's home is writable"

if [ "$webui" = "--webui" ]; then
  ctype="$(curl -fsS -m 5 -o /dev/null -w '%{content_type}' "$base/")" || fail "web UI index did not load"
  case "$ctype" in text/html*) ;; *) fail "web UI index served $ctype, not HTML" ;; esac
  echo "ok: the web UI loads"
fi

for _ in $(seq 1 40); do
  status="$(docker inspect -f '{{.State.Health.Status}}' "$name")"
  [ "$status" = healthy ] && break
  sleep 2
done
[ "$status" = healthy ] || fail "container health is '$status', not healthy"
echo "ok: the built-in health check reports healthy"

# A Compose `user:` override (#346: `user: "1000:1000"`) must still be able to
# stage uploads and write the engine's state.
docker rm -f "$name" >/dev/null 2>&1
docker run -d --name "$name" --user 1000:1000 -p "${port}:19113" \
  -e PS5_ADDR=192.0.2.1:9113 -e PS5UPLOAD_ALLOW_IP=0.0.0.0/0 "$image" >/dev/null
for _ in $(seq 1 30); do
  curl -fsS -m 2 "$base/api/jobs" >/dev/null 2>&1 && break
  sleep 1
done
code="$(curl -sS -m 30 -o "$tmp/upload.json" -w '%{http_code}' -F "pkg=@$tmp/smoke.pkg" "$base/api/pkg/upload")"
[ "$code" = 200 ] || fail "as uid 1000, package upload returned HTTP $code: $(cat "$tmp/upload.json")"
curl -fsS -m 5 "$base/api/cache/artwork" >/dev/null || fail "as uid 1000, artwork cache route failed"
if docker logs "$name" 2>&1 | grep -q "Permission denied"; then
  fail "as uid 1000, the engine could not write its state (Permission denied in the log)"
fi
echo "ok: a user: override (uid 1000) can upload and save state"

ok=1
echo "docker smoke test passed: $image"
