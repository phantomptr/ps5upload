#!/usr/bin/env bash
# Install the repository-pinned PS5 Payload SDK release on POSIX hosts.
#
# The release archive is checksum-verified before extraction. Existing SDKs
# without the matching version marker are moved aside, not deleted, so an
# upgrade can be rolled back. The v0.42 archive was assembled on Linux and
# contains a Linux/x86-64 prospero-nid binary even though the upstream source
# is portable Python; install that exact tagged source implementation so the
# SDK also works on macOS and ARM Linux.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SDK_METADATA="$SCRIPT_DIR/ps5-sdk.env"
PORTABLE_NID="$SCRIPT_DIR/ps5-sdk/prospero-nid"

[ -f "$SDK_METADATA" ] || { echo "ERROR: missing $SDK_METADATA" >&2; exit 1; }
# shellcheck disable=SC1090
source "$SDK_METADATA"

SDK_DIR="${PS5_SDK_INSTALL_DIR:-$HOME/ps5-payload-sdk}"
SDK_URL="https://github.com/ps5-payload-dev/sdk/releases/download/${PS5_SDK_TAG}/ps5-payload-sdk.zip"
VERSION_MARKER=".ps5upload-sdk-version"
EXPECTED_NID="4J2sUJmuHZQ"
TMP_DIR=""

log() { printf '\n==> %s\n' "$*"; }
ok() { printf '✓ %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

cleanup() {
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    rm -rf -- "$TMP_DIR"
  fi
}
trap cleanup EXIT

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "need sha256sum or shasum to verify the SDK download"
  fi
}

validate_metadata() {
  case "$PS5_SDK_TAG" in
    v[0-9]*.[0-9]*) ;;
    *) die "invalid PS5_SDK_TAG in $SDK_METADATA: $PS5_SDK_TAG" ;;
  esac
  case "$PS5_SDK_SHA256" in
    ""|*[!0-9a-f]*) die "invalid PS5_SDK_SHA256 in $SDK_METADATA" ;;
  esac
  case "$PS5_SDK_NID_SHA256" in
    ""|*[!0-9a-f]*) die "invalid PS5_SDK_NID_SHA256 in $SDK_METADATA" ;;
  esac
  [ "${#PS5_SDK_SHA256}" -eq 64 ] || die "PS5_SDK_SHA256 must contain 64 hex characters"
  [ "${#PS5_SDK_NID_SHA256}" -eq 64 ] || die "PS5_SDK_NID_SHA256 must contain 64 hex characters"
  case "$PS5_SQLITE_VERSION" in
    ""|*[!0-9]*) die "invalid PS5_SQLITE_VERSION in $SDK_METADATA" ;;
  esac
  case "$PS5_SQLITE_YEAR" in
    ""|*[!0-9]*) die "invalid PS5_SQLITE_YEAR in $SDK_METADATA" ;;
  esac
  case "$PS5_SQLITE_SHA256" in
    ""|*[!0-9a-f]*) die "invalid PS5_SQLITE_SHA256 in $SDK_METADATA" ;;
  esac
  [ "${#PS5_SQLITE_SHA256}" -eq 64 ] || die "PS5_SQLITE_SHA256 must contain 64 hex characters"
  [ -f "$PORTABLE_NID" ] || die "missing portable prospero-nid at $PORTABLE_NID"
  [ "$(sha256_file "$PORTABLE_NID")" = "$PS5_SDK_NID_SHA256" ] || \
    die "portable prospero-nid checksum does not match $SDK_METADATA"
}

install_portable_nid() {
  local root="$1"
  command -v python3 >/dev/null 2>&1 || die "python3 is required by the portable prospero-nid tool"
  install -m 0755 "$PORTABLE_NID" "$root/bin/prospero-nid"
  local actual
  actual="$("$root/bin/prospero-nid" sceKernelGetProcessTime)"
  [ "$actual" = "$EXPECTED_NID" ] || die "prospero-nid self-test failed (got $actual)"
}

# ── SQLite amalgamation ──────────────────────────────────────────────────
#
# The payload links a real SQLite so it can read (and repair) the console's
# content databases with the actual record parser instead of a hand-rolled
# b-tree walker. No firmware ships libSceSqlite.sprx, so this cannot come
# from the console — it is compiled into the payload from these sources.
#
# Sources are fetched rather than committed: the amalgamation is a 9 MB
# generated file, pinned by version and SHA-256 in ps5-sdk.env. Only
# sqlite3.c and sqlite3.h are kept; shell.c and sqlite3ext.h are dropped
# (a CLI shell and the extension ABI, neither of which the payload builds).
install_sqlite_amalgamation() {
  local dir="$SCRIPT_DIR/../payload/third_party/sqlite3"
  local stamp="$dir/.version"

  if [ -f "$dir/sqlite3.c" ] && [ -f "$dir/sqlite3.h" ] \
    && [ -f "$stamp" ] \
    && [ "$(tr -d '[:space:]' < "$stamp")" = "$PS5_SQLITE_VERSION" ]; then
    ok "SQLite $PS5_SQLITE_VERSION amalgamation already present"
    return 0
  fi

  log "Downloading SQLite $PS5_SQLITE_VERSION amalgamation"
  local tmp zip
  tmp="$(mktemp -d)"
  zip="$tmp/sqlite-amalgamation.zip"
  curl --retry 3 --retry-delay 2 --retry-connrefused \
    --fail --location --silent --show-error --output "$zip" \
    "https://www.sqlite.org/${PS5_SQLITE_YEAR}/sqlite-amalgamation-${PS5_SQLITE_VERSION}.zip"

  local actual
  actual="$(sha256_file "$zip")"
  if [ "$actual" != "$PS5_SQLITE_SHA256" ]; then
    rm -rf -- "$tmp"
    die "SQLite checksum mismatch: expected $PS5_SQLITE_SHA256, got $actual"
  fi
  ok "Verified SQLite archive SHA-256 $actual"

  # Same archive-layout guard the SDK download uses: every entry must live
  # under the single documented top-level directory, with no traversal.
  local top="sqlite-amalgamation-${PS5_SQLITE_VERSION}"
  while IFS= read -r entry; do
    case "$entry" in
      "$top"/*) ;;
      *) rm -rf -- "$tmp"; die "unexpected SQLite archive entry: $entry" ;;
    esac
    case "/$entry/" in
      */../*|*/./*) rm -rf -- "$tmp"; die "unsafe SQLite archive entry: $entry" ;;
    esac
  done < <(unzip -Z1 "$zip")

  unzip -q "$zip" -d "$tmp"
  [ -f "$tmp/$top/sqlite3.c" ] || { rm -rf -- "$tmp"; die "SQLite archive is missing sqlite3.c"; }
  [ -f "$tmp/$top/sqlite3.h" ] || { rm -rf -- "$tmp"; die "SQLite archive is missing sqlite3.h"; }

  mkdir -p "$dir"
  install -m 0644 "$tmp/$top/sqlite3.c" "$dir/sqlite3.c"
  install -m 0644 "$tmp/$top/sqlite3.h" "$dir/sqlite3.h"
  printf '%s\n' "$PS5_SQLITE_VERSION" > "$stamp"
  chmod 0644 "$stamp"
  rm -rf -- "$tmp"
  ok "Installed SQLite $PS5_SQLITE_VERSION sources at $dir"
}

validate_metadata
install_sqlite_amalgamation

if [ -f "$SDK_DIR/toolchain/prospero.mk" ] \
  && [ -f "$SDK_DIR/$VERSION_MARKER" ] \
  && [ "$(tr -d '[:space:]' < "$SDK_DIR/$VERSION_MARKER")" = "$PS5_SDK_TAG" ]; then
  install_portable_nid "$SDK_DIR"
  ok "PS5 Payload SDK $PS5_SDK_TAG already installed at $SDK_DIR"
  exit 0
fi

log "Downloading PS5 Payload SDK $PS5_SDK_TAG"
TMP_DIR="$(mktemp -d)"
ZIP_PATH="$TMP_DIR/ps5-payload-sdk.zip"
curl --retry 3 --retry-delay 2 --retry-connrefused \
  --fail --location --silent --show-error \
  --output "$ZIP_PATH" "$SDK_URL"

ACTUAL_SHA256="$(sha256_file "$ZIP_PATH")"
[ "$ACTUAL_SHA256" = "$PS5_SDK_SHA256" ] || \
  die "SDK checksum mismatch: expected $PS5_SDK_SHA256, got $ACTUAL_SHA256"
ok "Verified official archive SHA-256 $ACTUAL_SHA256"

# Refuse unexpected archive layouts before invoking unzip. Every entry must be
# below the single documented ps5-payload-sdk/ top-level directory and may not
# contain traversal components.
while IFS= read -r entry; do
  case "$entry" in
    ps5-payload-sdk/*) ;;
    *) die "unexpected SDK archive entry: $entry" ;;
  esac
  case "/$entry/" in
    */../*|*/./*) die "unsafe SDK archive entry: $entry" ;;
  esac
done < <(unzip -Z1 "$ZIP_PATH")

unzip -q "$ZIP_PATH" -d "$TMP_DIR"
STAGED_SDK="$TMP_DIR/ps5-payload-sdk"
[ -f "$STAGED_SDK/toolchain/prospero.mk" ] || die "SDK archive is missing toolchain/prospero.mk"
[ -f "$STAGED_SDK/target/lib/crt1.o" ] || die "SDK archive is missing target/lib/crt1.o"
install_portable_nid "$STAGED_SDK"
printf '%s\n' "$PS5_SDK_TAG" > "$STAGED_SDK/$VERSION_MARKER"

mkdir -p "$(dirname "$SDK_DIR")"
BACKUP_DIR=""
if [ -e "$SDK_DIR" ]; then
  BACKUP_DIR="${SDK_DIR}.backup-$(date +%Y%m%d-%H%M%S)-$$"
  mv "$SDK_DIR" "$BACKUP_DIR"
  ok "Moved the previous SDK to $BACKUP_DIR"
fi

if ! mv "$STAGED_SDK" "$SDK_DIR"; then
  if [ -n "$BACKUP_DIR" ] && [ -e "$BACKUP_DIR" ] && [ ! -e "$SDK_DIR" ]; then
    mv "$BACKUP_DIR" "$SDK_DIR"
  fi
  die "could not install the SDK at $SDK_DIR"
fi

ok "Installed PS5 Payload SDK $PS5_SDK_TAG at $SDK_DIR"
