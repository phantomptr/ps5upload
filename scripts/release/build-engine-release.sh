#!/usr/bin/env bash
#
# Release build of the engine sidecar and its RAR helper for one target, with the desktop
# app's dependencies compiling alongside. Run from the repo root:
#
#   RUST_TARGET=aarch64-apple-darwin scripts/release/build-engine-release.sh
#
# Two callers, on purpose the same script: publish.yml (the release) and release-cache.yml
# (which runs the identical build on main so the compile cache holds what a release reads).
# sccache keys an object by the exact compiler command line and environment, so the two
# must not drift apart: change the build here, never in one workflow.
#
# --locked: the lockfile is the release's bill of materials; a Cargo.toml that outgrew it
# (two green PRs squash-merged, see CI's clippy step) must fail here, not quietly resolve to
# something newer.
#
# The desktop app's dependencies compile in the background: the engine build ends in minutes
# of one-crate, one-core work (ps5upload-engine, ps5upload-ava1) while the Tauri build later
# spends 3-5 min compiling tauri, wry and the rest before the app crate. Same command line
# Tauri runs (--bins --features tauri/custom-protocol --release --target), and on macOS the
# MACOSX_DEPLOYMENT_TARGET Tauri sets from bundle.macOS.minimumSystemVersion, so the Tauri
# build finds them fresh (checked on macOS with cargo -v: no dependency dirty). It cannot
# build the app itself: the app's build script needs the engine binary, and even once that
# exists the app's generate_context! needs client/dist, which only the Tauri build's
# beforeBuildCommand creates, so the app crate fails at once and --keep-going finishes the
# rest; the Tauri build compiles the app as before. Nothing here reaches the engine build's
# environment, and what the pre-build leaves is reused only where cargo's fingerprints match,
# so no binary changes. It runs at lower priority so the engine build keeps the CPU.
#
#   RUST_TARGET=... scripts/release/build-engine-release.sh --start-desktop-deps
#
# starts only that background build and returns. publish.yml runs it as soon as the toolchains
# are in, before npm ci and the wait for the payload ELF: none of the desktop app's dependencies
# reads the payload (only the app crate's build script does, and it fails there regardless), so
# the 2-4 minutes the runner otherwise idles go to compiling them. The plain run below then finds
# it started (or finished) and does not start a second one; when it was never started, or the
# runner did not keep it alive between steps, it starts it as before. Cargo's lock on the target
# directory keeps two builds from ever sharing it, so the wait is about time, not correctness.

set -euo pipefail

: "${RUST_TARGET:?set RUST_TARGET to the rust target triple}"
cd "$(dirname "$0")/../.."
state="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/desktop-deps"
log="$state.log"

start_desktop_deps() {
  rm -f "$state.done"
  (
    cd client/src-tauri
    if [ "$(uname -s)" = "Darwin" ]; then
      MACOSX_DEPLOYMENT_TARGET="$(node -p "require('./tauri.conf.json').bundle.macOS.minimumSystemVersion")"
      export MACOSX_DEPLOYMENT_TARGET
    fi
    nice="$(command -v nice || true)"
    rc=0
    ${nice:+$nice -n 10} cargo build --locked --release --target "$RUST_TARGET" \
      --bins --features tauri/custom-protocol --keep-going || rc=$?
    echo "$rc" > "$state.done"
  ) < /dev/null >> "$log" 2>&1 &
  echo "$!" > "$state.pid"
  echo "desktop dependencies: pre-build started (log: $log)"
}

deps_running() {
  [ -f "$state.pid" ] && kill -0 "$(cat "$state.pid")" 2>/dev/null
}

if [ "${1:-}" = "--start-desktop-deps" ]; then
  start_desktop_deps
  exit 0
fi

# A pre-build killed by a signal (status 128+) did not finish: start it again.
if { [ -f "$state.done" ] && [ "$(cat "$state.done")" -lt 128 ]; } || deps_running; then
  echo "desktop dependencies: pre-build already started"
else
  start_desktop_deps
fi

# ps5upload-unrar: the RAR helper. UnRAR is never linked into the engine (its license is
# GPL-incompatible, LICENSES/UnRAR-exception.md); the app embeds the helper next to the
# engine and the engine starts it over pipes.
(cd engine && cargo build --locked --release -p ps5upload-engine -p ps5upload-unrar --target "$RUST_TARGET")

# Expected to exit non-zero (the app's build script, see above), so its status is not read.
while [ ! -f "$state.done" ] && deps_running; do
  sleep 5
done
# "Compiling" without a trailing space: with CARGO_TERM_COLOR=always a colour code follows it.
echo "desktop dependencies pre-built: $(grep -c 'Compiling' "$log" || true) crates"
