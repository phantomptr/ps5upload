#!/usr/bin/env bash
# Turnkey verification for the two checks that need physical hardware the dev
# box lacks (a FW11 console; a real installable .pkg; a console that stays
# awake). Everything else in the backport / stream-install / patch-fix work is
# already unit-tested, CI-green, and live-verified on 9.60 — see CHANGELOG and
# the 2026-09-08/09 design docs. Run this when the hardware exists.
#
#   PS5=192.168.86.100 ENGINE=http://127.0.0.1:19113 tests/lab/verify-pending.sh stream /path/to/real-game.pkg
#   (FW11 console)                                    tests/lab/verify-pending.sh patch  PPSA-with-a-PS4-update
set -euo pipefail
PS5="${PS5:-192.168.86.100}"
ENGINE="${ENGINE:-http://127.0.0.1:19113}"
MODE="${1:-help}"

need_up() {
  ping -c1 -W2000 "$PS5" >/dev/null 2>&1 || { echo "ABORT: $PS5 is not reachable — wake the console / re-run your loader."; exit 1; }
  nc -z -G3 "$PS5" 9114 2>/dev/null || { echo "ABORT: payload port :9114 closed — load ps5upload first (make send-payload PS5_HOST=$PS5)."; exit 1; }
}

case "$MODE" in
  stream)
    # (2) Full browser/Docker stream install end-to-end with a REAL package.
    # Replicates handleBrowserStreamFile: upload -> serve_only -> dpi-ensure
    # -> dpi-direct-install -> payload-restore. NOTE ps5_addr (not `ip`) is the
    # dpi-ensure field — the manual smoke earlier got this wrong.
    PKG="${2:?usage: verify-pending.sh stream /path/to/game.pkg}"
    [ -f "$PKG" ] || { echo "no such pkg: $PKG"; exit 1; }
    MAGIC=$(xxd -l4 -p "$PKG"); [ "$MAGIC" = "7f434e54" ] || { echo "ABORT: $PKG magic=$MAGIC, not a PS4/PS5 package (7f434e54)."; exit 1; }
    need_up
    ADDR="$PS5:9114"
    echo "1. upload"; UP=$(curl -s -X POST "$ENGINE/api/pkg/upload" -F "f=@$PKG"); PATHV=$(jq -r .path <<<"$UP"); UID=$(jq -r .upload_id <<<"$UP"); echo "   -> $PATHV"
    CID=$(curl -s -X POST "$ENGINE/api/pkg/parse" -H 'content-type: application/json' -d "{\"path\":\"$PATHV\"}" | jq -r '.head.content_id // .content_id')
    echo "2. install/start serve_only (content_id=$CID)"
    SID=$(curl -s -X POST "$ENGINE/api/pkg/install/start" -H 'content-type: application/json' \
      -d "{\"ps5_addr\":\"$ADDR\",\"path\":\"$PATHV\",\"content_id\":\"$CID\",\"serve_only\":true,\"delete_staging\":false}" | jq -r .session_id)
    echo "   -> session $SID"
    echo "3. dpi-ensure (deploy DPI daemon via :9021)"; curl -s -X POST "$ENGINE/api/pkg/dpi-ensure" -H 'content-type: application/json' -d "{\"ps5_addr\":\"$ADDR\"}" | jq -c '{ok,listening,sent,reason,error}'
    echo "4. dpi-direct-install (DPI pulls over HTTP + installs)"; curl -s -X POST "$ENGINE/api/pkg/dpi-direct-install" -H 'content-type: application/json' -d "{\"ps5_addr\":\"$ADDR\",\"session_id\":\"$SID\"}" | jq -c '{ok,installed,rc,err_message,error}'
    echo "5. payload-restore"; curl -s -X POST "$ENGINE/api/pkg/payload-restore" -H 'content-type: application/json' -d "{\"ps5_addr\":\"$ADDR\"}" | jq -c '{ok,restored,error}'
    echo "6. cleanup"; curl -s -X DELETE "$ENGINE/api/pkg/upload/$UID" >/dev/null; echo "   done"
    echo "PASS if step 4 reports installed:true and the title appears on the PS5."
    ;;

  patch)
    # (1) FW11 patch-fix efficacy. Requires a FW11 console and a title that has
    # a pending PS4 update which currently fails in-process with 0x80B2116F.
    TID="${2:?usage: verify-pending.sh patch PPSAxxxxx}"
    echo "On a FW11 console:"
    echo "  1. make payload   # already builds the PS5UPLOAD_FULL_ESCALATE path (default off)"
    echo "  2. Load a payload built with the escalation forced on, OR set the env at load time:"
    echo "       PS5UPLOAD_FULL_ESCALATE=1"
    echo "  3. Install the PS4 update for $TID via ps5upload (the in-process path)."
    echo "  4. PASS if InstallByPackage returns 0 (was 0x80B2116F); the base game is never at risk"
    echo "     (the escalation wraps the non-wiping Tier-1 call). See payload/src/bgft.c and"
    echo "     payload/include/bgft_escalate.h."
    ;;

  *)
    echo "usage: verify-pending.sh {stream <game.pkg> | patch <title_id>}"
    ;;
esac
