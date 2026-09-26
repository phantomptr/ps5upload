#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TX_ID="exercise-tx-$(date +%s)"
BODY="${1:-{\"tx_id\":\"$TX_ID\",\"kind\":\"upload_tree\",\"source\":\"exercise_tx_stub\"}}"
QUERY_BODY="{\"tx_id\":\"$TX_ID\"}"

echo "[tx] hello"
"$ROOT_DIR/lab/hello-runtime.sh"

echo "[tx] status before"
"$ROOT_DIR/lab/status-runtime.sh"

echo "[tx] begin"
"$ROOT_DIR/lab/begin-tx.sh" "$BODY"

echo "[tx] query after begin"
"$ROOT_DIR/lab/query-tx.sh" "$QUERY_BODY"

echo "[tx] abort"
"$ROOT_DIR/lab/abort-tx.sh" "$QUERY_BODY"

echo "[tx] query after abort"
"$ROOT_DIR/lab/query-tx.sh" "$QUERY_BODY"
