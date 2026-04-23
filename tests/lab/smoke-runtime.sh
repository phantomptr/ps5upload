#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAYLOAD_PATH="${1:-$ROOT_DIR/payload2/ps5upload.elf}"

echo "[smoke] sending payload"
"$ROOT_DIR/lab/send-payload.sh" "$PAYLOAD_PATH"

echo "[smoke] waiting for runtime port"
for _ in $(seq 1 10); do
  if "$ROOT_DIR/lab/check-runtime-port.sh"; then
    echo "[smoke] runtime port ready"
    echo "[smoke] hello"
    "$ROOT_DIR/lab/hello-runtime.sh"
    echo "[smoke] querying status"
    "$ROOT_DIR/lab/status-runtime.sh"
    exit 0
  fi
  sleep 1
done

echo "[smoke] runtime port did not become ready in time" >&2
"$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
exit 1
