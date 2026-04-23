#!/usr/bin/env bash
set -euo pipefail
node "$(dirname "$0")/release-posts.js" "$@"
