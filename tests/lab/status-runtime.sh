#!/usr/bin/env bash
set -euo pipefail

PS5_IP="${PS5_IP:-192.168.137.2}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo run -q --manifest-path "$ROOT_DIR/../engine/Cargo.toml" -p ps5upload-lab -- "$PS5_IP" status
