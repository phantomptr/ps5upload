#!/usr/bin/env bash
#
# Security scan: known-vulnerable dependencies and committed secrets.
# Run locally with `make security`; CI runs it in the security-scan job of .github/workflows/engine-ci.yml.
#
#   cargo audit   engine/ and client/src-tauri/ lockfiles against the RustSec database
#   npm audit     client/ and the root package (high and critical only)
#   gitleaks      the whole git history for keys, tokens and passwords
#
# Each check that is missing its tool is reported and skipped locally, but fails in CI
# (CI=true), so the workflow can never pass by not running a check.
#
# Accepted findings live here, each with its reason, so a new one fails the scan:
# Gitleaks false positives (AES test vectors, a test archive password, the old Wake
# example key) are listed by fingerprint in .gitleaksignore.

set -uo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root" || exit 1

failed=()
skipped=()

have() { command -v "$1" >/dev/null 2>&1; }

missing() {
  if [ "${CI:-}" = "true" ]; then
    echo "::error::$1 is not installed"
    failed+=("$1 (not installed)")
  else
    echo "skip: $1 is not installed ($2)"
    skipped+=("$1")
  fi
}

section() { printf '\n== %s\n' "$1"; }

section "cargo audit"
if cargo audit --version >/dev/null 2>&1; then
  for dir in engine client/src-tauri; do
    echo "-- $dir"
    # Warnings (unmaintained, unsound, yanked) are printed but do not fail: they are
    # mostly Linux GTK crates under Tauri that we cannot change. Vulnerabilities fail.
    if ! (cd "$dir" && cargo audit); then
      failed+=("cargo audit ($dir)")
    fi
  done
else
  missing cargo-audit "cargo install cargo-audit --locked"
fi

section "npm audit"
if have npm; then
  for dir in client .; do
    echo "-- $dir"
    # What the app ships: no allowances at all.
    if ! (cd "$dir" && npm audit --audit-level=high --omit=optional --omit=dev >/dev/null); then
      (cd "$dir" && npm audit --audit-level=high --omit=optional --omit=dev) || true
      failed+=("npm audit, shipped dependencies ($dir)")
    fi
    # Everything, development tools included: an advisory with no fix yet can be allowed for a
    # while in scripts/npm-audit-allow.json (with a reason and an expiry).
    # npm exits non-zero whenever it finds anything (allowed or not); the filter decides.
    if ! { (cd "$dir" && npm audit --omit=optional --json 2>/dev/null) || true; } | node scripts/npm-audit-filter.mjs; then
      failed+=("npm audit ($dir)")
    fi
  done
else
  missing npm "install Node.js"
fi

section "workflow actions pinned to a commit"
# A tag (`@v4`) can be moved to other code after the fact; a 40-character commit cannot. Every
# `uses:` in a workflow names a commit (Dependabot keeps them current).
unpinned="$(grep -rnE '^[[:space:]]*-?[[:space:]]*uses:[[:space:]]*[^[:space:]#]+@' .github/workflows \
  | grep -vE 'uses:[[:space:]]*[^[:space:]@]+@[0-9a-f]{40}([[:space:]]|$)' \
  | grep -vE 'uses:[[:space:]]*\./' || true)"
if [ -n "$unpinned" ]; then
  echo "$unpinned"
  echo "::error::workflow actions not pinned to a commit (see above)"
  failed+=("unpinned workflow actions")
else
  echo "every workflow action is pinned"
fi

section "gitleaks (full history)"
if have gitleaks; then
  if ! gitleaks git --redact --no-banner .; then
    failed+=("gitleaks")
  fi
else
  missing gitleaks "brew install gitleaks"
fi

echo
if [ "${#skipped[@]}" -gt 0 ]; then
  echo "skipped: ${skipped[*]}"
fi
if [ "${#failed[@]}" -gt 0 ]; then
  echo "security scan FAILED: ${failed[*]}"
  exit 1
fi
echo "security scan passed"
