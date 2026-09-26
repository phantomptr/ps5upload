#!/usr/bin/env bash
set -euo pipefail

PS5_IP="${PS5_IP:-192.168.137.2}"
RUNTIME_PORT="${RUNTIME_PORT:-9113}"

if ! command -v nc >/dev/null 2>&1; then
  echo "nc is required" >&2
  exit 1
fi

if nc -z -w 1 "$PS5_IP" "$RUNTIME_PORT"; then
  echo "runtime port open: ${PS5_IP}:${RUNTIME_PORT}"
else
  echo "runtime port closed: ${PS5_IP}:${RUNTIME_PORT}" >&2
  exit 1
fi
