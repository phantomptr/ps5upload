#!/usr/bin/env bash
set -euo pipefail

PS5_IP="${PS5_IP:-192.168.137.2}"
RUNTIME_PORT="${RUNTIME_PORT:-9113}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required" >&2
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT_DIR/lab/ftx2_control.py" takeover "$PS5_IP" "$RUNTIME_PORT"
