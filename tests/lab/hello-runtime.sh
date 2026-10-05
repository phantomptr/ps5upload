#!/usr/bin/env bash
set -euo pipefail

# node.info over AVA1 (the helper's only port is 9120; a host:port address is accepted and the
# port ignored).
PS5_IP="${PS5_IP:-192.168.137.2}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo run -q --manifest-path "$ROOT_DIR/../engine/Cargo.toml" -p ps5upload-lab -- "$PS5_IP" hello
