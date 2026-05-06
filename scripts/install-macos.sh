#!/usr/bin/env bash
# install-macos.sh — one-shot dev environment bootstrap for macOS (Intel + Apple Silicon).
#
# Idempotent: safe to re-run.
#
# Installs:
#   - Xcode Command Line Tools (system WebKit framework + clang)
#   - Homebrew (if missing) → node, llvm@18, openssl@3, pkg-config, cmake, file
#     (llvm@18 is the only Homebrew llvm shipped with `ld.lld`, which prospero-clang needs;
#      the root Makefile pins LLVM_CONFIG to llvm@18 on macOS — keep them aligned)
#   - Rust toolchain (rustup, stable, default profile)
#   - PS5 Payload SDK v0.38 → $PS5_PAYLOAD_SDK (default $HOME/ps5-payload-sdk)
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
SDK_TAG="v0.38"
SDK_URL="https://github.com/ps5-payload-dev/sdk/releases/download/${SDK_TAG}/ps5-payload-sdk.zip"

BREW_DEPS=(
  node
  llvm@18
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

# Verify llvm@18 has ld.lld (the reason the Makefile pins this version)
LLVM18_PREFIX="$(brew --prefix llvm@18 2>/dev/null || true)"
if [ -n "$LLVM18_PREFIX" ] && [ -x "$LLVM18_PREFIX/bin/ld.lld" ]; then
  ok "llvm@18 with ld.lld at $LLVM18_PREFIX"
else
  warn "llvm@18 installed but ld.lld not found — payload build will likely fail"
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
if [ -f "$SDK_DIR/toolchain/prospero.mk" ]; then
  ok "PS5 SDK already present at $SDK_DIR"
else
  log "Downloading PS5 Payload SDK ${SDK_TAG} → $SDK_DIR"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  curl -fL --progress-bar -o "$TMP/sdk.zip" "$SDK_URL"
  unzip -q -o "$TMP/sdk.zip" -d "$TMP"
  if [ ! -d "$TMP/ps5-payload-sdk" ]; then
    die "SDK zip did not contain expected ps5-payload-sdk/ directory"
  fi
  mkdir -p "$(dirname "$SDK_DIR")"
  mv "$TMP/ps5-payload-sdk" "$SDK_DIR"
  trap - EXIT
  rm -rf "$TMP"
  [ -f "$SDK_DIR/toolchain/prospero.mk" ] || die "SDK extracted but prospero.mk missing"
  ok "PS5 SDK installed at $SDK_DIR"
fi

# ─── 6. client npm deps ────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
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
