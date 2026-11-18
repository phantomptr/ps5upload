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
# apt is pointed at the next mirror of the runner's own family (use_next_mirror below: x64 and
# arm64 runners read different archives): whichever mirror the runner image picked (Azure's, or
# an AWS one like us-east-2.ec2.archive.ubuntu.com, which trickled 13 MB in 4 minutes on
# 2026-11-18 and failed all three tries of a release build), the next try reads another one.
# Only then does the real install run, from the downloaded files (`--no-download`) and with no
# clock: dpkg must never be killed halfway.
#
# A step that calls this needs a timeout above the three tries (3 x 250 s, plus 30 s of pauses:
# 13 minutes) plus the install itself: 20 minutes.
#
# A release build (publish.yml, docker-engine.yml) calls this through scripts/ci-apt-latest.sh,
# which runs the default branch's copy: a tag keeps its own files for ever, and v12.6.6 could
# never be published because the fix for its failing mirror was on main only.

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

# Mirrors to move to after a failed try, in order. Plain http like the runner's own list (apt
# checks the signed Release files, not the transport). Two families, never mixed: amd64 packages
# live under /ubuntu/, every other architecture (the ubuntu-24.04-arm runners) under
# /ubuntu-ports/ on other hosts. archive.ubuntu.com has no arm64 packages at all, so pointing an
# arm64 runner at it would turn a slow mirror into a certain failure.
ARCHIVE_MIRRORS=(
  "http://archive.ubuntu.com/ubuntu/"
  "http://mirrors.edge.kernel.org/ubuntu/"
)
PORTS_MIRRORS=(
  "http://ports.ubuntu.com/ubuntu-ports/"
  "http://mirrors.ocf.berkeley.edu/ubuntu-ports/"
)
# Where the runner's Ubuntu sources are (deb822 on 24.04, the old one-line file before).
# Third-party lists (LLVM, PPAs) are other files and are never touched.
SOURCES_FILES="${CI_APT_SOURCES_FILES:-/etc/apt/sources.list.d/ubuntu.sources /etc/apt/sources.list}"

apt_arch() {
  echo "${CI_APT_ARCH:-$(dpkg --print-architecture 2>/dev/null || uname -m)}"
}

# stdin -> file, as root when the file is not ours to write.
write_file() {
  if [ -w "$1" ]; then
    cat > "$1"
  else
    as_root tee "$1" >/dev/null
  fi
}

# The Ubuntu mirror URLs in the sources files, one per line, no trailing slash: an
# *.archive.ubuntu.com or *.ports.ubuntu.com host, or one of the fallbacks above (a later try
# moves off those too). Not security.ubuntu.com, and nothing else that merely ends in /ubuntu
# (a PPA).
sources_mirrors() {
  local f known url
  known="$(printf '%s\n' "${ARCHIVE_MIRRORS[@]}" "${PORTS_MIRRORS[@]}" | sed 's#/$##')"
  for f in $SOURCES_FILES; do
    [ -f "$f" ] || continue
    { grep -v '^[[:space:]]*#' "$f" || true; } | { grep -Eo 'https?://[A-Za-z0-9._/-]+' || true; }
  done | sed 's#/$##' | while IFS= read -r url; do
    if printf '%s\n' "$url" | grep -Eq '^https?://([a-z0-9-]+\.)*(archive|ports)\.ubuntu\.com/ubuntu(-ports)?$' \
        || printf '%s\n' "$known" | grep -qxF "$url"; then
      printf '%s\n' "$url"
    fi
  done
}

# Does a sources file read the mirror list?
list_in_use() {
  local f
  [ -f "$MIRRORS_FILE" ] || return 1
  for f in $SOURCES_FILES; do
    [ -f "$f" ] || continue
    if { grep -v '^[[:space:]]*#' "$f" || true; } | grep -qF "mirror+file:$MIRRORS_FILE"; then
      return 0
    fi
  done
  return 1
}

# Points apt at the mirror for try number $1 (2, 3, ...).
#  - The runner reads a mirror list (GitHub's x64 images: the sources say mirror+file:, the list
#    has one "URL<TAB>priority:N" line per mirror): apt reads the list top-down and only moves on
#    when a mirror errors, not when it is slow, so the list becomes that one mirror.
#  - No list, or a list nothing reads (an image whose sources name ports.ubuntu.com directly):
#    the mirror's URL is replaced in the Ubuntu sources files themselves.
# The family (/ubuntu/ or /ubuntu-ports/) is the one the runner already uses, else the machine's
# architecture. A fallback the runner is already on is skipped. Sources this cannot read (no
# list, no known Ubuntu mirror in the sources) are left alone.
use_next_mirror() {
  local next_try="$1" mode current family mirror idx n i
  if list_in_use; then
    mode=list
    current="$( { grep -Eo '^https?://[^[:space:]]+' "$MIRRORS_FILE" || true; } | sed -n 1p)"
  else
    mode=sources
    current="$(sources_mirrors | sed -n 1p)"
    if [ -z "$current" ]; then
      echo "ci-apt: no mirror list in use and no Ubuntu mirror in the sources; the next try reads the same ones" >&2
      return 0
    fi
  fi
  case "$current" in
    */ubuntu-ports|*/ubuntu-ports/) family=ports ;;
    */ubuntu|*/ubuntu/) family=archive ;;
    *)
      case "$(apt_arch)" in
        amd64|i386|x86_64) family=archive ;;
        *) family=ports ;;
      esac
      ;;
  esac
  local mirrors=("${ARCHIVE_MIRRORS[@]}")
  if [ "$family" = ports ]; then
    mirrors=("${PORTS_MIRRORS[@]}")
  fi
  n=${#mirrors[@]}
  idx=$(( (next_try - 2) % n ))
  mirror=""
  for i in $(seq 0 $((n - 1))); do
    mirror="${mirrors[$(( (idx + i) % n ))]}"
    if [ "${mirror%/}" != "${current%/}" ]; then
      break
    fi
  done
  echo "ci-apt: next try reads $mirror (was: ${current:-nothing}; $mode, $family)" >&2
  if [ "$mode" = list ]; then
    printf '%s\tpriority:1\n' "$mirror" | write_file "$MIRRORS_FILE"
    return 0
  fi
  local f re url tmp wanted
  wanted=ubuntu
  if [ "$family" = ports ]; then
    wanted=ubuntu-ports
  fi
  tmp="$(mktemp)"
  for f in $SOURCES_FILES; do
    [ -f "$f" ] || continue
    cat "$f" > "$tmp"
    # Every known mirror of this family in the file (on ports, security is the same host).
    while IFS= read -r url; do
      [ "${url##*/}" = "$wanted" ] || continue
      re="$(printf '%s' "$url" | sed 's/[.]/\\./g')"
      sed -E "/^[[:space:]]*#/!s#${re}/?([[:space:]]|\$)#${mirror}\\1#g" "$tmp" > "$tmp.new"
      cat "$tmp.new" > "$tmp"
    done < <(sources_mirrors | sort -u)
    if ! cmp -s "$f" "$tmp"; then
      write_file "$f" < "$tmp"
    fi
  done
  rm -f "$tmp" "$tmp.new"
}

# Test hook (scripts/ci-apt.test.mjs): only move the mirror as a failed try would; install nothing.
if [ "${1:-}" = "--rotate" ]; then
  use_next_mirror "${2:?--rotate needs the try number}"
  exit 0
fi

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
  if [ "$attempt" -lt "$ATTEMPTS" ]; then
    # Never the reason the install fails: a mirror that cannot be switched is tried again as is.
    use_next_mirror $((attempt + 1)) || echo "ci-apt: could not switch mirrors; the next try reads the same one" >&2
    sleep $((attempt * 10))
  fi
done
if [ "$fetched" -ne 1 ]; then
  echo "::error::apt could not download after $ATTEMPTS attempts: $*" >&2
  exit 1
fi

# Offline and unclocked: everything it needs is in /var/cache/apt/archives.
as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-download "$@"
