#!/usr/bin/env bash
# Turnkey hardware check for the browser/Docker stream install with a REAL
# package, through the unified install endpoint. Run it against a console that
# already has the loader's prerequisite payloads running.
#
#   PS5=192.168.86.99 ENGINE=http://127.0.0.1:19113 tests/lab/verify-pending.sh stream /path/to/real-game.pkg
set -euo pipefail
PS5="${PS5:-192.168.86.99}"
ENGINE="${ENGINE:-http://127.0.0.1:19113}"
MODE="${1:-help}"

need_up() {
  ping -c1 -W2000 "$PS5" >/dev/null 2>&1 || { echo "ABORT: $PS5 is not reachable — wake the console / re-run your loader."; exit 1; }
  nc -z -G3 "$PS5" 9114 2>/dev/null || { echo "ABORT: payload port :9114 closed — load ps5upload first (make send-payload PS5_HOST=$PS5)."; exit 1; }
}

case "$MODE" in
  stream)
    # Replicates handleBrowserStreamFile: upload the pkg to the engine host, then
    # one POST /api/pkg/install with a host_file source. The engine creates the
    # pkg-host session, brings up the installer daemon, hands it the URL, waits
    # until the console has pulled the whole package, and verifies.
    PKG="${2:?usage: verify-pending.sh stream /path/to/game.pkg}"
    [ -f "$PKG" ] || { echo "no such pkg: $PKG"; exit 1; }
    MAGIC=$(xxd -l4 -p "$PKG"); [ "$MAGIC" = "7f434e54" ] || { echo "ABORT: $PKG magic=$MAGIC, not a PS4/PS5 package (7f434e54)."; exit 1; }
    need_up
    ADDR="$PS5:9114"
    echo "1. upload"; UP=$(curl -s -X POST "$ENGINE/api/pkg/upload" -F "f=@$PKG"); PATHV=$(jq -r .path <<<"$UP"); UPID=$(jq -r .upload_id <<<"$UP"); echo "   -> $PATHV"
    HEAD=$(curl -s -X POST "$ENGINE/api/pkg/parse" -H 'content-type: application/json' -d "{\"path\":\"$PATHV\"}")
    CID=$(jq -r '.head.content_id // .content_id' <<<"$HEAD"); TID=$(jq -r '.head.title_id // .title_id' <<<"$HEAD"); CAT=$(jq -r '.head.package_type // .package_type' <<<"$HEAD")
    echo "2. install (content_id=$CID)"
    JOB=$(curl -s -X POST "$ENGINE/api/pkg/install" -H 'content-type: application/json' \
      -d "{\"ps5_addr\":\"$ADDR\",\"source\":{\"host_file\":\"$PATHV\"},\"content_id\":\"$CID\",\"title_id\":\"$TID\",\"category\":\"$CAT\",\"options\":{\"allow_destructive_reinstall\":true}}" | jq -r .job)
    echo "   -> job $JOB"
    echo "3. status"
    while :; do
      S=$(curl -s "$ENGINE/api/pkg/install/status?job=$JOB")
      echo "   $(jq -c '{phase,verdict,served:.metrics.served_bytes,total:.metrics.total_bytes}' <<<"$S")"
      case "$(jq -r .phase <<<"$S")" in done|failed) break;; esac
      sleep 3
    done
    echo "4. cleanup"; curl -s -X DELETE "$ENGINE/api/pkg/upload/$UPID" >/dev/null; echo "   done"
    echo "PASS if the verdict is installed and the title launches on the PS5."
    ;;

  *)
    echo "usage: verify-pending.sh stream <game.pkg>"
    ;;
esac
