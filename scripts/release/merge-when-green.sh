#!/usr/bin/env bash
#
# Wait for a pull request's merge gate and squash-merge it, ONLY when the gate passed.
#
#   scripts/release/merge-when-green.sh <pr-number> [--timeout-min N] [--no-rerun] [--no-merge]
#
# This is the way to merge a release PR. `gh pr merge --admin` does not wait for or look at any
# check, so the wait and the merge must be one command that cannot lose the verdict in between:
# a release PR was merged with a FAILED gate by `wait-pr-gate.sh N | tail -3 && gh pr merge ...`
# (the pipe made `tail`'s exit status the one `&&` saw). Here nothing sits between the two.
#
# What it does:
#   1. Waits for `PR gate` (scripts/release/wait-pr-gate.sh, run directly, no pipe).
#   2. Gate failed: asks scripts/release/gate-failures.mjs why. When every failed job is
#      something outside the change (no runner, apt mirrors, the artifact wait, a simulator
#      crash) or a Playwright flaky-only result, it re-runs the failed jobs ONCE
#      (`gh run rerun --failed`; "already running" is fine: ci-runner-retry.yml may have been
#      first) and waits for the gate again. A real failure, or a second failure, ends it.
#   3. Gate passed: `gh pr merge <n> --squash --admin --match-head-commit <the commit the gate
#      was read for>`, so a push that lands meanwhile is not merged untested.
#
# Exit status: 0 merged (or, with --no-merge, the gate passed); 1 the gate failed and the PR was
# NOT merged; 2 bad usage, a lookup that kept failing, or the wait timed out (NOT merged);
# 3 the gate passed but the merge command failed. The last line always says which.
#
# --no-rerun: never re-run anything. --no-merge: stop after the verdict (a dry run).
# Test hooks (merge-when-green.test.mjs): MERGE_WHEN_GREEN_WAIT replaces wait-pr-gate.sh,
# MERGE_WHEN_GREEN_POLL the seconds between polls.

set -uo pipefail

usage() {
  echo "usage: $0 <pr-number> [--timeout-min N] [--no-rerun] [--no-merge]" >&2
  exit 2
}

pr="${1:-}"
[[ "$pr" =~ ^[0-9]+$ ]] || usage
shift
timeout_min=60
rerun_allowed=1
do_merge=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --timeout-min) timeout_min="${2:-}"; [[ "$timeout_min" =~ ^[0-9]+$ ]] || usage; shift 2 ;;
    --no-rerun) rerun_allowed=0; shift ;;
    --no-merge) do_merge=0; shift ;;
    *) usage ;;
  esac
done

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
wait_gate="${MERGE_WHEN_GREEN_WAIT:-$here/wait-pr-gate.sh}"
poll="${MERGE_WHEN_GREEN_POLL:-20}"
workflow=engine-ci.yml

say() { echo "merge-when-green: $*"; }
not_merged() { # exit status, reason
  echo "merge-when-green: NOT MERGED: PR #$pr: $2" >&2
  exit "$1"
}

# gh with three tries: a network error is not an answer. Prints the output of the try that worked.
gh_retry() {
  local out try
  for try in 1 2 3; do
    if out="$(gh "$@")"; then
      printf '%s' "$out"
      return 0
    fi
    [[ "$try" == 3 ]] || sleep "$poll"
  done
  return 1
}

repo="$(gh_retry repo view --json nameWithOwner --jq .nameWithOwner)" || not_merged 2 "could not read the repository"
info="$(gh_retry pr view "$pr" --json state,headRefOid,isDraft)" || not_merged 2 "could not read the pull request"
state="$(jq -r .state <<<"$info")"
sha="$(jq -r .headRefOid <<<"$info")"
if [[ "$state" == MERGED ]]; then
  say "PR #$pr is already merged; nothing to do"
  exit 0
fi
[[ "$state" == OPEN ]] || not_merged 2 "it is $state"
[[ "$(jq -r .isDraft <<<"$info")" == false ]] || not_merged 2 "it is a draft"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || not_merged 2 "could not read its head commit"
say "PR #$pr at $sha"

# "<id> <attempt> <status>" of the newest ci run for this commit's pull request, or nothing.
latest_run() {
  local out
  out="$(gh_retry api "repos/$repo/actions/workflows/$workflow/runs?head_sha=$sha&event=pull_request&per_page=30")" || return 1
  jq -r '[.workflow_runs[]] | sort_by(.created_at) | last // empty | "\(.id) \(.run_attempt) \(.status)"' <<<"$out"
}

# Waits until run $1 is past attempt $2 (or going again) and has finished. 0 when it finished.
wait_for_new_attempt() {
  local id="$1" old_attempt="$2" deadline out attempt status seen=0
  deadline=$(( $(date +%s) + timeout_min * 60 ))
  while :; do
    if out="$(gh api "repos/$repo/actions/runs/$id" 2>/dev/null)"; then
      attempt="$(jq -r .run_attempt <<<"$out")"
      status="$(jq -r .status <<<"$out")"
      if [[ "$attempt" =~ ^[0-9]+$ ]] && { (( attempt > old_attempt )) || [[ "$status" != completed ]]; }; then
        seen=1
      fi
      if (( seen )) && [[ "$status" == completed ]]; then
        return 0
      fi
    fi
    if (( $(date +%s) > deadline )); then
      return 1
    fi
    # A re-run that never registers (the request was accepted but nothing started).
    if (( ! seen )) && (( $(date +%s) > deadline - timeout_min * 60 + 300 )); then
      return 1
    fi
    sleep "$poll"
  done
}

reruns=0
rounds=0
while :; do
  rounds=$((rounds + 1))
  # Directly, never through a pipe: its exit status IS the verdict.
  "$wait_gate" "$pr" --timeout-min "$timeout_min"
  rc=$?
  if (( rc == 0 )); then
    break
  fi
  if (( rc != 1 )); then
    not_merged 2 "the wait for the gate ended without a verdict (exit $rc)"
  fi
  if (( ! rerun_allowed )); then
    not_merged 1 "the gate failed (--no-rerun)"
  fi
  if (( rounds >= 4 )); then
    not_merged 1 "the gate failed again"
  fi
  run="$(latest_run)" || not_merged 2 "could not look up the ci run for $sha"
  [[ -n "$run" ]] || not_merged 1 "the gate failed and there is no ci run for $sha to look at"
  read -r run_id run_attempt run_status <<<"$run"
  if [[ "$run_status" != completed ]]; then
    # Someone (ci-runner-retry.yml, a person) is already running it again: that is not our re-run.
    say "ci run $run_id is running again (attempt $run_attempt); waiting for it"
    wait_for_new_attempt "$run_id" $((run_attempt - 1)) || not_merged 2 "ci run $run_id did not finish in time"
    continue
  fi
  verdict_json="$(node "$here/gate-failures.mjs" "$run_id" --attempt "$run_attempt" --repo "$repo")" \
    || not_merged 2 "could not read why ci run $run_id failed"
  verdict="$(jq -r .verdict <<<"$verdict_json")"
  say "ci run $run_id, attempt $run_attempt: $verdict"
  jq -r '.jobs[] | "  \(.kind)\t\(.name): \(.reason)"' <<<"$verdict_json"
  if [[ "$verdict" == none ]]; then
    # The run's newest attempt has no failed job, yet the gate check reads failed: the checks
    # have not caught up with a re-run. Read the gate again (bounded by the rounds above).
    say "no failed job in that attempt; reading the gate again"
    sleep "$poll"
    continue
  fi
  if [[ "$(jq -r .rerun <<<"$verdict_json")" != true ]]; then
    not_merged 1 "the gate failed on something a re-run would not fix (gh run view $run_id --log-failed)"
  fi
  if (( reruns >= 1 )); then
    not_merged 1 "the gate failed again after one re-run ($verdict); read it: gh run view $run_id --log-failed"
  fi
  reruns=$((reruns + 1))
  say "re-running the failed jobs of ci run $run_id once"
  if ! rerun_out="$(gh run rerun "$run_id" --repo "$repo" --failed 2>&1)"; then
    case "$rerun_out" in
      *"already running"*) say "it is already running again" ;;
      *) not_merged 1 "could not re-run ci run $run_id: $(head -n 1 <<<"$rerun_out")" ;;
    esac
  fi
  wait_for_new_attempt "$run_id" "$run_attempt" || not_merged 2 "the re-run of ci run $run_id did not finish in time"
done

if (( ! do_merge )); then
  say "PR #$pr: the gate passed for $sha (--no-merge: not merging)"
  exit 0
fi
# --match-head-commit: only the commit whose gate was just read.
if ! gh pr merge "$pr" --squash --admin --match-head-commit "$sha"; then
  echo "merge-when-green: NOT MERGED: PR #$pr: the gate passed but gh pr merge failed (see above)" >&2
  exit 3
fi
say "MERGED PR #$pr ($sha)"
