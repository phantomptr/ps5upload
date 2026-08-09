#!/usr/bin/env bash
# install-macos.sh — one-shot dev environment bootstrap for macOS (Intel + Apple Silicon).
#
# Idempotent: safe to re-run.
#
# Installs:
#   - Xcode Command Line Tools (system WebKit framework + clang)
#   - Homebrew (if missing) → node, llvm, python, openssl@3, pkg-config, cmake, file
#     (the Makefile discovers Homebrew's current LLVM prefix automatically)
#   - Rust toolchain (rustup, stable, default profile)
#   - Repository-pinned PS5 Payload SDK → $PS5_PAYLOAD_SDK
#     (currently v0.42; default $HOME/ps5-payload-sdk)
#
# After it finishes the script prints the env exports you need to add to ~/.zshrc
# (or ~/.bash_profile) so `make build` and `make run-client` work in any new shell.

set -euo pipefail

# ─── config ────────────────────────────────────────────────────────────────────
# Where to install the PS5 SDK. Default is $HOME/ps5-payload-sdk — a user-writable
# path that doesn't need sudo. We deliberately do NOT read PS5_PAYLOAD_SDK here:
# that env var is "where the SDK lives at build time" and the Makefile defaults
# it to /opt/ps5-payload-sdk (root-only). Override the install location with
# PS5_SDK_INSTALL_DIR if you want somewhere else.
SDK_DIR="${PS5_SDK_INSTALL_DIR:-$HOME/ps5-payload-sdk}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

BREW_DEPS=(
  node
  llvm
  python
  openssl@3
  pkg-config
  cmake
  file
)

# ─── helpers ───────────────────────────────────────────────────────────────────
log()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m✓\033[0m %s\n'   "$*"; }
warn() { printf '\033[1;33m! %s\033[0m\n'   "$*"; }
die()  { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

require() { command -v "$1" >/dev/null 2>&1; }

# ─── pre-flight ────────────────────────────────────────────────────────────────
if [ "$(uname -s)" != "Darwin" ]; then
  die "This script targets macOS. For Linux use scripts/install-ubuntu.sh; for Windows scripts/install-windows.ps1."
fi

# ─── 1. Xcode Command Line Tools ───────────────────────────────────────────────
if xcode-select -p >/dev/null 2>&1; then
  ok "Xcode Command Line Tools already installed: $(xcode-select -p)"
else
  log "Installing Xcode Command Line Tools (a GUI prompt will appear; complete it then re-run this script)"
  xcode-select --install || true
  die "Re-run this script after the Xcode CLT install completes."
fi

# ─── 2. Homebrew ───────────────────────────────────────────────────────────────
if require brew; then
  ok "Homebrew already installed: $(brew --version | head -1)"
else
  log "Installing Homebrew"
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  # Make brew available in this shell (Apple Silicon vs Intel paths)
  if [ -x /opt/homebrew/bin/brew ]; then
    eval "$(/opt/homebrew/bin/brew shellenv)"
  elif [ -x /usr/local/bin/brew ]; then
    eval "$(/usr/local/bin/brew shellenv)"
  fi
  ok "Homebrew installed: $(brew --version | head -1)"
fi

# ─── 3. brew packages ──────────────────────────────────────────────────────────
log "Installing brew packages: ${BREW_DEPS[*]}"
brew install "${BREW_DEPS[@]}"
ok "brew packages installed"

# Verify the current Homebrew LLVM is installed. The formula becomes
# version-suffixed only after a newer major replaces it, so hardcoding
# /opt/homebrew/opt/llvm@22 breaks while LLVM 22 is still current.
LLVM_PREFIX="$(brew --prefix llvm 2>/dev/null || true)"
if [ -n "$LLVM_PREFIX" ] && [ -x "$LLVM_PREFIX/bin/llvm-config" ]; then
  ok "LLVM at $LLVM_PREFIX ($("$LLVM_PREFIX/bin/llvm-config" --version))"
else
  die "Homebrew LLVM was installed but llvm-config is not available"
fi

# ─── 4. Rust toolchain ─────────────────────────────────────────────────────────
if require rustc && require cargo; then
  ok "Rust already installed: $(rustc --version)"
else
  log "Installing Rust via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain stable --profile default
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
  ok "Rust installed: $($HOME/.cargo/bin/rustc --version)"
fi

# ─── 5. PS5 Payload SDK ────────────────────────────────────────────────────────
# The shared installer verifies the release checksum, upgrades stale/unmarked
# SDKs recoverably, and replaces v0.42's Linux-only prospero-nid binary with the
# exact portable implementation from the same upstream tag.
PS5_SDK_INSTALL_DIR="$SDK_DIR" "$REPO_ROOT/scripts/install-ps5-sdk.sh"

# ─── 6. client npm deps ────────────────────────────────────────────────────────
if [ -d "$REPO_ROOT/client" ]; then
  log "Installing client npm dependencies"
  (cd "$REPO_ROOT/client" && npm install --no-audit --no-fund)
  ok "client/node_modules ready"
fi

# ─── 7. wrap up ────────────────────────────────────────────────────────────────
RC_FILE="$HOME/.zshrc"
[ -n "${BASH_VERSION:-}" ] && RC_FILE="$HOME/.bash_profile"

ok "Setup complete."
cat <<EOF

Add these to your shell rc ($RC_FILE) so future shells pick them up:

  export PS5_PAYLOAD_SDK="$SDK_DIR"
  . "\$HOME/.cargo/env"
  # The Makefile auto-sets LLVM_CONFIG on macOS — no action needed there.

Then in this terminal:

  export PS5_PAYLOAD_SDK="$SDK_DIR"
  source "\$HOME/.cargo/env"
  make build
  make run-client

EOF
