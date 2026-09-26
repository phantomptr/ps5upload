#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "[takeover] current status"
BEFORE="$("$ROOT_DIR/lab/status-runtime.sh")"
echo "$BEFORE"

echo "[takeover] requesting takeover"
"$ROOT_DIR/lab/takeover-runtime.sh"

echo "[takeover] waiting for runtime to drop"
for _ in $(seq 1 10); do
  if ! "$ROOT_DIR/lab/check-runtime-port.sh" >/dev/null 2>&1; then
    echo "[takeover] runtime port closed after takeover request"
    exit 0
  fi
  sleep 1
done

echo "[takeover] runtime port did not close in time" >&2
"$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
exit 1
