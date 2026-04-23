# PS5 Upload - Root Makefile
#
# Tree layout (after the 2.1 restructure):
#   payload/   — PS5 C payload (FreeBSD 11)
#   engine/    — Rust workspace: ftx2-proto, ps5upload-core, -engine HTTP service,
#                -lab CLI, -tests mock server, -bench
#   client/    — Tauri 2 desktop app, cross-platform (Linux/macOS/Windows, x64+arm64)
#   tests/     — root integration smoke + tests/lab/ (real-hardware shell scripts)
#   bench/     — golden workloads, baselines, perf-gate helpers
#   specs/     — FTX2 protocol, payload lifecycle, engine-UI test contract
#
# Retired in 2.1: app/ (browser server), shared/ (legacy JS modules), ui/ (empty
# scaffold), client/electron/ (Tauri replaces Electron). The 1.x C payload was
# already gone pre-rename.

JOBS ?= $(shell getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 4)
NPM_INSTALL ?= npm install --no-audit --no-fund
CARGO ?= cargo

PS5_HOST ?= 192.168.137.2
PS5_LOADER_PORT ?= 9021

# Default PS5 payload SDK location — override on the command line if installed
# elsewhere. `setup-payload` validates this path exists before any C build.
PS5_PAYLOAD_SDK ?= /opt/ps5-payload-sdk
export PS5_PAYLOAD_SDK

# On macOS the payload SDK's `prospero-clang` wrapper resolves `ld.lld` and
# `clang` through `prospero-llvm-config`. Homebrew keeps llvm keg-only (no
# PATH-exposed `llvm-config-NN`), AND llvm@21 ships without `ld.lld`, so we
# hard-code llvm@18 — the only Homebrew llvm known to include the full
# toolchain on this platform. Linux/WSL picks up `llvm-config-<N>` from apt
# naturally and doesn't need this override.
ifeq ($(shell uname -s),Darwin)
  LLVM_CONFIG ?= /opt/homebrew/opt/llvm@18/bin/llvm-config
  export LLVM_CONFIG
endif

PAYLOAD_DIR := payload
PAYLOAD_ELF := $(PAYLOAD_DIR)/ps5upload.elf
ENGINE_DIR  := engine
CLIENT_DIR  := client

.PHONY: all help
.PHONY: setup setup-engine setup-payload setup-client
.PHONY: build payload engine client _engine-release
.PHONY: test test-root test-engine test-payload test-client
.PHONY: clean clean-payload clean-engine clean-client
.PHONY: verify info install-hooks
.PHONY: run-engine run-client dev start
.PHONY: install-engine uninstall-engine
.PHONY: dist dist-win dist-mac dist-linux
.PHONY: send-payload gen-fixtures sweep validate validate-xl
.PHONY: sync-version sync-version-check

# Default target
all: build

#──────────────────────────────────────────────────────────────────────────────
# Help
#──────────────────────────────────────────────────────────────────────────────

help:
	@echo "PS5 Upload - Build System"
	@echo ""
	@echo "Quick start:"
	@echo "  1. make setup         - Check toolchains, install client deps"
	@echo "  2. make build         - Build payload ELF + Rust engine + client UI"
	@echo "  3. make send-payload  - Upload payload ELF to PS5 (PS5_HOST=$(PS5_HOST))"
	@echo "  4. make run-client    - Start the Tauri desktop app (spawns engine automatically)"
	@echo ""
	@echo "Validation:"
	@echo "  make validate         - Rebuild + send + smoke + full sweep + timestamped report"
	@echo "  make validate-xl      - Same as validate plus the 200k-file stress profile"
	@echo "  make sweep            - Run sweep against an already-loaded payload"
	@echo ""
	@echo "Build parts:"
	@echo "  make payload          - Build the PS5 payload in $(PAYLOAD_DIR)/"
	@echo "  make engine           - Build the Rust engine workspace"
	@echo "  make client           - Build the Tauri/React UI"
	@echo ""
	@echo "Testing:"
	@echo "  make test-engine      - cargo test --workspace"
	@echo "  make test-payload     - Validate $(PAYLOAD_ELF)"
	@echo "  make test-client      - Type-check + build client UI"
	@echo ""
	@echo "Version:"
	@echo "  make sync-version       - Sync downstream files from VERSION (canonical source)"
	@echo "  make sync-version-check - Fail if any downstream file drifts from VERSION"
	@echo ""
	@echo "Packaging:"
	@echo "  make dist             - Tauri bundle for the current OS"
	@echo "  make dist-win|mac|linux - Specific target platform"
	@echo ""
	@echo "Auto-launch (engine starts at OS login):"
	@echo "  make install-engine    - Register systemd/launchd/Task Scheduler job"
	@echo "  make uninstall-engine  - Remove the auto-launch registration"
	@echo ""
	@echo "Environment overrides (defaults shown):"
	@echo "  PS5_HOST=$(PS5_HOST)          PS5 IP address"
	@echo "  PS5_LOADER_PORT=$(PS5_LOADER_PORT)               PS5 payload loader port"
	@echo "  PS5_PAYLOAD_SDK=$(PS5_PAYLOAD_SDK)     PS5 C payload SDK install"
ifeq ($(shell uname -s),Darwin)
	@echo "  LLVM_CONFIG=$(LLVM_CONFIG)"
	@echo "                                Auto-set on macOS (needs Homebrew llvm@18)"
endif

#──────────────────────────────────────────────────────────────────────────────
# Setup
#──────────────────────────────────────────────────────────────────────────────

setup: setup-payload setup-engine setup-client
	@echo ""
	@echo "✓ Setup complete"
	@echo ""
	@echo "Suggested next steps:"
	@echo "  1. make build"
	@echo "  2. make run-client"
	@echo ""

setup-payload:
	@echo "Checking PS5 Payload SDK..."
	@if [ -z "$(PS5_PAYLOAD_SDK)" ]; then \
		echo ""; \
		echo "ERROR: PS5_PAYLOAD_SDK is not set."; \
		echo "Install the PS5 Payload SDK and export PS5_PAYLOAD_SDK=/opt/ps5-payload-sdk"; \
		echo ""; \
		exit 1; \
	fi
	@if [ ! -f "$(PS5_PAYLOAD_SDK)/toolchain/prospero.mk" ]; then \
		echo ""; \
		echo "ERROR: SDK files not found at $(PS5_PAYLOAD_SDK)"; \
		echo ""; \
		exit 1; \
	fi
	@echo "✓ PS5 SDK found: $(PS5_PAYLOAD_SDK)"

setup-engine:
	@echo "Checking Rust toolchain..."
	@command -v rustc >/dev/null 2>&1 || { \
		echo "ERROR: rustc is not installed."; \
		echo "Install Rust via https://rustup.rs"; \
		exit 1; \
	}
	@command -v $(CARGO) >/dev/null 2>&1 || { \
		echo "ERROR: cargo is not installed."; \
		echo "Install Rust via https://rustup.rs"; \
		exit 1; \
	}
	@echo "✓ Rust toolchain found: rustc $$(rustc --version), cargo $$($(CARGO) --version)"

setup-client:
	@echo "Checking client toolchain..."
	@command -v node >/dev/null 2>&1 || { echo "ERROR: Node.js is not installed."; exit 1; }
	@command -v npm >/dev/null 2>&1 || { echo "ERROR: npm is not installed."; exit 1; }
	@echo "✓ Node.js toolchain found: node $$(node --version), npm $$(npm --version)"
	@echo "Installing client dependencies..."
	@cd $(CLIENT_DIR) && $(NPM_INSTALL)
	@echo "✓ Client dependencies installed"

#──────────────────────────────────────────────────────────────────────────────
# Build
#──────────────────────────────────────────────────────────────────────────────

build: sync-version-check payload engine client
	@echo ""
	@echo "✓ Build complete"
	@echo ""
	@echo "Outputs:"
	@echo "  - $(PAYLOAD_ELF)"
	@echo "  - $(ENGINE_DIR)/target/"
	@echo "  - $(CLIENT_DIR)/dist/"
	@echo ""

# VERSION (repo root) is the canonical source of truth. `sync-version`
# rewrites downstream files (payload config.h macro, client package
# manifests, Tauri conf + Cargo.toml) from it; `sync-version-check`
# fails if any downstream file has drifted — wired into `build` so
# a desync gets caught before we ship an ELF + bundle with mismatched
# version strings.
sync-version:
	@node scripts/update-version.js

sync-version-check:
	@node scripts/update-version.js --check

payload: setup-payload
	@echo "Building PS5 payload..."
	@$(MAKE) -C $(PAYLOAD_DIR) -j$(JOBS)
	@echo "✓ Built $(PAYLOAD_ELF)"

send-payload: payload
	@echo "Sending payload to $(PS5_HOST):$(PS5_LOADER_PORT) ..."
	@if ! command -v nc >/dev/null 2>&1; then \
		echo "ERROR: nc (netcat) not found — install netcat"; exit 1; \
	fi
	@nc -w 1 "$(PS5_HOST)" "$(PS5_LOADER_PORT)" < "$(PAYLOAD_ELF)"
	@echo "✓ Payload sent — wait for PS5 notification before testing"

engine: setup-engine
	@echo "Building Rust engine workspace..."
	@cd $(ENGINE_DIR) && $(CARGO) build --workspace
	@echo "Building release ps5upload-engine (the binary Tauri spawns)..."
	@cd $(ENGINE_DIR) && $(CARGO) build --release -p ps5upload-engine
	@echo "✓ Engine workspace built"

# Internal helper: build the `ps5upload-engine` release binary, which the
# Tauri setup hook (client/src-tauri/src/engine.rs) spawns as a child of
# the window process. `cargo build --release` is itself incremental, so
# this is a cheap no-op when sources are unchanged — and a real rebuild
# the moment they aren't. (An earlier version guarded with
# `[ ! -x binary ]`, which stayed stale through code changes; removed.)
_engine-release: setup-engine
	@cd $(ENGINE_DIR) && $(CARGO) build --release -p ps5upload-engine

client: setup-client
	@echo "Building client UI..."
	@cd $(CLIENT_DIR) && npm run build:vite
	@echo "✓ Client UI built"

#──────────────────────────────────────────────────────────────────────────────
# Benchmarking + validation
#
# `make validate` is the single-command flow for "we are still progressing":
# rebuild payload, send to PS5, wait for ready, run smoke + full sweep, write
# a timestamped report under bench/reports/ and print a summary table.
# `make validate-xl` adds the 200k-file stress profile.
#──────────────────────────────────────────────────────────────────────────────

gen-fixtures:
	@echo "Generating benchmark fixtures under bench/fixtures/..."
	@node scripts/gen-fixtures.mjs

sweep:
	@echo "Running FTX2 sweep against live PS5 at $(PS5_HOST):9113 ..."
	@node bench/run-ftx2-sweep.mjs --spawn-engine --gen-fixtures

# Wait for the payload's runtime port to accept connections after send.
# Retries 15×/2s = 30s ceiling; exits non-zero if the port never opens.
_wait-payload-ready:
	@echo "Waiting for PS5 runtime port 9113 ..."
	@i=0; while [ $$i -lt 15 ]; do \
		if nc -z -w 1 "$(PS5_HOST)" 9113 >/dev/null 2>&1; then \
			echo "✓ runtime port open"; exit 0; \
		fi; \
		i=$$((i+1)); sleep 2; \
	done; \
	echo "ERROR: PS5 runtime port 9113 did not open within 30s"; exit 1

validate: send-payload _wait-payload-ready
	@echo ""
	@echo "── Running smoke suite ────────────────────────────────"
	@npm run --silent smoke:hardware
	@echo ""
	@echo "── Running sweep (default profiles) ────────────────────"
	@node bench/run-ftx2-sweep.mjs --spawn-engine --gen-fixtures
	@echo ""
	@echo "✓ validate complete — see bench/reports/ for the full report"

validate-xl: send-payload _wait-payload-ready
	@echo ""
	@echo "── Running smoke suite ────────────────────────────────"
	@npm run --silent smoke:hardware
	@echo ""
	@echo "── Running sweep (INCLUDING XL 200k-file stress) ───────"
	@node bench/run-ftx2-sweep.mjs --spawn-engine --gen-fixtures --xl
	@echo ""
	@echo "✓ validate-xl complete — see bench/reports/ for the full report"

#──────────────────────────────────────────────────────────────────────────────
# Distribution — all drive `tauri build`, producing ~14 MB .app/.AppImage/.msi
# instead of Electron's ~300 MB bundles. The Rust engine binary is bundled as
# a Tauri resource and discovered at runtime by client/src-tauri/src/engine.rs.
# `payload` and `_engine-release` run first so the bundle includes a current
# ELF and a freshly-built engine.
#──────────────────────────────────────────────────────────────────────────────

dist: payload _engine-release setup-client
	@echo "Building desktop distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist
	@echo "✓ Distribution packages built: $(CLIENT_DIR)/src-tauri/target/release/bundle/"

dist-win: payload _engine-release setup-client
	@echo "Building Windows distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:win
	@echo "✓ Windows packages built: $(CLIENT_DIR)/src-tauri/target/release/bundle/"

dist-mac: payload _engine-release setup-client
	@echo "Building macOS distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:mac
	@echo "✓ macOS packages built: $(CLIENT_DIR)/src-tauri/target/release/bundle/"

dist-linux: payload _engine-release setup-client
	@echo "Building Linux distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:linux
	@echo "✓ Linux packages built: $(CLIENT_DIR)/src-tauri/target/release/bundle/"

#──────────────────────────────────────────────────────────────────────────────
# Testing
#──────────────────────────────────────────────────────────────────────────────

test: test-root test-engine test-payload test-client
	@echo ""
	@echo "✓ All tests passed"
	@echo ""

# Root-level tests: syntax-check the node scripts that back the bench + smoke
# harnesses. Hardware smoke coverage lives in `make sweep` / `make validate`.
test-root:
	@echo "Syntax-checking root node scripts..."
	@node --check tests/smoke-hardware.mjs
	@node --check bench/run-ftx2-sweep.mjs
	@node --check bench/run-ftx2-upload.mjs
	@node --check bench/check-ftx2-baseline.mjs
	@node --check scripts/gen-fixtures.mjs
	@echo "✓ Root scripts valid"

test-engine: setup-engine
	@echo "Running Rust engine tests..."
	@cd $(ENGINE_DIR) && $(CARGO) test --workspace
	@echo "✓ Engine tests passed"

test-payload: payload
	@echo "Validating payload binary..."
	@if [ ! -f "$(PAYLOAD_ELF)" ]; then \
		echo "ERROR: $(PAYLOAD_ELF) not found."; \
		exit 1; \
	fi
	@file $(PAYLOAD_ELF) | grep -q "ELF" || { \
		echo "ERROR: $(PAYLOAD_ELF) is not a valid ELF file."; \
		exit 1; \
	}
	@echo "✓ $(PAYLOAD_ELF) exists and is an ELF binary"

test-client: setup-client
	@echo "Testing client build..."
	@cd $(CLIENT_DIR) && npm run build:vite
	@if [ ! -d "$(CLIENT_DIR)/dist" ]; then \
		echo "ERROR: client build failed - dist directory not found."; \
		exit 1; \
	fi
	@echo "✓ Client checks passed"

verify: test
	@echo "Running client packaging validation (Tauri)..."
	@cd $(CLIENT_DIR) && npm run build
	@echo "✓ Client packaging validation passed"

#──────────────────────────────────────────────────────────────────────────────
# Auto-launch (engine starts on OS login)
#──────────────────────────────────────────────────────────────────────────────

install-engine: setup-engine
	@echo "Installing engine auto-launch for this platform..."
	@case "$$(uname -s)" in \
		Linux)  bash scripts/autolaunch/install-linux.sh ;; \
		Darwin) bash scripts/autolaunch/install-macos.sh ;; \
		*)      echo "Use scripts/autolaunch/install-windows.ps1 on Windows" ;; \
	esac

uninstall-engine:
	@case "$$(uname -s)" in \
		Linux)  bash scripts/autolaunch/uninstall-linux.sh ;; \
		Darwin) bash scripts/autolaunch/uninstall-macos.sh ;; \
		*)      echo "Use scripts/autolaunch/uninstall-windows.ps1 on Windows" ;; \
	esac

#──────────────────────────────────────────────────────────────────────────────
# Run
#──────────────────────────────────────────────────────────────────────────────

run-engine: setup-engine
	@echo "Starting ps5upload-engine (standalone; kept for diagnostics —"
	@echo "in normal use the Tauri client spawns the engine automatically)..."
	@cd $(ENGINE_DIR) && $(CARGO) run --release -p ps5upload-engine

# `run-client` is the one-command dev flow. `npm run dev` in client/ is wired
# to `tauri dev`, which starts Vite + the Rust main process. The Rust setup
# hook in client/src-tauri/src/engine.rs spawns ps5upload-engine as a child
# of the window process; killing the window tears the engine down cleanly.
run-client: setup-client _engine-release _kill-stale-client
	@echo "Starting PS5 Upload client (Tauri + Rust + Vite)..."
	@if [ -z "$$DISPLAY" ] && [ -z "$$WAYLAND_DISPLAY" ] && [ "$$(uname -s)" = "Linux" ]; then \
		if command -v xvfb-run >/dev/null 2>&1; then \
			echo "No display detected, using xvfb-run..."; \
			cd $(CLIENT_DIR) && xvfb-run --auto-servernum npm run dev; \
		else \
			echo "ERROR: No display server found ($$DISPLAY / $$WAYLAND_DISPLAY not set)."; \
			echo "  Install xvfb to run headless: sudo apt-get install -y xvfb"; \
			exit 1; \
		fi; \
	else \
		cd $(CLIENT_DIR) && npm run dev; \
	fi

# Kill any stale ps5upload processes + Vite on :1420 that belongs to us.
# Narrow enough not to touch unrelated node servers. Invoked as a dep of
# run-client so repeated launches after a mis-terminated session don't
# fail with "Port 1420 already in use".
.PHONY: _kill-stale-client
_kill-stale-client:
	@pkill -f ps5upload-desktop 2>/dev/null || true
	@pkill -f ps5upload-engine 2>/dev/null || true
	@pid=$$(lsof -ti :1420 2>/dev/null); \
	if [ -n "$$pid" ]; then \
		if ps -o command= -p $$pid 2>/dev/null | grep -q 'ps5upload.*node_modules.*vite'; then \
			kill -9 $$pid 2>/dev/null; \
			echo "✓ killed stale Vite on :1420 (pid $$pid)"; \
		fi; \
	fi
	@sleep 1

#──────────────────────────────────────────────────────────────────────────────
# Clean
#──────────────────────────────────────────────────────────────────────────────

clean: clean-payload clean-engine clean-client
	@echo "✓ All clean"

clean-payload:
	@echo "Cleaning payload artifacts..."
	@if [ -d "$(PAYLOAD_DIR)" ]; then $(MAKE) -C $(PAYLOAD_DIR) clean; fi
	@echo "✓ Payload cleaned"

clean-engine:
	@echo "Cleaning Rust engine artifacts..."
	@if [ -d "$(ENGINE_DIR)" ]; then cd $(ENGINE_DIR) && $(CARGO) clean; fi
	@echo "✓ Engine cleaned"

clean-client:
	@echo "Cleaning client artifacts..."
	@rm -rf $(CLIENT_DIR)/dist
	@rm -rf $(CLIENT_DIR)/release
	@rm -rf $(CLIENT_DIR)/node_modules/.vite
	@rm -rf $(CLIENT_DIR)/src-tauri/target
	@echo "✓ Client cleaned"

#──────────────────────────────────────────────────────────────────────────────
# Development / Utility
#──────────────────────────────────────────────────────────────────────────────

info:
	@echo "PS5 Upload - Build Information"
	@echo ""
	@echo "Environment:"
	@echo "  PS5_PAYLOAD_SDK: $(PS5_PAYLOAD_SDK)"
	@echo "  Rust: $$(rustc --version 2>/dev/null || echo 'Not found')"
	@echo "  Cargo: $$($(CARGO) --version 2>/dev/null || echo 'Not found')"
	@echo "  Node: $$(node --version 2>/dev/null || echo 'Not found')"
	@echo "  npm: $$(npm --version 2>/dev/null || echo 'Not found')"
	@echo "  Make: $$(make --version | head -1)"
	@echo ""
	@echo "Artifacts:"
	@echo "  Payload:"
	@[ -f "$(PAYLOAD_ELF)" ] && echo "    ✓ $(PAYLOAD_ELF)" || echo "    ✗ missing (run: make payload)"
	@echo "  Engine target dir:"
	@[ -d "$(ENGINE_DIR)/target" ] && echo "    ✓ $(ENGINE_DIR)/target" || echo "    ✗ missing (run: make engine)"
	@echo "  Client build:"
	@[ -d "$(CLIENT_DIR)/dist" ] && echo "    ✓ $(CLIENT_DIR)/dist" || echo "    ✗ missing (run: make client)"
	@echo "  Tauri bundles:"
	@[ -d "$(CLIENT_DIR)/src-tauri/target/release/bundle" ] && echo "    ✓ $(CLIENT_DIR)/src-tauri/target/release/bundle" || echo "    ✗ missing (run: make dist)"
	@echo ""

install-hooks:
	@echo "Installing git hooks..."
	@mkdir -p .git/hooks
	@echo '#!/bin/bash' > .git/hooks/pre-commit
	@echo 'echo "Checking documentation..."' >> .git/hooks/pre-commit
	@echo 'git diff --cached --name-only | grep -E "\.(c|rs|py|h|md)$$" > /dev/null' >> .git/hooks/pre-commit
	@echo 'if [ $$? -eq 0 ]; then' >> .git/hooks/pre-commit
	@echo '  echo "Code or docs changed. Keep .progress and specs in sync."' >> .git/hooks/pre-commit
	@echo 'fi' >> .git/hooks/pre-commit
	@chmod +x .git/hooks/pre-commit
	@echo "✓ Git hooks installed"

# Convenience aliases
dev: run-client
start: run-client

release-post:
	@./scripts/release-posts.sh
