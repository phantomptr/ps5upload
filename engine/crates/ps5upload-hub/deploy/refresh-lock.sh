#!/bin/sh
# Rebuilds deploy/Cargo.lock (the Docker build's lock) from engine/Cargo.lock: the same versions,
# pruned to what the hub uses. Run from anywhere after the hub's dependencies change:
#   sh engine/crates/ps5upload-hub/deploy/refresh-lock.sh
set -eu
crate=$(cd "$(dirname "$0")/.." && pwd)
engine=$(cd "$crate/../.." && pwd)
tmp=$(mktemp -d)
mkdir "$tmp/ps5upload-hub"
cp "$crate/Cargo.toml" "$tmp/ps5upload-hub/"
cp -R "$crate/src" "$crate/tests" "$tmp/ps5upload-hub/"
cp "$crate/deploy/workspace.toml" "$tmp/Cargo.toml"
cp "$engine/Cargo.lock" "$tmp/Cargo.lock"
# Resolving drops the packages the hub does not use and keeps every version it does.
# Offline by default: the lock is only pruned, never updated, and nothing is fetched. A clean
# machine (CI) has not downloaded every crate the lock names for other targets, so it sets
# HUB_LOCK_ONLINE=1; the versions still come from the engine's lock.
offline=--offline
[ "${HUB_LOCK_ONLINE:-}" = 1 ] && offline=
(cd "$tmp" && cargo metadata --format-version 1 $offline >/dev/null)
cp "$tmp/Cargo.lock" "$crate/deploy/Cargo.lock"
echo "wrote $crate/deploy/Cargo.lock (scratch copy left in $tmp)"
