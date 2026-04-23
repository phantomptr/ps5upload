#!/usr/bin/env bash
# install-linux.sh — install ps5upload-engine as a systemd user service
# that starts on login and restarts on failure.
#
# Usage:
#   ./scripts/autolaunch/install-linux.sh [--ps5-addr HOST:PORT]
#
# Requirements: systemd (user session), cargo already installed.
# The engine binary is built from engine/ via cargo.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
PS5_ADDR="${PS5_ADDR:-192.168.137.2:9113}"
ENGINE_PORT="${ENGINE_PORT:-9114}"
SERVICE_NAME="ps5upload-engine"
UNIT_DIR="${HOME}/.config/systemd/user"

for arg in "$@"; do
    case "$arg" in
        --ps5-addr=*) PS5_ADDR="${arg#--ps5-addr=}" ;;
        --engine-port=*) ENGINE_PORT="${arg#--engine-port=}" ;;
    esac
done

echo "==> Building engine release binary..."
cargo build --release -p ps5upload-engine --manifest-path "${REPO_DIR}/engine/Cargo.toml"
ENGINE_BIN="${REPO_DIR}/engine/target/release/ps5upload-engine"

echo "==> Installing systemd user unit..."
mkdir -p "${UNIT_DIR}"

cat > "${UNIT_DIR}/${SERVICE_NAME}.service" <<EOF
[Unit]
Description=ps5upload FTX2 engine
After=network.target

[Service]
Type=simple
ExecStart=${ENGINE_BIN}
Restart=on-failure
RestartSec=5s
Environment=PS5_ADDR=${PS5_ADDR}
Environment=ENGINE_PORT=${ENGINE_PORT}

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable  "${SERVICE_NAME}"
systemctl --user restart "${SERVICE_NAME}"

echo ""
echo "✓ ${SERVICE_NAME} installed and started"
echo "  dashboard: http://127.0.0.1:${ENGINE_PORT}"
echo ""
echo "Useful commands:"
echo "  systemctl --user status  ${SERVICE_NAME}"
echo "  systemctl --user stop    ${SERVICE_NAME}"
echo "  systemctl --user disable ${SERVICE_NAME}"
echo "  journalctl --user -u     ${SERVICE_NAME} -f"
