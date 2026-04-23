#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAYLOAD_PATH="${1:-$ROOT_DIR/payload2/ps5upload.elf}"

echo "[reload] initial status"
"$ROOT_DIR/lab/status-runtime.sh" || true

echo "[reload] resending payload to trigger takeover"
"$ROOT_DIR/lab/send-payload.sh" "$PAYLOAD_PATH"

echo "[reload] waiting for runtime readiness after reload"
for _ in $(seq 1 15); do
  if "$ROOT_DIR/lab/check-runtime-port.sh" >/dev/null 2>&1; then
    echo "[reload] runtime reachable after reload"
    "$ROOT_DIR/lab/status-runtime.sh"
    exit 0
  fi
  sleep 1
done

echo "[reload] runtime did not recover after resend; capturing trace" >&2
"$ROOT_DIR/lab/capture-runtime-trace.sh" >&2
exit 1
