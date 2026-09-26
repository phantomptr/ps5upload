#!/usr/bin/env bash
set -euo pipefail

PS5_IP="${PS5_IP:-192.168.137.2}"
RUNTIME_PORT="${RUNTIME_PORT:-9113}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_TX_ID="lab-tx-$(date +%s)"
BODY="${1:-{\"tx_id\":\"$DEFAULT_TX_ID\",\"kind\":\"upload_tree\",\"source\":\"lab_stub\"}}"

python3 "$ROOT_DIR/lab/ftx2_control.py" begin-tx "$PS5_IP" "$RUNTIME_PORT" "$BODY"
