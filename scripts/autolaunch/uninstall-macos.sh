#!/usr/bin/env bash
# uninstall-macos.sh — remove the ps5upload-engine launchd agent.
set -euo pipefail

LABEL="com.phantomptr.ps5upload-engine"
PLIST_FILE="${HOME}/Library/LaunchAgents/${LABEL}.plist"

launchctl unload "${PLIST_FILE}" 2>/dev/null || true
rm -f "${PLIST_FILE}"

echo "✓ ${LABEL} removed"
