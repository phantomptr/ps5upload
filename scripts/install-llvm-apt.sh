#!/usr/bin/env bash
# Adds apt.llvm.org's repository for one LLVM version (Ubuntu 24.04 does not ship llvm-22) on an
# Ubuntu or Debian host, so `apt-get update` then `apt-get install clang-22 ...` work.
#
# Replaces `curl https://apt.llvm.org/llvm.sh | sudo bash`: that ran a mutable remote script as
# root on the machines that build the shipped payload. Here the only thing fetched is the signing
# key, refused unless it has the pinned fingerprint, and apt checks the packages against it.
#
# Usage: sudo scripts/install-llvm-apt.sh 22

set -euo pipefail

# LLVM's Debian packaging key (Sylvestre Ledru), https://apt.llvm.org/
KEY_FINGERPRINT="6084F3CF814B57C1CF12EFD515CF4D18AF4F7421" # gitleaks:allow (a public key's fingerprint)
KEY_URL="https://apt.llvm.org/llvm-snapshot.gpg.key"
KEYRING="/etc/apt/keyrings/apt.llvm.org.gpg"

version="${1:-}"
case "$version" in
  ""|*[!0-9]*) echo "usage: $0 <llvm major version>" >&2; exit 2 ;;
esac

# shellcheck disable=SC1091
. /etc/os-release
# Ubuntu derivatives name the Ubuntu release they follow in UBUNTU_CODENAME.
codename="${UBUNTU_CODENAME:-${VERSION_CODENAME:-}}"
[ -n "$codename" ] || { echo "install-llvm-apt: no release codename in /etc/os-release" >&2; exit 1; }

command -v gpg >/dev/null || { echo "install-llvm-apt: gpg is required (apt-get install gnupg)" >&2; exit 1; }

key="$(mktemp)"
trap 'rm -f "$key"' EXIT
curl --fail --location --silent --show-error --retry 3 --retry-delay 5 --output "$key" "$KEY_URL"
fingerprint="$(gpg --show-keys --with-colons "$key" | awk -F: '$1 == "fpr" { print $10; exit }')"
if [ "$fingerprint" != "$KEY_FINGERPRINT" ]; then
  echo "install-llvm-apt: signing key has fingerprint '$fingerprint', expected $KEY_FINGERPRINT" >&2
  exit 1
fi

install -d -m 0755 "$(dirname "$KEYRING")"
gpg --batch --yes --dearmor --output "$KEYRING" "$key"
chmod 0644 "$KEYRING"
echo "deb [signed-by=$KEYRING] https://apt.llvm.org/$codename/ llvm-toolchain-$codename-$version main" \
  > "/etc/apt/sources.list.d/llvm-$version.list"
echo "Added apt.llvm.org llvm-$version for $codename; run apt-get update next."
