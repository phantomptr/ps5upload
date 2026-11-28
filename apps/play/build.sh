#!/usr/bin/env bash
# PS5Upload Play - build on macOS or Linux through Docker (make -C apps/play calls this).
#   apps/play/build.sh            -> apps/play/dist/FAKE99261/
#   apps/play/build.sh release    -> also apps/play/dist/PS5Upload-Play-FAKE99261-<version>.zip
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../.." && pwd)
image=ps5upload-play-build:1
if [[ ${PLAY_NO_DOCKER:-} == 1 ]]; then
    exec bash "$here/tools/build-in-container.sh" "$@"
fi
docker image inspect "$image" >/dev/null 2>&1 || docker build -t "$image" "$here/docker"
# The repo is mounted so the build can read branding/ and LICENSE; it writes only under apps/play.
docker run --rm -u "$(id -u):$(id -g)" -e HOME=/tmp -v "$repo:/repo" -w /repo/apps/play "$image" \
    bash tools/build-in-container.sh "$@"
