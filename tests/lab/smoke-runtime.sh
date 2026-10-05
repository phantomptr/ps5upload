#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAYLOAD_PATH="${1:-$ROOT_DIR/../payload/ps5upload.elf}"

echo "[smoke] sending payload"
"$ROOT_DIR/lab/send-payload.sh" "$PAYLOAD_PATH"

echo "[smoke] waiting for runtime port"
for _ in $(seq 1 10); do
  if "$ROOT_DIR/lab/check-runtime-port.sh"; then
    echo "[smoke] runtime port ready"
    # Brief pause: takeover from a previous payload instance leaves
    # both old and new listeners bound for ~1s while the old one
    # drains. 2 s is plenty for the old listener to fully exit.
    sleep 2
    echo "[smoke] hello"
    "$ROOT_DIR/lab/hello-runtime.sh"
    echo "[smoke] querying status"
    "$ROOT_DIR/lab/status-runtime.sh"

    # Probe the ShellUI-RPC surface: sensor reads exercise the ptrace remote-call path; a
    # regression in pt_call shows up here as zero readings. `processes` exercises the sysctl
    # path and should return real names like "SceShellUI".
    echo "[smoke] hw-temps via ShellUI RPC"
    cargo run -q --manifest-path "$ROOT_DIR/../engine/Cargo.toml" -p ps5upload-lab -- \
      "${PS5_IP:-192.168.137.2}" hw-temps || true
    echo "[smoke] processes via sysctl"
    cargo run -q --manifest-path "$ROOT_DIR/../engine/Cargo.toml" -p ps5upload-lab -- \
      "${PS5_IP:-192.168.137.2}" processes | head -25 || true
    exit 0
  fi
  sleep 1
done

echo "[smoke] runtime port did not become ready in time" >&2
"$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
exit 1
