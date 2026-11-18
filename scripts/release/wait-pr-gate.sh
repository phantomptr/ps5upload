#!/usr/bin/env bash
#
# Wait for a pull request's merge gate, and only that.
#
#   scripts/release/wait-pr-gate.sh <pr-number> [--timeout-min N]
#
# The gate is the ci workflow's aggregate job "PR gate" (engine-ci.yml): it needs every
# correctness job, so its result is the merge verdict. CodeQL keeps running on the PR (and
# blocks a release through scripts/release.sh's open-alert check), but at 23-27 min it is
# the slowest check by far, and waiting for "all checks" meant waiting for it.
#
# Exits 0 when the gate passes, 1 when it fails or is cancelled (the failing jobs are
# printed), 2 on bad usage or when the timeout (default 60 min) runs out. A network or TLS
# error from gh is retried, never taken as a verdict.
#
# This only waits and reports. To wait AND merge, use scripts/release/merge-when-green.sh <pr>.
# Never chain this to `gh pr merge` yourself, and never through a pipe: in
# `wait-pr-gate.sh N | tail -3 && gh pr merge N --admin` the `&&` sees tail's exit status, and
# that merged a release PR whose gate had FAILED. The verdict is this script's own exit status.

set -uo pipefail

usage() { echo "usage: $0 <pr-number> [--timeout-min N]" >&2; exit 2; }

pr="${1:-}"
[[ "$pr" =~ ^[0-9]+$ ]] || usage
shift
timeout_min=60
while [[ $# -gt 0 ]]; do
  case "$1" in
    --timeout-min) timeout_min="${2:-}"; [[ "$timeout_min" =~ ^[0-9]+$ ]] || usage; shift 2 ;;
    *) usage ;;
  esac
done

gate="PR gate"
poll=20
deadline=$(( $(date +%s) + timeout_min * 60 ))
started=$(date +%s)
last=""

while :; do
  # gh exits non-zero while checks are pending (8) and on network errors alike, so the
  # verdict comes from the JSON, not the exit status.
  out="$(gh pr checks "$pr" --json name,bucket,state,workflow,link 2>&1)"
  if ! printf '%s' "$out" | jq -e 'type == "array"' >/dev/null 2>&1; then
    case "$out" in
      *"no checks reported"*) status="no checks yet" ;;
      *) status="gh error, retrying: $(printf '%s' "$out" | head -n 1 | cut -c1-120)" ;;
    esac
  else
    bucket="$(printf '%s' "$out" | jq -r --arg g "$gate" '[.[] | select(.name == $g)][0].bucket // "missing"')"
    case "$bucket" in
      pass)
        mins=$(( ($(date +%s) - started) / 60 ))
        echo "$gate passed on PR #$pr (waited ${mins} min)."
        pending="$(printf '%s' "$out" | jq -r '[.[] | select(.bucket == "pending") | .name] | join(", ")')"
        [[ -n "$pending" ]] && echo "still running (not part of the gate): $pending"
        exit 0
        ;;
      fail|cancel)
        echo "$gate did not pass on PR #$pr ($bucket). Failed or cancelled jobs:" >&2
        printf '%s' "$out" | jq -r '.[] | select(.bucket == "fail" or .bucket == "cancel")
          | "  [\(.workflow)] \(.name): \(.state)  \(.link)"' >&2
        echo "NOT passed: do not merge PR #$pr (exit 1)." >&2
        exit 1
        ;;
      missing) status="$gate not started yet" ;;
      *)
        done_n="$(printf '%s' "$out" | jq '[.[] | select(.bucket != "pending")] | length')"
        all_n="$(printf '%s' "$out" | jq 'length')"
        status="$gate $bucket ($done_n/$all_n checks finished)"
        ;;
    esac
  fi
  if [[ "$status" != "$last" ]]; then
    echo "$(date +%H:%M:%S) $status"
    last="$status"
  fi
  if (( $(date +%s) > deadline )); then
    echo "timed out after ${timeout_min} min waiting for $gate on PR #$pr" >&2
    exit 2
  fi
  sleep "$poll"
done
