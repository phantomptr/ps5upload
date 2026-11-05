#!/usr/bin/env bash
#
# The local pre-release check (`make release-check`): what CI's PR gate covers, run on this
# machine before a release, one part after another.
#
#   1. make quality            (fmt, clippy, tests, client build; CPU-heavy)
#   2. Playwright e2e suite    (own ports; serves the build from step 1 for its CSP tests)
#   3. make test-payload       (builds the payload with the PS5 SDK and checks the ELFs)
#
# The e2e suite follows the build rather than running beside it: vite build empties client/dist,
# which the suite's CSP server serves, and its tests have real timing that a loaded machine fails.
#
# Fails if any part fails, and prints each part's result and time. Logs go to a temp folder
# (printed); a failed part's log tail is shown.
#
# BUILD_LOCK=/path/to/lockfile serializes every part with other builds sharing that lock (lockf on
# macOS, flock on Linux).
#
# The e2e suite gets its own vite on a free port (PS5UPLOAD_VITE_PORT, client/playwright.config.ts),
# so a dev server already on :1420 (`make run-client`) neither blocks it nor is reused by it.

set -uo pipefail

cd "$(dirname "$0")/../.." || exit 2
logs="$(mktemp -d "${TMPDIR:-/tmp}/release-check.XXXXXX")"

locked() {
  if [[ -z "${BUILD_LOCK:-}" ]]; then "$@"
  elif command -v lockf >/dev/null; then lockf -k "$BUILD_LOCK" "$@"
  elif command -v flock >/dev/null; then flock "$BUILD_LOCK" "$@"
  else echo "release-check: BUILD_LOCK set but neither lockf nor flock exists" >&2; return 2
  fi
}

# A port with the next two up also free: vite, its live reload, and the production build's server.
free_port() {
  node -e '
    const net = require("net");
    const take = (p) => new Promise((ok) => { const s = net.createServer();
      s.once("error", () => ok(false)); s.listen(p, "127.0.0.1", () => s.close(() => ok(true))); });
    (async () => { for (let p = 14200; p < 14400; p += 3)
      if ((await take(p)) && (await take(p + 1)) && (await take(p + 2))) { console.log(p); return; }
      process.exit(1); })();'
}

e2e() {
  local port
  port="$(free_port)" || { echo "no free port for the e2e dev server"; return 1; }
  echo "e2e: vite on 127.0.0.1:$port"
  # make quality has built it, unless its client lane stopped first (then this run has failed anyway).
  [[ -f client/dist/index.html ]] || (cd client && locked npm run build:vite) || return 1
  (cd client && PS5UPLOAD_VITE_PORT="$port" locked npx playwright test)
}

declare -a names=() codes=() secs=()
record() { names+=("$1"); codes+=("$2"); secs+=("$3"); }

run_part() { # log, command...
  local log="$1" start rc
  shift
  start=$(date +%s)
  "$@" > "$log" 2>&1
  rc=$?
  echo "$rc $(( $(date +%s) - start ))" > "$log.result"
  return "$rc"
}

echo "release-check: logs in $logs"
run_and_record() { # name, command...
  local name="$1" rc s
  shift
  echo "release-check: $name..."
  run_part "$logs/$name.log" "$@"
  read -r rc s < "$logs/$name.log.result" || { rc=1; s=0; }
  record "$name" "$rc" "$s"
}
run_and_record quality locked make quality
run_and_record e2e e2e
run_and_record test-payload locked make test-payload

failed=0
echo
echo "release-check summary"
for i in "${!names[@]}"; do
  if [[ "${codes[$i]}" == 0 ]]; then status=ok; else status="FAILED (exit ${codes[$i]})"; failed=1; fi
  printf '  %-13s %-18s %4ds  %s\n' "${names[$i]}" "$status" "${secs[$i]}" "$logs/${names[$i]}.log"
done
if (( failed )); then
  for i in "${!names[@]}"; do
    [[ "${codes[$i]}" == 0 ]] && continue
    echo
    echo "---- ${names[$i]}: last 40 lines ----"
    tail -n 40 "$logs/${names[$i]}.log"
  done
  exit 1
fi
echo "release-check: all passed"
