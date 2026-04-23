#!/bin/bash
# Direct-send + lab-CLI test loop — iterate on payload bugs without
# clicking through the UI for every send.
#
# Usage:
#   scripts/hw-test-loop.sh <PS5_IP>
#
# Flow:
#   1. Build the payload
#   2. `nc` the ELF to PS5_IP:9021 (the loader)
#   3. Poll :9114 until the mgmt listener comes up
#   4. Exercise every command via ps5upload-lab so we can see exactly
#      which feature breaks, with the payload's own error string
#      surfaced (not a generic "HTTP 502")
#
# On failure, the script prints which step failed. Toggle the PS5's
# network to get a clean slate when ports wedge.

set -eu

if [ $# -lt 1 ]; then
    echo "usage: $0 <PS5_IP>" >&2
    exit 2
fi
PS5_IP="$1"
LOADER_PORT=9021
MGMT_PORT=9114
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$SCRIPT_DIR"

say() { printf '\n=== %s ===\n' "$*"; }

# --- 1. build --------------------------------------------------------
say "1. Building payload"
LLVM_CONFIG=/opt/homebrew/opt/llvm@18/bin/llvm-config make -s payload >/dev/null 2>&1 && \
  echo "  built $(stat -f%z payload/ps5upload.elf 2>/dev/null || stat -c%s payload/ps5upload.elf) bytes"

# --- 2. check loader + send ------------------------------------------
say "2. Probing PS5"
if ! nc -zv -G 3 "$PS5_IP" "$LOADER_PORT" 2>&1 | grep -q "open"; then
  echo "  ERROR: PS5 loader :$LOADER_PORT not reachable — is the PS5 on?"
  exit 1
fi
echo "  loader :$LOADER_PORT open"
say "3. Sending payload via Python (matches Tauri send shape)"
python3 -c "
import socket, sys
with open('payload/ps5upload.elf','rb') as f: data = f.read()
s = socket.create_connection(('$PS5_IP', $LOADER_PORT), timeout=10)
s.sendall(data)
s.shutdown(socket.SHUT_WR)
s.close()
print(f'  sent {len(data)} bytes')
" 2>&1 || { echo "  SEND FAILED -- loader stuck, ask user to reset network"; exit 2; }

# --- 4. wait for mgmt port -------------------------------------------
say "4. Waiting for mgmt port :$MGMT_PORT"
for i in $(seq 1 20); do
  if nc -zv -G 2 "$PS5_IP" "$MGMT_PORT" 2>&1 | grep -q "open"; then
    echo "  mgmt :$MGMT_PORT up after $i attempt(s)"
    break
  fi
  sleep 1
  [ "$i" -eq 20 ] && { echo "  ERROR: payload never bound :$MGMT_PORT"; exit 3; }
done

# --- 5. exercise via lab CLI -----------------------------------------
say "5. status (basic health)"
cargo run -q -p ps5upload-lab -- "$PS5_IP:$MGMT_PORT" status 2>&1 | head -20

say "6. volumes (FS_LIST_VOLUMES)"
cargo run -q -p ps5upload-lab -- "$PS5_IP:$MGMT_PORT" volumes 2>&1 | head -20

say "7. apps (APP_LIST_REGISTERED via sqlite)"
cargo run -q -p ps5upload-lab -- "$PS5_IP:$MGMT_PORT" apps 2>&1 | head -20

say "DONE"
