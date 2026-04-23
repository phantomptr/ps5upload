#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TRACE_DIR="${TRACE_DIR:-$ROOT_DIR/lab/traces}"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="$TRACE_DIR/$STAMP"

mkdir -p "$OUT_DIR"

{
  echo "timestamp=$STAMP"
  echo "ps5_ip=${PS5_IP:-192.168.137.2}"
  echo "runtime_port=${RUNTIME_PORT:-9113}"
  echo "payload_loader_port=${PS5_PORT:-9021}"
} > "$OUT_DIR/context.txt"

if "$ROOT_DIR/lab/check-runtime-port.sh" > "$OUT_DIR/port-check.txt" 2>&1; then
  :
else
  :
fi

if "$ROOT_DIR/lab/hello-runtime.sh" > "$OUT_DIR/hello.txt" 2>&1; then
  :
else
  :
fi

if "$ROOT_DIR/lab/status-runtime.sh" > "$OUT_DIR/status.txt" 2>&1; then
  :
else
  :
fi

if "$ROOT_DIR/lab/query-tx.sh" > "$OUT_DIR/query-tx.txt" 2>&1; then
  :
else
  :
fi

echo "$OUT_DIR"
