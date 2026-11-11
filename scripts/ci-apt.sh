#!/usr/bin/env bash
# Installs apt packages on a CI runner without letting a stalled mirror take the job with it.
# Every apt call in .github/workflows goes through here.
#
# Usage: scripts/ci-apt.sh [apt-get install options] <package>...
#   e.g. scripts/ci-apt.sh --no-install-recommends samba attr
#
# apt has no default network timeout, and GitHub's runners read Ubuntu's archive through
# azure.archive.ubuntu.com, which goes quiet now and then: each package then waits out its own
# timeout, and one `apt-get install` of a long list outlived a 12-minute step on its first try.
#
# So the network half runs first and on a clock: `apt-get update` and `apt-get install
# --download-only` together get CI_APT_ATTEMPT_TIMEOUT seconds (240), three times at most. Killing
# a download is safe (nothing is unpacked yet; apt resumes the partial files). After a failed try
# the Azure mirror is taken out of the runner's mirror list, so the next one reads
# archive.ubuntu.com. Only then does the real install run, from the downloaded files
# (`--no-download`) and with no clock: dpkg must never be killed halfway.
#
# A step that calls this needs a timeout above 3 x 250 s plus the install itself: 20 minutes.

set -euo pipefail

ATTEMPTS=3
ATTEMPT_TIMEOUT="${CI_APT_ATTEMPT_TIMEOUT:-240}"
MIRRORS_FILE="${CI_APT_MIRRORS_FILE:-/etc/apt/apt-mirrors.txt}"
NET_OPTS=(-o Acquire::Retries=1 -o Acquire::http::Timeout=10 -o Acquire::https::Timeout=10)

as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  else
    sudo "$@"
  fi
}

# The clocked half, run as root under `timeout` by the loop below (which re-runs this script, so
# one clock covers both commands and `timeout` signals the whole process group).
if [ "${1:-}" = "--fetch" ]; then
  shift
  export DEBIAN_FRONTEND=noninteractive
  apt-get "${NET_OPTS[@]}" update
  apt-get "${NET_OPTS[@]}" install -y --download-only "$@"
  exit 0
fi

if [ "$#" -eq 0 ]; then
  echo "usage: $0 [apt-get install options] <package>..." >&2
  exit 2
fi
case "$ATTEMPT_TIMEOUT" in
  ""|*[!0-9]*) echo "ci-apt: CI_APT_ATTEMPT_TIMEOUT must be a number of seconds" >&2; exit 2 ;;
esac

# Takes the Azure mirror out of the runner's list, unless it is the only one there.
drop_azure_mirror() {
  [ -f "$MIRRORS_FILE" ] || return 0
  grep -q 'azure\.archive\.ubuntu\.com' "$MIRRORS_FILE" || return 0
  if ! grep -v 'azure\.archive\.ubuntu\.com' "$MIRRORS_FILE" | grep -q '[^[:space:]]'; then
    echo "ci-apt: $MIRRORS_FILE lists no other mirror; leaving it alone" >&2
    return 0
  fi
  echo "ci-apt: taking azure.archive.ubuntu.com out of $MIRRORS_FILE" >&2
  as_root sed -i '/azure\.archive\.ubuntu\.com/d' "$MIRRORS_FILE" || true
}

self="${BASH_SOURCE[0]}"
fetched=0
for attempt in $(seq 1 "$ATTEMPTS"); do
  rc=0
  as_root timeout -k 10 "$ATTEMPT_TIMEOUT" bash "$self" --fetch "$@" || rc=$?
  if [ "$rc" -eq 0 ]; then
    fetched=1
    break
  fi
  if [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    echo "ci-apt: attempt $attempt/$ATTEMPTS ran past ${ATTEMPT_TIMEOUT}s and was stopped" >&2
  else
    echo "ci-apt: attempt $attempt/$ATTEMPTS failed (exit $rc)" >&2
  fi
  drop_azure_mirror
  if [ "$attempt" -lt "$ATTEMPTS" ]; then
    sleep $((attempt * 10))
  fi
done
if [ "$fetched" -ne 1 ]; then
  echo "::error::apt could not download after $ATTEMPTS attempts: $*" >&2
  exit 1
fi

# Offline and unclocked: everything it needs is in /var/cache/apt/archives.
as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-download "$@"
