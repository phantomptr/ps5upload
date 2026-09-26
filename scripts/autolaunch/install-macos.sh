#!/usr/bin/env bash
# install-macos.sh — install ps5upload-engine as a launchd user agent
# that starts on login and restarts on failure.
#
# Usage:
#   ./scripts/autolaunch/install-macos.sh [--ps5-addr HOST:PORT]
#
# Requirements: macOS with launchd, cargo already installed.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
PS5_ADDR="${PS5_ADDR:-192.168.137.2:9113}"
# Engine listens on 19113 by default; matches the hard-coded URL the
# desktop client probes when it spawns the engine as a sidecar, and
# the env var name the engine reads (`PS5UPLOAD_ENGINE_PORT`).
PS5UPLOAD_ENGINE_PORT="${PS5UPLOAD_ENGINE_PORT:-19113}"
LABEL="com.phantomptr.ps5upload-engine"
PLIST_DIR="${HOME}/Library/LaunchAgents"
PLIST_FILE="${PLIST_DIR}/${LABEL}.plist"
LOG_DIR="${HOME}/Library/Logs/ps5upload-engine"

for arg in "$@"; do
    case "$arg" in
        --ps5-addr=*) PS5_ADDR="${arg#--ps5-addr=}" ;;
        --engine-port=*) PS5UPLOAD_ENGINE_PORT="${arg#--engine-port=}" ;;
    esac
done

echo "==> Building engine release binary..."
cargo build --release -p ps5upload-engine --manifest-path "${REPO_DIR}/engine/Cargo.toml"
ENGINE_BIN="${REPO_DIR}/engine/target/release/ps5upload-engine"

echo "==> Installing launchd plist..."
mkdir -p "${PLIST_DIR}" "${LOG_DIR}"

cat > "${PLIST_FILE}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${ENGINE_BIN}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PS5_ADDR</key>
        <string>${PS5_ADDR}</string>
        <key>PS5UPLOAD_ENGINE_PORT</key>
        <string>${PS5UPLOAD_ENGINE_PORT}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${LOG_DIR}/stdout.log</string>
    <key>StandardErrorPath</key>
    <string>${LOG_DIR}/stderr.log</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
EOF

launchctl unload "${PLIST_FILE}" 2>/dev/null || true
launchctl load   "${PLIST_FILE}"

echo ""
echo "✓ ${LABEL} installed and started"
echo "  dashboard: http://127.0.0.1:${PS5UPLOAD_ENGINE_PORT}"
echo "  logs:      ${LOG_DIR}/"
echo ""
echo "Useful commands:"
echo "  launchctl list ${LABEL}"
echo "  launchctl unload ${PLIST_FILE}  # stop + disable"
echo "  tail -f ${LOG_DIR}/stdout.log"
