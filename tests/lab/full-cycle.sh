#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAYLOAD_PATH="${1:-$ROOT_DIR/payload2/ps5upload.elf}"

echo "[full-cycle] smoke runtime"
"$ROOT_DIR/lab/smoke-runtime.sh" "$PAYLOAD_PATH"

echo "[full-cycle] exercise transaction stub"
"$ROOT_DIR/lab/exercise-tx-stub.sh"

echo "[full-cycle] verify replay across reload"
"$ROOT_DIR/lab/reload-and-verify-replay.sh" "$PAYLOAD_PATH"

echo "[full-cycle] verify takeover request closes runtime"
"$ROOT_DIR/lab/verify-takeover.sh"
