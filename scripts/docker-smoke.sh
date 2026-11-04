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
base="http://127.0.0.1:${port}${SMOKE_BASE_PATH:-}"
# SMOKE_BASE_PATH (e.g. /ps5) also runs the engine under that URL prefix, which it
# reads from PS5UPLOAD_BASE_URL at start-up. That checks the engine's routes and API
# under a prefix, on the one image CI ships. The web page takes its prefix when the
# image is built (Dockerfile.webui, VITE_BASE_URL), so this does not prove a page
# works there: REVERSE_PROXY.md has the build for that.
prefix_env=()
if [ -n "${SMOKE_BASE_PATH:-}" ]; then prefix_env=(-e "PS5UPLOAD_BASE_URL=${SMOKE_BASE_PATH}/"); fi

cleanup() {
  if [ "${ok:-0}" != 1 ]; then docker logs "$name" 2>&1 | tail -40 || true; fi
  docker rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

# 0.0.0.0/0 because the runner reaches the container through Docker's NAT,
# not loopback — exactly the case PS5UPLOAD_ALLOW_IP exists for.
docker run -d --name "$name" -p "${port}:19113" \
  -e PS5_ADDR=192.0.2.1 -e PS5UPLOAD_ALLOW_IP=0.0.0.0/0 ${prefix_env[@]+"${prefix_env[@]}"} "$image" >/dev/null

for _ in $(seq 1 30); do
  curl -fsS -m 2 "$base/api/jobs" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS -m 5 "$base/api/jobs" >/dev/null || fail "engine never answered GET /api/jobs"
echo "ok: engine answers through the published port"

# RAR archives are read by the separate ps5upload-unrar program next to the engine
# (UnRAR is never linked into the engine): an image without it cannot open a .rar.
docker run --rm --entrypoint /ps5upload-unrar "$image" --version | grep -q '^ps5upload-unrar ' \
  || fail "the RAR helper /ps5upload-unrar is missing or does not run"
echo "ok: the RAR helper is next to the engine"

# Reaching it by a LAN name works; a public name needs PS5UPLOAD_ALLOW_HOST, which the refusal names.
for host in nas nas.local ps5upload.home 192.168.1.10; do
  curl -fsS -m 5 -H "Host: $host" "$base/api/jobs" >/dev/null || fail "the Host $host was refused"
done
echo "ok: the engine answers when reached by a LAN name"
refused="$(curl -sS -m 5 -o /dev/null -w '%{http_code}' -H 'Host: rebound.example' "$base/api/jobs")"
[ "$refused" = 403 ] || fail "the Host rebound.example got $refused, not 403"
curl -sS -m 5 -H 'Host: rebound.example' "$base/api/jobs" | grep -q PS5UPLOAD_ALLOW_HOST \
  || fail "the refusal does not name PS5UPLOAD_ALLOW_HOST"
echo "ok: a public name is refused, and told which setting allows it"

tmp="$(mktemp -d)"
head -c 65536 /dev/urandom > "$tmp/smoke.pkg"
code="$(curl -sS -m 30 -o "$tmp/upload.json" -w '%{http_code}' -F "pkg=@$tmp/smoke.pkg" "$base/api/pkg/upload")"
[ "$code" = 200 ] || fail "package upload returned HTTP $code: $(cat "$tmp/upload.json")"
upload_id="$(sed -n 's/.*"upload_id":"\([^"]*\)".*/\1/p' "$tmp/upload.json")"
[ -n "$upload_id" ] || fail "upload response had no upload_id: $(cat "$tmp/upload.json")"
curl -fsS -m 10 -X DELETE "$base/api/pkg/upload/$upload_id" >/dev/null || fail "upload cleanup failed"
echo "ok: a package uploads to the engine's /tmp and cleans up"

# Generic browser intake and remote downloads share the same API prefix.
code="$(curl -sS -m 30 -o "$tmp/file.json" -w '%{http_code}' -F "file=@$tmp/smoke.pkg;filename=notes.txt" "$base/api/files/upload")"
[ "$code" = 200 ] || fail "browser file upload returned HTTP $code"
file_id="$(sed -n 's/.*"upload_id":"\([^"]*\)".*/\1/p' "$tmp/file.json")"
[ -n "$file_id" ] || fail "browser file response had no upload_id: $(cat "$tmp/file.json")"
curl -fsS -m 10 -X DELETE "$base/api/files/upload/$file_id" >/dev/null || fail "browser file cleanup failed"
code="$(curl -sS -m 5 -o "$tmp/download.json" -w '%{http_code}' "$base/api/pkg/remote/download/status?id=smoke-missing")"
[ "$code" = 404 ] && grep -q 'no such download' "$tmp/download.json" || fail "remote download API missed the configured prefix"
# Console URLs are generated at the root, independently of the browser prefix.
code="$(curl -sS -m 5 -o "$tmp/host.txt" -w '%{http_code}' "http://127.0.0.1:${port}/pkg-host/smoke-missing/file.pkg")"
[ "$code" = 404 ] && grep -q 'no such install session' "$tmp/host.txt" || fail "console package route is missing at the root"
echo "ok: browser intake and download APIs follow the prefix; console URLs remain reachable"

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
  # The page must find its own code: under a prefix, and from a deep link. An HTML reply to a
  # script is the blank page a passing check on the index used to hide.
  for page in "$base/" "$base/games/PPSA01234"; do
    script="$(curl -fsS -m 5 "$page" | grep -oE '<script[^>]+type="module"[^>]+src="[^"]+"' | head -1 | sed -E 's/.*src="([^"]+)"/\1/')" \
      || fail "$page did not load"
    [ -n "$script" ] || fail "$page names no script"
    case "$script" in /*) ;; *) fail "$page names a relative script ($script): a deep link cannot find it" ;; esac
    ctype="$(curl -fsS -m 5 -o /dev/null -w '%{content_type}' "http://127.0.0.1:${port}${script}")" \
      || fail "$script did not load"
    case "$ctype" in *javascript*) ;; *) fail "$script served $ctype, not JavaScript" ;; esac
  done
  echo "ok: the web UI finds its scripts, from a deep link too"
  curl -fsS -m 5 "$base/" | grep -q "name=\"ps5upload-base\" content=\"${SMOKE_BASE_PATH:-}\"" \
    || fail "the page was not told its prefix"
  echo "ok: the page knows its prefix"
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
  -e PS5_ADDR=192.0.2.1 -e PS5UPLOAD_ALLOW_IP=0.0.0.0/0 ${prefix_env[@]+"${prefix_env[@]}"} "$image" >/dev/null
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
