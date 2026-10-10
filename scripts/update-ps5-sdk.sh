#!/usr/bin/env bash
# Move ps5upload to a newer PS5 Payload SDK release and install it.
#
#   scripts/update-ps5-sdk.sh               latest release
#   scripts/update-ps5-sdk.sh --tag v0.44   a specific release
#   scripts/update-ps5-sdk.sh --check       only report whether an update exists
#                                           (exit 0: up to date, 3: update available)
#   scripts/update-ps5-sdk.sh --no-install  update the pin, skip the local install
#
# Steps: resolve the tag, download the release zip and check it unpacks to an
# SDK, rewrite PS5_SDK_TAG/PS5_SDK_SHA256 in scripts/ps5-sdk.env, refresh the
# vendored prospero-nid and the vendored crt (payload/third_party/sdk-crt) when
# upstream changed them at that tag, move the "SDK vX.Y" mentions in the docs,
# then install with scripts/install-ps5-sdk.sh into
# PS5_SDK_INSTALL_DIR (default /opt/ps5-payload-sdk). The previous SDK is kept
# as a backup, never deleted. Re-running at the pinned tag changes nothing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SDK_METADATA="$SCRIPT_DIR/ps5-sdk.env"
PORTABLE_NID="$SCRIPT_DIR/ps5-sdk/prospero-nid"
CRT_DIR="$REPO_ROOT/payload/third_party/sdk-crt"
REPO="ps5-payload-dev/sdk"
INSTALL_DIR="${PS5_SDK_INSTALL_DIR:-/opt/ps5-payload-sdk}"
VERSION_MARKER=".ps5upload-sdk-version"

# Files that name the pinned release in prose ("SDK v0.43", "currently v0.43").
DOC_FILES=(
  README.md
  FAQ.md
  TESTING.md
  scripts/install-macos.sh
  scripts/install-ubuntu.sh
  scripts/install-windows.ps1
)

TAG=""
CHECK_ONLY=0
DO_INSTALL=1
TMP_DIR=""

log() { printf '\n==> %s\n' "$*"; }
ok() { printf '✓ %s\n' "$*"; }
warn() { printf 'WARNING: %s\n' "$*" >&2; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

usage() {
  sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

cleanup() {
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    rm -rf -- "$TMP_DIR"
  fi
}
trap cleanup EXIT

while [ $# -gt 0 ]; do
  case "$1" in
    --tag) [ $# -ge 2 ] || die "--tag needs a value"; TAG="$2"; shift 2 ;;
    --tag=*) TAG="${1#--tag=}"; shift ;;
    --check) CHECK_ONLY=1; shift ;;
    --no-install) DO_INSTALL=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

fetch() { # url dest
  local auth=()
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    case "$1" in https://api.github.com/*) auth=(-H "Authorization: Bearer $GITHUB_TOKEN") ;; esac
  fi
  curl --retry 3 --retry-delay 2 --retry-connrefused --fail --location \
    --silent --show-error ${auth[@]+"${auth[@]}"} --output "$2" "$1"
}

latest_tag() {
  local tag=""
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    tag="$(gh api "repos/$REPO/releases/latest" --jq .tag_name 2>/dev/null || true)"
  fi
  if [ -z "$tag" ]; then
    local json="$TMP_DIR/latest.json"
    fetch "https://api.github.com/repos/$REPO/releases/latest" "$json"
    tag="$(sed -n 's/^ *"tag_name": *"\([^"]*\)".*/\1/p' "$json" | head -n1)"
  fi
  [ -n "$tag" ] || die "could not read the latest release of $REPO"
  printf '%s\n' "$tag"
}

env_value() { sed -n "s/^$1=//p" "$SDK_METADATA" | head -n1; }

set_env_value() { # key value
  local tmp="$SDK_METADATA.tmp.$$"
  awk -v k="$1" -v v="$2" 'index($0, k "=") == 1 { print k "=" v; next } { print }' \
    "$SDK_METADATA" > "$tmp"
  mv "$tmp" "$SDK_METADATA"
}

# "SDK v0.43" -> "SDK v0.44" and "currently v0.43" / "currently **v0.43**".
move_doc_mentions() { # old new
  local old="$1" new="$2" file changed=0
  local old_re="${old//./\\.}"
  for file in "${DOC_FILES[@]}"; do
    [ -f "$REPO_ROOT/$file" ] || continue
    if grep -qE "(SDK|currently) (\*\*)?$old_re([^0-9]|$)" "$REPO_ROOT/$file"; then
      local tmp="$REPO_ROOT/$file.tmp.$$"
      sed -E "s/(SDK|currently) (\*\*)?$old_re([^0-9]|$)/\1 \2$new\3/g" "$REPO_ROOT/$file" > "$tmp"
      cat "$tmp" > "$REPO_ROOT/$file" # keep the file's mode
      rm -f -- "$tmp"
      ok "Docs: $file now says $new"
      changed=1
    fi
  done
  [ "$changed" -eq 1 ] || ok "Docs: no $old mentions to move"
}

# Re-vendor crt/ and re-apply ps5upload.patch when the tag's crt differs from
# what we vendored. Refuses (before anything is rewritten) if the patch no
# longer applies: that needs a person, see payload/third_party/sdk-crt/README.md.
check_crt() { # source-dir
  local src="$1/crt" work="$TMP_DIR/crt-check" f
  [ -d "$src" ] || die "the $TAG source has no crt/ directory"
  rm -rf -- "$work"
  mkdir -p "$work/crt"
  cp "$src"/*.c "$src"/*.h "$src"/*.S "$work/crt/"
  (cd "$work" && patch -p1 --quiet --forward < "$CRT_DIR/ps5upload.patch" >/dev/null) || \
    die "ps5upload.patch no longer applies to the $TAG crt; re-vendor it by hand (payload/third_party/sdk-crt/README.md)"
  CRT_CHANGED=0
  for f in "$work"/crt/*; do
    cmp -s "$f" "$CRT_DIR/$(basename "$f")" || { CRT_CHANGED=1; break; }
  done
  CRT_PATCHED_DIR="$work/crt"
}

# ── Resolve the target release ─────────────────────────────────────────────

[ -f "$SDK_METADATA" ] || die "missing $SDK_METADATA"
TMP_DIR="$(mktemp -d)"
CUR_TAG="$(env_value PS5_SDK_TAG)"
CUR_SHA="$(env_value PS5_SDK_SHA256)"

if [ -z "$TAG" ]; then
  log "Looking up the latest $REPO release"
  TAG="$(latest_tag)"
fi
case "$TAG" in
  v[0-9]*.[0-9]*) ;;
  *) die "not a release tag: $TAG" ;;
esac
ok "Pinned: $CUR_TAG   Target: $TAG"

if [ "$CHECK_ONLY" -eq 1 ]; then
  if [ "$TAG" = "$CUR_TAG" ]; then
    ok "Up to date"
    exit 0
  fi
  printf 'Update available: %s -> %s (run scripts/update-ps5-sdk.sh --tag %s)\n' "$CUR_TAG" "$TAG" "$TAG"
  exit 3
fi

# ── Download and check the release ─────────────────────────────────────────

log "Downloading $REPO $TAG"
ZIP="$TMP_DIR/ps5-payload-sdk.zip"
fetch "https://github.com/$REPO/releases/download/$TAG/ps5-payload-sdk.zip" "$ZIP"
SHA="$(sha256_file "$ZIP")"
ok "Release zip SHA-256 $SHA"
if [ "$TAG" = "$CUR_TAG" ] && [ "$SHA" != "$CUR_SHA" ]; then
  die "the $TAG zip no longer matches the pinned checksum ($CUR_SHA); upstream re-uploaded it, check before trusting it"
fi
while IFS= read -r entry; do
  case "$entry" in
    ps5-payload-sdk/*) ;;
    *) die "unexpected archive entry: $entry" ;;
  esac
  case "/$entry/" in
    */../*|*/./*) die "unsafe archive entry: $entry" ;;
  esac
done < <(unzip -Z1 "$ZIP")
unzip -q "$ZIP" -d "$TMP_DIR/zip"
[ -f "$TMP_DIR/zip/ps5-payload-sdk/toolchain/prospero.mk" ] || die "the $TAG zip has no toolchain/prospero.mk"
[ -f "$TMP_DIR/zip/ps5-payload-sdk/target/lib/crt1.o" ] || die "the $TAG zip has no target/lib/crt1.o"
ok "Archive unpacks to an SDK"

log "Fetching the $TAG source (prospero-nid, crt)"
SRC_TGZ="$TMP_DIR/src.tar.gz"
fetch "https://codeload.github.com/$REPO/tar.gz/refs/tags/$TAG" "$SRC_TGZ"
mkdir -p "$TMP_DIR/src"
tar -xzf "$SRC_TGZ" -C "$TMP_DIR/src" --strip-components=1
UP_NID=""
for candidate in host/bin/prospero-nid bin/prospero-nid; do
  if [ -f "$TMP_DIR/src/$candidate" ]; then UP_NID="$TMP_DIR/src/$candidate"; break; fi
done
[ -n "$UP_NID" ] || die "the $TAG source has no prospero-nid"
head -n1 "$UP_NID" | grep -q python || die "upstream prospero-nid at $TAG is no longer a Python script; vendor it by hand"
check_crt "$TMP_DIR/src"

# ── Rewrite the pin ────────────────────────────────────────────────────────

log "Updating the repository pin"
if [ "$TAG" = "$CUR_TAG" ]; then
  ok "scripts/ps5-sdk.env already pins $TAG"
else
  set_env_value PS5_SDK_TAG "$TAG"
  set_env_value PS5_SDK_SHA256 "$SHA"
  ok "scripts/ps5-sdk.env: $CUR_TAG -> $TAG"
fi

UP_NID_SHA="$(sha256_file "$UP_NID")"
if [ "$UP_NID_SHA" = "$(sha256_file "$PORTABLE_NID")" ]; then
  ok "prospero-nid unchanged upstream"
else
  install -m 0755 "$UP_NID" "$PORTABLE_NID"
  set_env_value PS5_SDK_NID_SHA256 "$UP_NID_SHA"
  ok "prospero-nid updated from $TAG (sha256 $UP_NID_SHA)"
fi

if [ "$CRT_CHANGED" -eq 0 ]; then
  ok "crt unchanged upstream (vendored copy + ps5upload.patch is current)"
else
  cp "$CRT_PATCHED_DIR"/* "$CRT_DIR/"
  warn "crt changed upstream at $TAG: re-vendored into payload/third_party/sdk-crt with ps5upload.patch re-applied."
  warn "Review the diff, rebuild, and compare llvm-nm of payload/third_party/sdk-crt/build/crt1.o with the SDK's target/lib/crt1.o."
fi

if [ "$TAG" != "$CUR_TAG" ]; then
  move_doc_mentions "$CUR_TAG" "$TAG"
fi

# ── Install ────────────────────────────────────────────────────────────────

if [ "$DO_INSTALL" -eq 0 ]; then
  ok "Skipping the install (--no-install)"
  exit 0
fi

log "Installing $TAG into $INSTALL_DIR"
INSTALLER="$SCRIPT_DIR/install-ps5-sdk.sh"
PARENT="$(dirname "$INSTALL_DIR")"
installed_tag() { tr -d '[:space:]' < "$INSTALL_DIR/$VERSION_MARKER" 2>/dev/null || true; }

if [ -f "$INSTALL_DIR/toolchain/prospero.mk" ] && [ "$(installed_tag)" = "$TAG" ] \
  && [ ! -w "$INSTALL_DIR" ]; then
  ok "$TAG is already installed at $INSTALL_DIR"
elif { [ -d "$PARENT" ] && [ -w "$PARENT" ]; } \
  || { [ "$(installed_tag)" = "$TAG" ] && [ -w "$INSTALL_DIR" ]; }; then
  # The installer moves an old SDK aside itself, next to the new one.
  PS5_SDK_INSTALL_DIR="$INSTALL_DIR" bash "$INSTALLER"
else
  # The parent (e.g. /opt) needs root. Stage the SDK as this user (so the
  # SQLite sources the installer fetches into the repo stay user-owned), then
  # swap it in.
  STAGE="$TMP_DIR/stage/ps5-payload-sdk"
  mkdir -p "$(dirname "$STAGE")"
  PS5_SDK_INSTALL_DIR="$STAGE" bash "$INSTALLER"
  OLD_TAG="$(installed_tag)"
  BACKUP_SUFFIX="backup-${OLD_TAG:-unknown}-$(date +%Y%m%d-%H%M%S)"
  if [ -t 0 ] || sudo -n true 2>/dev/null; then
    if [ -e "$INSTALL_DIR" ]; then
      sudo mv "$INSTALL_DIR" "$INSTALL_DIR.$BACKUP_SUFFIX"
      ok "Moved the previous SDK to $INSTALL_DIR.$BACKUP_SUFFIX"
    fi
    sudo mkdir -p "$PARENT"
    sudo mv "$STAGE" "$INSTALL_DIR"
  elif [ -d "$INSTALL_DIR" ] && [ -w "$INSTALL_DIR" ]; then
    # No sudo, but the SDK directory itself is ours: swap its contents and
    # keep the old ones in the home directory (the parent isn't writable).
    BACKUP="${PS5_SDK_BACKUP_DIR:-$HOME}/$(basename "$INSTALL_DIR").$BACKUP_SUFFIX"
    mkdir -p "$BACKUP"
    find "$INSTALL_DIR" -mindepth 1 -maxdepth 1 -exec mv {} "$BACKUP/" \;
    if ! find "$STAGE" -mindepth 1 -maxdepth 1 -exec mv {} "$INSTALL_DIR/" \;; then
      find "$INSTALL_DIR" -mindepth 1 -maxdepth 1 -exec rm -rf {} + || true
      find "$BACKUP" -mindepth 1 -maxdepth 1 -exec mv {} "$INSTALL_DIR/" \;
      die "could not install into $INSTALL_DIR; the previous SDK was restored"
    fi
    ok "Moved the previous SDK to $BACKUP ($PARENT is not writable without sudo)"
  else
    die "$INSTALL_DIR is not writable; re-run from a terminal so sudo can ask for a password"
  fi
  ok "Installed PS5 Payload SDK $TAG at $INSTALL_DIR"
fi

[ "$(installed_tag)" = "$TAG" ] || die "$INSTALL_DIR does not report $TAG after the install"

log "Done"
printf 'SDK %s at %s.\nNext: rm -rf payload/build payload/installer/build payload/third_party/sdk-crt/build && make payload test-payload\n' \
  "$TAG" "$INSTALL_DIR"
if [ "$TAG" != "$CUR_TAG" ]; then
  printf 'Review and commit: git diff -- scripts/ps5-sdk.env scripts/ps5-sdk payload/third_party/sdk-crt %s\n' "${DOC_FILES[*]}"
fi
