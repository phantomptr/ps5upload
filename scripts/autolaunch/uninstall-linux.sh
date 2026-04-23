#!/usr/bin/env bash
# uninstall-linux.sh — remove the ps5upload-engine systemd user service.
set -euo pipefail

SERVICE_NAME="ps5upload-engine"
UNIT_FILE="${HOME}/.config/systemd/user/${SERVICE_NAME}.service"

systemctl --user stop    "${SERVICE_NAME}" 2>/dev/null || true
systemctl --user disable "${SERVICE_NAME}" 2>/dev/null || true
rm -f "${UNIT_FILE}"
systemctl --user daemon-reload

echo "✓ ${SERVICE_NAME} removed"
