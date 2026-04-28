#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAYLOAD_PATH="${1:-$ROOT_DIR/payload2/ps5upload.elf}"

echo "[smoke] sending payload"
"$ROOT_DIR/lab/send-payload.sh" "$PAYLOAD_PATH"

echo "[smoke] waiting for runtime port"
for _ in $(seq 1 10); do
  if "$ROOT_DIR/lab/check-runtime-port.sh"; then
    echo "[smoke] runtime port ready"
    # Brief pause: takeover from a previous payload instance leaves
    # both old and new mgmt-port listeners bound for ~1s while the
    # old one drains. The kernel may route a freshly-issued STATUS
    # to the still-shutting-down instance, which RSTs the
    # connection. 2 s is plenty for the old listener to fully exit.
    sleep 2
    echo "[smoke] hello"
    "$ROOT_DIR/lab/hello-runtime.sh"
    echo "[smoke] querying status"
    "$ROOT_DIR/lab/status-runtime.sh"

    # Probe the ShellUI-RPC surface — sensor reads exercise the
    # ptrace remote-call path; a regression in pt_call shows up
    # here as zero readings. PROC_LIST exercises the sysctl path
    # and should return real names like "SceShellUI".
    if command -v python3 >/dev/null 2>&1 && [[ -f "$ROOT_DIR/lab/ftx2_probe.py" ]]; then
      echo "[smoke] HW_TEMPS via ShellUI RPC"
      python3 "$ROOT_DIR/lab/ftx2_probe.py" "${PS5_IP:-192.168.137.2}" \
        "${PS5_MGMT_PORT:-9114}" 66 || true
      echo "[smoke] PROC_LIST via sysctl"
      python3 "$ROOT_DIR/lab/ftx2_probe.py" "${PS5_IP:-192.168.137.2}" \
        "${PS5_MGMT_PORT:-9114}" 74 | head -25 || true
    fi
    exit 0
  fi
  sleep 1
done

echo "[smoke] runtime port did not become ready in time" >&2
"$ROOT_DIR/lab/capture-runtime-trace.sh" >&2 || true
exit 1
