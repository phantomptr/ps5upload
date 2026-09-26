#!/usr/bin/env bash
set -euo pipefail

PS5_IP="${PS5_IP:-192.168.137.2}"
RUNTIME_PORT="${RUNTIME_PORT:-9113}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BODY="${1:-{\"tx_id\":\"lab-default-tx\"}}"

python3 "$ROOT_DIR/lab/ftx2_control.py" query-tx "$PS5_IP" "$RUNTIME_PORT" "$BODY"
