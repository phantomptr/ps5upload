#!/usr/bin/env bash
# install-ubuntu.sh — one-shot dev environment bootstrap for Ubuntu / Debian / WSL2.
#
# Idempotent: safe to re-run. Each step skips if already satisfied.
#
# Installs:
#   - apt deps for Tauri (libwebkit2gtk-4.1, libgtk-3, librsvg2, libayatana-appindicator3,
#     libxdo, libssl, build-essential, file, curl, wget, unzip, pkg-config, python3)
#   - Rust toolchain (rustup, stable, default profile)
#   - Node.js 22 LTS via NodeSource (only if `node` is missing — keeps existing installs)
#   - Repository-pinned PS5 Payload SDK (currently v0.42) → $PS5_PAYLOAD_SDK
#     (default $HOME/ps5-payload-sdk)
#
# After it finishes, the script prints the env exports you need to add to ~/.bashrc
# (or ~/.zshrc) so `make build` and `make run-client` work in any new shell.

set -euo pipefail

# ─── config ────────────────────────────────────────────────────────────────────
# Where to install the PS5 SDK. Default is $HOME/ps5-payload-sdk — a user-writable
# path that doesn't need sudo. We deliberately do NOT read PS5_PAYLOAD_SDK here:
# that env var is "where the SDK lives at build time" and the Makefile defaults
# it to /opt/ps5-payload-sdk (root-only). Override the install location with
# PS5_SDK_INSTALL_DIR if you want somewhere else.
SDK_DIR="${PS5_SDK_INSTALL_DIR:-$HOME/ps5-payload-sdk}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODE_MAJOR="22"

APT_DEPS=(
  # Tauri 2 system libs (matches _check-tauri-system-deps in the root Makefile)
  libwebkit2gtk-4.1-dev
  libgtk-3-dev
  librsvg2-dev
  libayatana-appindicator3-dev
  libxdo-dev
  libssl-dev
  build-essential
  file
  curl
  wget
  unzip
  pkg-config
  python3
  # LLVM 22 toolchain — required by the PS5 SDK's `prospero-clang` wrapper,
  # which dispatches into `${llvm-bindir}/clang` + `${llvm-bindir}/ld.lld`.
  # SDK v0.42 supports llvm 16–22; we pin to 22 to match macOS's
  # macOS `LLVM_CONFIG` so both platforms use the same major. Ubuntu 24.04
  # doesn't ship llvm-22 in its own repos, so we add apt.llvm.org below
  # before installing these.
  clang-22
  llvm-22-dev
  lld-22
)

# ─── helpers ───────────────────────────────────────────────────────────────────
log()  { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m✓\033[0m %s\n'   "$*"; }
warn() { printf '\033[1;33m! %s\033[0m\n'   "$*"; }
die()  { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

require() { command -v "$1" >/dev/null 2>&1; }

# ─── pre-flight ────────────────────────────────────────────────────────────────
if [ "$(uname -s)" != "Linux" ]; then
  die "This script targets Linux. For macOS use scripts/install-macos.sh; for Windows scripts/install-windows.ps1."
fi
if ! require apt-get; then
  die "apt-get not found — this script targets Debian/Ubuntu/WSL2. Use your distro's package manager manually."
fi

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  require sudo || die "sudo is not installed and you are not root."
  SUDO="sudo"
fi

# ─── 1. apt deps ───────────────────────────────────────────────────────────────
# Add the LLVM apt repo (Ubuntu 24.04 doesn't ship llvm-22 natively).
# The automatic installer script from apt.llvm.org handles GPG key + sources.list.
log "Adding LLVM apt repository for llvm-22"
if ! apt-cache show clang-22 >/dev/null 2>&1; then
  curl -fsSL https://apt.llvm.org/llvm.sh | $SUDO bash -s -- 22
else
  ok "llvm-22 already available in apt"
fi

log "Installing apt deps for Tauri (you may be prompted for your sudo password)"
$SUDO apt-get update
$SUDO apt-get install -y "${APT_DEPS[@]}"
ok "apt deps installed"

# ─── 2. Node.js (only if missing) ──────────────────────────────────────────────
if require node; then
  ok "Node.js already installed: $(node --version)"
else
  log "Installing Node.js ${NODE_MAJOR} LTS via NodeSource"
  curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | $SUDO -E bash -
  $SUDO apt-get install -y nodejs
  ok "Node.js installed: $(node --version)"
fi

# ─── 3. Rust toolchain ─────────────────────────────────────────────────────────
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

# ─── 4. PS5 Payload SDK ────────────────────────────────────────────────────────
# Shared with CI so developer and release builds use the same tag, checksum,
# archive validation, and recoverable upgrade path.
PS5_SDK_INSTALL_DIR="$SDK_DIR" "$REPO_ROOT/scripts/install-ps5-sdk.sh"

# ─── 5. client npm deps ────────────────────────────────────────────────────────
if [ -d "$REPO_ROOT/client" ]; then
  log "Installing client npm dependencies"
  (cd "$REPO_ROOT/client" && npm install --no-audit --no-fund)
  ok "client/node_modules ready"
fi

# ─── 6. wrap up ────────────────────────────────────────────────────────────────
RC_FILE="$HOME/.bashrc"
[ -n "${ZSH_VERSION:-}" ] && RC_FILE="$HOME/.zshrc"

ok "Setup complete."
cat <<EOF

Add these to your shell rc ($RC_FILE) so future shells pick them up:

  export PS5_PAYLOAD_SDK="$SDK_DIR"
  . "\$HOME/.cargo/env"

Then in this terminal:

  export PS5_PAYLOAD_SDK="$SDK_DIR"
  source "\$HOME/.cargo/env"
  make build
  make run-client

EOF
