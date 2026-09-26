#!/usr/bin/env bash
set -euo pipefail

PS5_IP="${PS5_IP:-192.168.137.2}"
PS5_PORT="${PS5_PORT:-9021}"
PAYLOAD_PATH="${1:-payload2/ps5upload.elf}"

if [[ ! -f "$PAYLOAD_PATH" ]]; then
  echo "payload file not found: $PAYLOAD_PATH" >&2
  exit 1
fi

if ! command -v nc >/dev/null 2>&1; then
  echo "nc is required" >&2
  exit 1
fi

echo "sending $PAYLOAD_PATH to ${PS5_IP}:${PS5_PORT}"
nc -w 1 "$PS5_IP" "$PS5_PORT" < "$PAYLOAD_PATH"
echo "payload send complete"
