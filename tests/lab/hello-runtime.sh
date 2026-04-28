#!/usr/bin/env bash
set -euo pipefail

PS5_IP="${PS5_IP:-192.168.137.2}"
# HELLO + STATUS go to the mgmt port (9114), not the transfer port
# (9113). The smoke script and ad-hoc invocations both expect this.
# Override with RUNTIME_PORT=9113 to talk to the transfer port
# directly, but that path doesn't speak HELLO and will return
# `wrong_port`.
RUNTIME_PORT="${RUNTIME_PORT:-9114}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required" >&2
  exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT_DIR/lab/ftx2_control.py" hello "$PS5_IP" "$RUNTIME_PORT"
