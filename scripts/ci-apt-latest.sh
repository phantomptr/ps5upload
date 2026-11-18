#!/usr/bin/env bash
# Runs the default branch's scripts/ci-apt.sh instead of this checkout's. For release builds.
#
# Usage: scripts/ci-apt-latest.sh [apt-get install options] <package>...   (as scripts/ci-apt.sh)
#
# Why: a release is built from its tag, and a tag keeps its own copy of every script for ever
# (the tag ruleset forbids moving it). ci-apt.sh is the one script whose failures come from
# outside the repository (which Ubuntu mirror a runner image reads, and how that mirror is doing
# today), so its fixes land on main after the tag that needed them: v12.6.6 was tagged, failed
# five publish attempts on one slow mirror, and could not be published at all. With this, a fix
# on main reaches every tag's next attempt: `gh run rerun <publish run> --failed`.
#
# What it trusts: this same repository (the checkout's own `origin`, with the checkout's own
# token) at its default branch and nothing else. main only changes through an admin merge, and
# the workflow calling this was itself cut from main, so this reads no code that could not
# already have been in the tag. Only ci-apt.sh is taken, written outside the workspace.
#
# Falls back to this checkout's ci-apt.sh when the default branch's cannot be fetched or does
# not parse, and always on a pull request or merge queue (there the change under test must be
# the one that runs).

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
own="$here/ci-apt.sh"
branch="${CI_APT_LATEST_BRANCH:-main}"

case "${GITHUB_EVENT_NAME:-}" in
  pull_request|pull_request_target|merge_group)
    exec bash "$own" "$@"
    ;;
esac

tmp="$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/ci-apt-latest.XXXXXX")"
got=0
if git -C "$here" fetch --quiet --no-tags --no-recurse-submodules --depth=1 origin "refs/heads/$branch" 2>/dev/null \
    && git -C "$here" show "FETCH_HEAD:scripts/ci-apt.sh" > "$tmp" 2>/dev/null \
    && [ -s "$tmp" ] && bash -n "$tmp" 2>/dev/null; then
  got=1
fi
if [ "$got" -ne 1 ]; then
  echo "ci-apt-latest: could not read scripts/ci-apt.sh from $branch; using this checkout's copy" >&2
  rm -f "$tmp"
  exec bash "$own" "$@"
fi
if cmp -s "$tmp" "$own"; then
  echo "ci-apt-latest: this checkout's ci-apt.sh is $branch's" >&2
else
  echo "ci-apt-latest: using $branch's ci-apt.sh ($(git -C "$here" rev-parse --short FETCH_HEAD)), which differs from this checkout's" >&2
fi
exec bash "$tmp" "$@"
