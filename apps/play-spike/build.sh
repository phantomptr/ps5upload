#!/usr/bin/env bash
# PS5Upload Play spike - build on macOS or Linux through Docker.
# Output: apps/play-spike/dist/FAKE99271/ (the app folder) plus reference hashes.
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../.." && pwd)
image=ps5upload-play-spike-build:1
docker image inspect "$image" >/dev/null 2>&1 || docker build -t "$image" "$here/docker"
# The repo is mounted so the build can read branding/; it writes only under apps/play-spike.
docker run --rm -u "$(id -u):$(id -g)" -v "$repo:/repo" -w /repo/apps/play-spike "$image" \
    bash tools/build-in-container.sh
