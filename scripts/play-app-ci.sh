#!/usr/bin/env bash
# PS5Upload Play (apps/play) in CI: engine-ci.yml's play-app job and publish.yml's build-payload.
#
#   scripts/play-app-ci.sh present                 present=true|false (for $GITHUB_OUTPUT): is the
#                                                  app in this tree (apps/play/Makefile)?
#   scripts/play-app-ci.sh images <file>           write the Docker images there now (one per line)
#   scripts/play-app-ci.sh save <before> <tar>     save the images the build added since <before>
#                                                  (its toolchain image) to <tar> for the cache;
#                                                  nothing is written when it added none
#   scripts/play-app-ci.sh build <out-dir>         make -C apps/play release, check the zip against
#                                                  VERSION, copy it to <out-dir> under its release
#                                                  name PS5Upload-Play-FAKE99261-<VERSION>.zip
#
# The app's Makefile builds in Docker and owns its image names; this script only notices which
# images appeared, so the cache needs no list that could drift from the app's build.
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-}" in
  present)
    if [ -f apps/play/Makefile ]; then echo "present=true"; else echo "present=false"; fi
    ;;
  images)
    [ -n "${2:-}" ] || { echo "usage: $0 images <file>" >&2; exit 2; }
    docker image ls --format '{{.Repository}}:{{.Tag}}' | grep -v '<none>' | sort -u > "$2"
    ;;
  save)
    [ -n "${2:-}" ] && [ -n "${3:-}" ] || { echo "usage: $0 save <before> <tar>" >&2; exit 2; }
    after="$(mktemp)"
    trap 'rm -f "$after"' EXIT
    docker image ls --format '{{.Repository}}:{{.Tag}}' | grep -v '<none>' | sort -u > "$after"
    new="$(comm -13 "$2" "$after" || true)"
    if [ -z "$new" ]; then
      echo "the build added no Docker image; nothing to cache"
      exit 0
    fi
    echo "caching: $new"
    # shellcheck disable=SC2086 # one image name per word
    docker save -o "$3" $new
    ;;
  build)
    [ -n "${2:-}" ] || { echo "usage: $0 build <out-dir>" >&2; exit 2; }
    out="$2"
    version="$(tr -d '[:space:]' < VERSION)"
    make -C apps/play release
    node scripts/check-play-version.mjs --dist
    mkdir -p "$out"
    zip="$(ls apps/play/dist/PS5Upload-Play-FAKE99261-*.zip)"
    cp -v "$zip" "$out/PS5Upload-Play-FAKE99261-${version}.zip"
    ;;
  *)
    echo "usage: $0 present | images <file> | save <before> <tar> | build <out-dir>" >&2
    exit 2
    ;;
esac
