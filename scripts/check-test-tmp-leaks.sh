#!/usr/bin/env bash
# Runs tests with the temp folder (TMPDIR, TEMP, TMP) pointed at a new empty folder and fails if
# they leave anything in it. Tests that made scratch folders and never removed them once filled a
# developer's temp folder with 32,911 leftovers (38 GB), and earlier ~2 TB.
#
#   scripts/check-test-tmp-leaks.sh                 engine crates that make the most scratch files
#   scripts/check-test-tmp-leaks.sh --all           the whole engine workspace, the scripts' node
#                                                   tests and the client's vitest suite
#   scripts/check-test-tmp-leaks.sh -- CMD ARGS...  any command
#   scripts/check-test-tmp-leaks.sh --check DIR     only list what is left in DIR (CI runs its tests
#                                                   with TMPDIR set and then this)
#
# A test's scratch space: Rust tests wrap the path in `test_tmp::temp_path`/`remove_at_exit`
# (engine/test-support/test_tmp.rs); node and vitest tests remove theirs in `after`/`afterAll`;
# shell scripts use an EXIT trap. PS5UPLOAD_KEEP_TEST_TMP=1 keeps them to inspect a failed run
# (this check then fails, as it should).
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2

# Left on purpose and reused, not per run: Node's compile cache (vite enables it).
ALLOWED=(node-compile-cache)

check_dir() {
  local dir="$1" args=() left n
  for a in "${ALLOWED[@]}"; do args+=(! -name "$a"); done
  left="$(find "$dir" -mindepth 1 -maxdepth 1 "${args[@]}" | sort)"
  if [[ -z "$left" ]]; then
    echo "check-test-tmp-leaks: nothing left in the temp folder"
    return 0
  fi
  n="$(printf '%s\n' "$left" | wc -l | tr -d ' ')"
  echo "::error::tests left $n file(s) or folder(s) in the temp folder ($dir):"
  printf '%s\n' "$left" | head -n 200 | while IFS= read -r p; do
    printf '  %s  %s\n' "$(du -sh "$p" 2>/dev/null | cut -f1)" "${p#"$dir"/}"
  done
  ((n > 200)) && echo "  ... and $((n - 200)) more"
  echo "Make each test remove what it creates, pass or fail (see the top of this script)."
  return 1
}

if [[ "${1:-}" == "--check" ]]; then
  [[ -d "${2:-}" ]] || { echo "usage: $0 --check DIR" >&2; exit 2; }
  check_dir "$2"
  exit
fi

mode=engine
cmd=()
case "${1:-}" in
  "") ;;
  --all) mode=all ;;
  --) shift; mode=cmd; cmd=("$@") ;;
  *) echo "usage: $0 [--all | -- CMD ARGS... | --check DIR]" >&2; exit 2 ;;
esac

tmp="$(mktemp -d "${TMPDIR:-/tmp}/ps5upload-tmp-leaks.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
scratch="$tmp/tmp"
mkdir "$scratch"

# Runs a command with the temp folder redirected; a failing test fails the check too.
in_scratch() {
  TMPDIR="$scratch/" TEMP="$scratch" TMP="$scratch" "$@"
}

nextest() {
  if cargo nextest --version >/dev/null 2>&1; then
    # Build first, outside the scratch folder: only what the tests leave counts.
    (cd engine && cargo build -q -p ps5upload-unrar && cargo nextest run --no-run "$@") || return 1
    (cd engine && in_scratch cargo nextest run --no-fail-fast "$@")
  else
    (cd engine && cargo build -q -p ps5upload-unrar && cargo test --no-run "$@") || return 1
    (cd engine && in_scratch cargo test --no-fail-fast "$@")
  fi
}

status=0
case "$mode" in
  engine)
    nextest -p ps5upload-core -p ps5upload-ava1 -p ps5upload-fpkg -p ps5upload-convert-core || status=1
    ;;
  all)
    nextest --workspace || status=1
    in_scratch bash -c 'node --test scripts/*.test.mjs scripts/release/*.test.mjs' || status=1
    if [[ -d client/node_modules ]]; then
      (cd client && in_scratch npx vitest run) || status=1
    else
      echo "check-test-tmp-leaks: client/node_modules missing (npm ci in client/): vitest skipped"
    fi
    ;;
  cmd)
    in_scratch "${cmd[@]}" || status=1
    ;;
esac

((status)) && echo "check-test-tmp-leaks: some tests failed (above)"
check_dir "$scratch" || status=1
exit "$status"
