#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAYLOAD_PATH="${1:-$ROOT_DIR/payload2/ps5upload.elf}"
MARKER="reload_verify_replay_$(date +%s)"
TX_ID="reload-tx-$(date +%s)"
BODY="${2:-{\"tx_id\":\"$TX_ID\",\"kind\":\"upload_tree\",\"source\":\"reload_verify_replay\",\"marker\":\"$MARKER\"}}"
QUERY_BODY="{\"tx_id\":\"$TX_ID\"}"

echo "[replay] begin tx before reload"
"$ROOT_DIR/lab/begin-tx.sh" "$BODY"

echo "[replay] query before reload"
"$ROOT_DIR/lab/query-tx.sh" "$QUERY_BODY"

echo "[replay] resend payload"
"$ROOT_DIR/lab/send-payload.sh" "$PAYLOAD_PATH"

echo "[replay] waiting for runtime after reload"
for _ in $(seq 1 15); do
  if "$ROOT_DIR/lab/check-runtime-port.sh" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "[replay] status after reload"
STATUS_JSON="$("$ROOT_DIR/lab/status-runtime.sh")"
echo "$STATUS_JSON"

echo "[replay] query after reload"
QUERY_JSON="$("$ROOT_DIR/lab/query-tx.sh" "$QUERY_BODY")"
echo "$QUERY_JSON"

if python3 - <<'PY' <<<"$STATUS_JSON"
import json, sys
data = json.loads(sys.stdin.read())
value = int(data.get("recovered_transactions", 0))
sys.exit(0 if value >= 1 else 1)
PY
then
  echo "[replay] recovered_transactions >= 1"
else
  echo "[replay] recovered_transactions check failed" >&2
  "$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
  exit 1
fi

if printf '%s' "$QUERY_JSON" | grep -q "$MARKER"; then
  echo "[replay] marker survived reload"
else
  echo "[replay] marker missing from queried transaction record" >&2
  "$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
  exit 1
fi

if printf '%s' "$QUERY_JSON" | grep -q "\"tx_id\": \"$TX_ID\""; then
  echo "[replay] tx_id survived reload"
else
  echo "[replay] tx_id missing from queried transaction record" >&2
  "$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
  exit 1
fi

if printf '%s' "$QUERY_JSON" | grep -q '"state": "interrupted"'; then
  echo "[replay] interrupted state preserved across reload"
else
  echo "[replay] interrupted state missing after reload" >&2
  "$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
  exit 1
fi
