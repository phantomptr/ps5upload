# PS5 Upload - Root Makefile
#
# Tree layout (current):
#   payload/   — PS5 C payload (FreeBSD 11)
#   engine/    — Rust workspace: ava1, ps5upload-core, -ava1, -engine HTTP service,
#                -lab CLI, -tests, -pkg
#   client/    — Tauri 2 desktop app, cross-platform (Linux/macOS/Windows, x64+arm64)
#   tests/     — root integration smoke + tests/lab/ (real-hardware shell scripts)
#   bench/     — hardware sweep, golden workloads and profiles
#   scripts/   — install + dev helpers (one per OS, plus shared mjs utilities)

JOBS ?= $(shell getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 4)
# `npm ci` — NOT `npm install`. The lockfile carries `libc` fields that select
# musl vs glibc native binaries for the Alpine-based web UI image
# (engine/Dockerfile.webui, node:26-alpine). npm 11 writes them; npm 10 does
# not know the field and DELETES it on every `npm install`. Since this target
# runs on every build, an older npm silently stripped that metadata and it had
# to be hand-reverted before each release. `npm ci` installs strictly FROM the
# lockfile and never rewrites it, which is also what CI does.
NPM_INSTALL ?= npm ci --no-audit --no-fund
CARGO ?= cargo

# AVA1 is the third_party/ava1 submodule (github.com/phantomptr/ava1). A clone made without
# --recurse-submodules, or a pull across the change that made it one, leaves it empty: fill it
# rather than fail later with missing-file errors.
ifeq ($(wildcard third_party/ava1/Cargo.toml),)
$(info third_party/ava1 is empty: git submodule update --init third_party/ava1)
$(shell git submodule update --init third_party/ava1 >&2)
endif

PS5_HOST ?= 192.168.137.2
PS5_LOADER_PORT ?= 9021

# Default PS5 payload SDK location — override on the command line if installed
# elsewhere. `setup-payload` validates this path exists before any C build.
PS5_PAYLOAD_SDK ?= /opt/ps5-payload-sdk
export PS5_PAYLOAD_SDK
PS5_SDK_TAG := $(shell sed -n 's/^PS5_SDK_TAG=//p' scripts/ps5-sdk.env 2>/dev/null)

# On macOS the payload SDK's `prospero-clang` wrapper resolves `ld.lld`
# and `clang` through `prospero-llvm-config`. SDK v0.45 supports LLVM 16–23.
# Homebrew exposes the current major as `llvm` (not `llvm@23`), so discover
# its real prefix and retain versioned fallbacks for older installations.
# Linux/WSL picks up `llvm-config-<N>` from apt naturally.
ifeq ($(shell uname -s),Darwin)
  MACOS_LLVM_CONFIG := $(shell \
    if command -v brew >/dev/null 2>&1; then \
      for formula in llvm llvm@23 llvm@22 llvm@18; do \
        prefix="$$(brew --prefix "$$formula" 2>/dev/null || true)"; \
        if [ -n "$$prefix" ] && [ -x "$$prefix/bin/llvm-config" ]; then \
          printf '%s\n' "$$prefix/bin/llvm-config"; \
          break; \
        fi; \
      done; \
    fi)
  LLVM_CONFIG ?= $(MACOS_LLVM_CONFIG)
  export LLVM_CONFIG
endif

PAYLOAD_DIR := payload
PAYLOAD_ELF := $(PAYLOAD_DIR)/ps5upload.elf
# The PS5Upload installer daemon (our own; :9115, FW 11+ pkg installs).
# Built from the same SDK; the engine auto-sends it to the loader port
# (:9021) as a companion image when :9115 isn't already listening.
INSTALLER_DIR := $(PAYLOAD_DIR)/installer
INSTALLER_ELF := $(INSTALLER_DIR)/ps5upload-installer.elf
ENGINE_DIR  := engine
CLIENT_DIR  := client

# Android (Tauri mobile). The engine is linked in-process (no sidecar);
# only the PS5 payload is bundled. Needs the Android SDK (ANDROID_HOME),
# a JDK 17, an NDK, and the aarch64-linux-android Rust target via rustup
# (Homebrew `rust` ships only the host std and can't cross-compile).
# JDK/NDK are auto-detected where possible — override any on the CLI.
ANDROID_TARGET     ?= aarch64
ANDROID_RUST_TARGET ?= aarch64-linux-android
# macOS resolves a JDK 17 via java_home; elsewhere fall back to $JAVA_HOME.
ANDROID_JAVA_HOME  ?= $(shell /usr/libexec/java_home -v 17 2>/dev/null || echo "$$JAVA_HOME")
# Newest NDK installed under the SDK.
ANDROID_NDK_HOME   ?= $(shell ls -d "$(ANDROID_HOME)"/ndk/* 2>/dev/null | sort -V | tail -1)
ANDROID_GEN_DIR    := $(CLIENT_DIR)/src-tauri/gen/android
ANDROID_APK        := $(ANDROID_GEN_DIR)/app/build/outputs/apk/universal/debug/app-universal-debug.apk
# Package id (matches tauri.conf.json `identifier`). Used to recover from a
# signature-mismatched prior install during the convenience deploy step.
ANDROID_APP_ID     := com.phantomptr.ps5upload
# rustup's *real* toolchain bin (not the ~/.cargo/bin shim). The Android std
# lives only here; Homebrew `rust` ships only the host std. We pin PATH to this
# dir for the android recipes so cross-compilation works even when Homebrew's
# cargo precedes rustup on PATH (otherwise `cargo --target aarch64-linux-android`
# picks Homebrew's cargo and fails with "can't find crate for `core`/`std`").
RUSTUP_CARGO_BIN := $(shell dirname "$$(rustup which cargo 2>/dev/null)" 2>/dev/null)
# Env preamble shared by the android recipes.
# Debug APKs keep line tables only. With full debug info the app library was
# 558 MB, stored uncompressed, so the APK passed 1.1 GB and `adb install`
# failed on an emulator with INSTALL_FAILED_INSUFFICIENT_STORAGE. Line tables
# keep function names and file:line in backtraces. Set here, not in
# Cargo.toml, so the desktop `tauri dev` build keeps its full debug info.
ANDROID_ENV = JAVA_HOME="$(ANDROID_JAVA_HOME)" ANDROID_HOME="$(ANDROID_HOME)" NDK_HOME="$(ANDROID_NDK_HOME)" CARGO_PROFILE_DEV_DEBUG=line-tables-only $(if $(RUSTUP_CARGO_BIN),PATH="$(RUSTUP_CARGO_BIN):$$PATH")
# adb from the SDK (falls back to PATH if a device-only adb is installed).
ADB ?= $(ANDROID_HOME)/platform-tools/adb

.PHONY: all help
.PHONY: install install-ubuntu install-macos install-windows update-sdk
.PHONY: setup setup-engine setup-payload setup-client
.PHONY: build payload installer engine client _engine-release _payload-if-ready _android-build-if-ready
.PHONY: test test-root test-engine test-engine-coverage test-desktop test-payload test-client test-client-coverage
.PHONY: lint lint-scripts lint-client audit-scripts coverage coverage-engine coverage-client
.PHONY: quality quality-full quality-hardware ci ci-full release-check
.PHONY: clean clean-payload clean-engine clean-client
.PHONY: verify info install-hooks
.PHONY: test-ava1 test-ava1-sanitize ava1-app-gen ava1-version-check check-no-ftx2
.PHONY: run-engine run-client dev start _check-tauri-system-deps
.PHONY: install-engine uninstall-engine
.PHONY: dist dist-win dist-win-arm dist-mac dist-mac-x64 dist-linux dist-linux-arm
.PHONY: ios-init ios-build run-ios android-deps android-init android-build android-deploy _android-install-if-device run-android run-android-background
.PHONY: emu-create emu-start emu-stop emu-status emu-test
.PHONY: send-payload gen-fixtures sweep validate validate-xl
.PHONY: sync-version sync-version-check
.PHONY: docker-engine docker-engine-run

# Default target
all: build

#──────────────────────────────────────────────────────────────────────────────
# Help
#──────────────────────────────────────────────────────────────────────────────

help:
	@echo "PS5 Upload - Build System"
	@echo ""
	@echo "First-time setup (fresh machine):"
	@echo "  make install          - Auto-detect host OS and install all dev deps"
	@echo "  make install-ubuntu   - Ubuntu/Debian/WSL2: apt + rustup + node + PS5 SDK $(PS5_SDK_TAG)"
	@echo "  make install-macos    - macOS: brew + rustup + LLVM + PS5 SDK $(PS5_SDK_TAG)"
	@echo "  make install-windows  - Windows 11: winget + rustup + VS Build Tools + PS5 SDK $(PS5_SDK_TAG)"
	@echo "  make update-sdk       - Move the PS5 SDK pin to the latest release and install it"
	@echo "                          (SDK_TAG=vX.Y for a specific one; SDK_CHECK=1 to only check)"
	@echo ""
	@echo "Quick start (after install):"
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
	@echo "  make lint             - script syntax + frontend ESLint"
	@echo "  make lint-scripts     - Node/Bash/Python/PowerShell syntax checks"
	@echo "  make audit-scripts    - Inventory script/test utilities"
	@echo "  make security         - Scan dependencies and git history (cargo/npm audit, gitleaks)"
	@echo "  make quality          - Full non-hardware validation gate"
	@echo "  make quality-full     - quality + payload validation"
	@echo "  make quality-hardware - quality + live PS5 validate"
	@echo "  make release-check    - Before a release: quality + Playwright e2e side by side, then"
	@echo "                          test-payload (BUILD_LOCK=<file> serializes the heavy builds)"
	@echo "  make coverage         - Rust + frontend coverage reports"
	@echo "  make coverage-engine  - Rust coverage report only"
	@echo "  make coverage-client  - Frontend coverage report only"
	@echo "  make test-engine      - cargo test --workspace (C interop serial)"
	@echo "  make test-desktop     - Tauri Rust cargo check/clippy/test"
	@echo "  make test-ava1        - AVA1 Rust + C conformance and interop tests"
	@echo "  make test-payload     - Validate $(PAYLOAD_ELF)"
	@echo "  make test-client      - Type-check + lint + unit tests + build client UI"
	@echo ""
	@echo "Version:"
	@echo "  make sync-version       - Sync downstream files from VERSION (canonical source)"
	@echo "  make sync-version-check - Fail if any downstream file drifts from VERSION"
	@echo ""
	@echo "Packaging:"
	@echo "  make dist             - Tauri bundle for the current OS"
	@echo "  make dist-win|dist-win-arm"
	@echo "  make dist-mac|dist-mac-x64"
	@echo "  make dist-linux|dist-linux-arm"
	@echo ""
	@echo "Android (Tauri mobile — needs Android SDK + JDK 17 + NDK + rustup):"
	@echo "  make run-android      - Build + run with live reload; emulator in the FOREGROUND (window)"
	@echo "  make run-android-background - Same, but the emulator runs headless (no window)"
	@echo "      Both reuse a running emulator instead of booting another; run-android"
	@echo "      brings its window to the front (and reopens a headless one with a window)."
	@echo "  make android-deploy   - Build APK + install/update it on connected device(s)"
	@echo "  make android-build    - Build a debug APK (no device needed)"
	@echo "  make android-init     - One-time: scaffold src-tauri/gen/android"
	@echo "  make ios-build        - Build an unsigned iPhone .ipa for sideloading (macOS + Xcode)"
	@echo "  make run-ios          - Run the app in the iPhone Simulator with live reload (macOS + Xcode)"
	@echo "  make android-deps     - Check the Android toolchain is ready"
	@echo "  make emu-test         - Build + run the app on a headless emulator, with a screenshot"
	@echo "  make emu-start        - Boot the test emulator (headless); emu-stop to shut it down"
	@echo "  make emu-status       - Show AVDs, whether one is running, and where artifacts go"
	@echo "      emu-test leaves the emulator up for fast re-runs; PS5UPLOAD_EMU_TEARDOWN=1 stops it"
	@echo ""
	@echo "Auto-launch (engine starts at OS login):"
	@echo "  make install-engine    - Register systemd/launchd/Task Scheduler job"
	@echo "  make uninstall-engine  - Remove the auto-launch registration"
	@echo ""
	@echo "Docker (self-hosted engine; CI publishes the same image to GHCR):"
	@echo "  make docker-engine      - Build the engine image locally (scratch + static binary)"
	@echo "  make docker-engine-run  - Run it (PS5_HOST=$(PS5_HOST)); exposes :19113"
	@echo ""
	@echo "Environment overrides (defaults shown):"
	@echo "  PS5_HOST=$(PS5_HOST)          PS5 IP address"
	@echo "  PS5_LOADER_PORT=$(PS5_LOADER_PORT)               PS5 payload loader port"
	@echo "  PS5_PAYLOAD_SDK=$(PS5_PAYLOAD_SDK)     PS5 C payload SDK install"
ifeq ($(shell uname -s),Darwin)
	@echo "  LLVM_CONFIG=$(LLVM_CONFIG)"
	@echo "                                Auto-set on macOS (needs Homebrew llvm)"
endif

#──────────────────────────────────────────────────────────────────────────────
# Install — fresh-machine bootstrap (apt/brew/winget + rustup + Node + PS5 SDK)
#
# `setup` only *checks* toolchains and runs `npm install`; `install` actually
# installs everything from scratch. The per-OS scripts live in `scripts/` and
# are idempotent — each step skips if already satisfied — so re-running them
# after a partial setup is safe.
#──────────────────────────────────────────────────────────────────────────────

install:
	@os="$$(uname -s)"; \
	case "$$os" in \
	  Linux*)                $(MAKE) install-ubuntu ;; \
	  Darwin*)               $(MAKE) install-macos ;; \
	  MINGW*|MSYS*|CYGWIN*)  $(MAKE) install-windows ;; \
	  *) echo "Unsupported host OS: $$os"; \
	     echo "Run install-ubuntu / install-macos / install-windows directly."; \
	     exit 1 ;; \
	esac

install-ubuntu:
	@bash scripts/install-ubuntu.sh

install-macos:
	@bash scripts/install-macos.sh

# On Windows the script is invoked via PowerShell. Tries pwsh (PowerShell 7+)
# first, then falls back to legacy Windows PowerShell. Run from any shell where
# `powershell.exe` or `pwsh` is on PATH (Git Bash, MSYS2, native PowerShell).
install-windows:
	@if command -v pwsh >/dev/null 2>&1; then \
	  pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/install-windows.ps1; \
	elif command -v powershell.exe >/dev/null 2>&1; then \
	  powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/install-windows.ps1; \
	else \
	  echo "ERROR: neither pwsh nor powershell.exe on PATH."; \
	  echo "  Run scripts/install-windows.ps1 directly from a PowerShell prompt."; \
	  exit 1; \
	fi

# Bump the PS5 Payload SDK pin (scripts/ps5-sdk.env) to the latest upstream
# release, or SDK_TAG, and install it into $(PS5_PAYLOAD_SDK); the old SDK is
# kept as a backup. SDK_CHECK=1 only reports whether a newer release exists.
update-sdk:
	@PS5_SDK_INSTALL_DIR="$(PS5_PAYLOAD_SDK)" bash scripts/update-ps5-sdk.sh \
	  $(if $(SDK_TAG),--tag $(SDK_TAG)) $(if $(SDK_CHECK),--check)

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
ifeq ($(shell uname -s),Darwin)
	@if [ -z "$(LLVM_CONFIG)" ] || [ ! -x "$(LLVM_CONFIG)" ]; then \
		echo ""; \
		echo "ERROR: Homebrew llvm-config was not found."; \
		echo "Install it with: brew install llvm"; \
		echo "Or export LLVM_CONFIG=/path/to/llvm-config"; \
		echo ""; \
		exit 1; \
	fi
	@echo "✓ LLVM found: $(LLVM_CONFIG) ($$($(LLVM_CONFIG) --version))"
endif

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
	@# Reinstall only when the lockfile (or Node) changed since the last install. npm ci
	@# deletes node_modules first — including Vite's dependency cache — so running it on
	@# every make broke any dev server already running (make run-client beside
	@# run-android or emu-test): its screens failed with "Importing a module script failed".
	@cd $(CLIENT_DIR) && want="$$(node -e "const c=require('crypto'),f=require('fs');process.stdout.write(c.createHash('sha256').update(f.readFileSync('package-lock.json')).update(process.version).digest('hex'))")"; \
	if [ -f node_modules/.ps5upload-installed ] && [ "$$(cat node_modules/.ps5upload-installed)" = "$$want" ]; then \
		echo "✓ Client dependencies already match package-lock.json"; \
	else \
		echo "Installing client dependencies..." && $(NPM_INSTALL) && printf '%s' "$$want" > node_modules/.ps5upload-installed && \
		echo "✓ Client dependencies installed"; \
	fi

#──────────────────────────────────────────────────────────────────────────────
# Build
#──────────────────────────────────────────────────────────────────────────────

# Build the Android APK only when the toolchain is configured; otherwise skip
# with a notice so `make build` still works on machines without the Android
# SDK/NDK/JDK 17 (desktop-only contributors, CI desktop jobs). Uses the same
# vars `android-deps` checks. `android-build` self-depends on `payload`, so the
# embedded ELF is always current.
_android-build-if-ready:
	@if command -v rustup >/dev/null 2>&1 && [ -n "$(ANDROID_HOME)" ] && [ -n "$(ANDROID_NDK_HOME)" ] && [ -n "$(ANDROID_JAVA_HOME)" ]; then \
		echo "Android toolchain detected — building/refreshing the APK..."; \
		$(MAKE) android-build && $(MAKE) _android-install-if-device; \
	else \
		echo "⚠ Skipping Android APK build — toolchain not configured."; \
		echo "  Need rustup + ANDROID_HOME + an NDK + JDK 17. Run 'make android-deps' to see what's missing."; \
	fi

# Install the freshly-built universal debug APK onto every connected device
# (`adb install -r` updates in place, keeping app data). This is the step that
# actually refreshes the app ON the phone after a build. No-op with a notice
# when no device is attached — so it's safe in CI and on dev machines with no
# phone plugged in.
_android-install-if-device:
	@apk="$(ANDROID_APK)"; \
	adb="$(ADB)"; command -v "$$adb" >/dev/null 2>&1 || adb="adb"; \
	if [ ! -f "$$apk" ]; then echo "⚠ No APK at $$apk — run 'make android-build' first."; exit 0; fi; \
	if ! command -v "$$adb" >/dev/null 2>&1; then echo "⚠ adb not found — APK is at $$apk."; exit 0; fi; \
	devs=$$("$$adb" devices 2>/dev/null | awk 'NR>1 && $$2=="device"{print $$1}'); \
	if [ -z "$$devs" ]; then \
		echo "⚠ No Android device connected — APK is at $$apk."; \
		echo "  Plug in a device (USB debugging on), then: $$adb install -r \"$$apk\""; \
	else \
		failed=0; \
		for d in $$devs; do \
			echo "Updating the app on device $$d (adb install -r)..."; \
			out=$$("$$adb" -s "$$d" install -r "$$apk" 2>&1); \
			if [ $$? -eq 0 ] && ! printf '%s' "$$out" | grep -qi 'Failure'; then \
				echo "✓ App updated on $$d"; \
			elif printf '%s' "$$out" | grep -qi 'UPDATE_INCOMPATIBLE\|signatures do not match'; then \
				echo "⚠ Existing install on $$d was signed with a different key — reinstalling fresh (its app data is cleared)..."; \
				"$$adb" -s "$$d" uninstall "$(ANDROID_APP_ID)" >/dev/null 2>&1 || true; \
				if "$$adb" -s "$$d" install "$$apk" >/dev/null 2>&1; then \
					echo "✓ App reinstalled on $$d"; \
				else \
					echo "⚠ Could not install on $$d — APK is at $$apk (install manually: $$adb -s $$d install -r \"$$apk\")."; \
					failed=1; \
				fi; \
			elif printf '%s' "$$out" | grep -qi 'INSUFFICIENT_STORAGE' && case "$$d" in emulator-*) true;; *) false;; esac; then \
				echo "⚠ $$d is out of space — removing the old install (an emulator's app data is disposable) and retrying..."; \
				"$$adb" -s "$$d" uninstall "$(ANDROID_APP_ID)" >/dev/null 2>&1 || true; \
				if out=$$("$$adb" -s "$$d" install "$$apk" 2>&1) && ! printf '%s' "$$out" | grep -qi 'Failure'; then \
					echo "✓ App reinstalled on $$d"; \
				else \
					echo "⚠ Still could not install on $$d — free space on it (or wipe it: emulator -avd <name> -wipe-data):"; \
					printf '%s\n' "$$out" | sed 's/^/    /'; \
					failed=1; \
				fi; \
			else \
				echo "⚠ adb install failed on $$d (build is fine — APK is at $$apk):"; \
				printf '%s\n' "$$out" | sed 's/^/    /'; \
				if printf '%s' "$$out" | grep -qi 'INSUFFICIENT_STORAGE'; then \
					echo "  The device is out of space. Free some, then: $$adb -s $$d install -r \"$$apk\""; \
				fi; \
				failed=1; \
			fi; \
		done; \
		if [ $$failed -eq 0 ]; then echo "✓ Android build deployed to connected device(s)."; \
		else echo "⚠ Android build NOT deployed to every device — see above."; fi; \
	fi

# Build the Android APK and install/update it on the connected device(s) in
# one step — the simplest "push my latest changes to the phone".
# The install step reports its own outcome: it used to be followed by an
# unconditional "✓ deployed", even straight after a failed install.
android-deploy: android-build _android-install-if-device

# Rebuild the payload only when the PS5 SDK is configured; otherwise skip so a
# frontend-only `make run-client` keeps working without the SDK. The shell
# embeds whatever ps5upload.elf.gz is on disk (build.rs re-embeds on change).
_payload-if-ready:
	@if [ -n "$(PS5_PAYLOAD_SDK)" ] && [ -f "$(PS5_PAYLOAD_SDK)/toolchain/prospero.mk" ]; then \
		$(MAKE) payload; \
	else \
		echo "⚠ Skipping payload rebuild (PS5_PAYLOAD_SDK not set) — using existing $(PAYLOAD_ELF) if present."; \
	fi

# Build only the release engine binary (the binary Tauri spawns). Skips the
# dev-profile workspace build that the standalone `engine` target does —
# `make build` only needs the release binary, not the dev workspace.
build: sync-version-check payload _engine-release client
	@$(MAKE) _android-build-if-ready
	@echo ""
	@echo "✓ Build complete"
	@echo ""
	@echo "Outputs:"
	@echo "  - $(PAYLOAD_ELF)"
	@echo "  - $(ENGINE_DIR)/target/"
	@echo "  - $(CLIENT_DIR)/dist/"
	@echo "  - $(ANDROID_APK) (when the Android toolchain is present)"
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

payload: setup-payload installer
	@echo "Building PS5 payload..."
	@$(MAKE) -C $(PAYLOAD_DIR) -j$(JOBS)
	@echo "✓ Built $(PAYLOAD_ELF)"

# PS5Upload installer daemon (our own; supersedes the DPI daemon).
installer: setup-payload
	@echo "Building PS5Upload installer daemon..."
	@$(MAKE) -C $(INSTALLER_DIR) -j$(JOBS)
	@echo "✓ Built $(INSTALLER_ELF)"

# The patched elfldr the engine sends to replace a console's stock one (see
# third_party/elfldr/README.md). Its ELF is checked in, so a build without the
# SDK still embeds it; rebuild after changing the vendored sources.
.PHONY: elfldr
elfldr: setup-payload
	@echo "Building the patched elfldr..."
	@$(MAKE) -C third_party/elfldr
	@rm -f third_party/elfldr/*.o
	@echo "✓ Built third_party/elfldr/elfldr-ps5.elf"

send-payload: payload
	@echo "Sending payload to $(PS5_HOST):$(PS5_LOADER_PORT) ..."
	@if ! command -v python3 >/dev/null 2>&1; then \
		echo "ERROR: python3 not found — required for bounded payload upload"; exit 1; \
	fi
	@python3 -c 'import pathlib, socket, sys; host, port, payload_path = sys.argv[1], int(sys.argv[2]), pathlib.Path(sys.argv[3]); payload = payload_path.read_bytes(); sock = socket.create_connection((host, port), timeout=5); sock.settimeout(5); sock.sendall(payload); sock.shutdown(socket.SHUT_WR); sock.close()' "$(PS5_HOST)" "$(PS5_LOADER_PORT)" "$(PAYLOAD_ELF)"
	@echo "✓ Payload sent — wait for PS5 notification before testing"

engine: setup-engine
	@echo "Building Rust engine workspace..."
	@cd $(ENGINE_DIR) && $(CARGO) build --workspace
	@echo "Building release ps5upload-engine + ps5upload-unrar (the binaries Tauri embeds)..."
	@cd $(ENGINE_DIR) && $(CARGO) build --release -p ps5upload-engine -p ps5upload-unrar
	@echo "✓ Engine workspace built"

# Internal helper: build the `ps5upload-engine` release binary, which the
# Tauri setup hook (client/src-tauri/src/engine.rs) spawns as a child of
# the window process. `cargo build --release` is itself incremental, so
# this is a cheap no-op when sources are unchanged — and a real rebuild
# the moment they aren't. (An earlier version guarded with
# `[ ! -x binary ]`, which stayed stale through code changes; removed.)
# ps5upload-unrar (the RAR helper; UnRAR is never linked into the engine, see
# LICENSES/UnRAR-exception.md) is built into the same directory: the desktop
# build embeds it next to the engine.
_engine-release: setup-engine
	@if [ -n "$(ENGINE_TARGET)" ]; then \
		cd $(ENGINE_DIR) && $(CARGO) build --release -p ps5upload-engine -p ps5upload-unrar --target "$(ENGINE_TARGET)"; \
	else \
		cd $(ENGINE_DIR) && $(CARGO) build --release -p ps5upload-engine -p ps5upload-unrar; \
	fi

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
	@echo "Running the upload sweep against live PS5 at $(PS5_HOST) ..."
	@node bench/run-sweep.mjs --spawn-engine --gen-fixtures

# Wait for the payload's runtime port to accept connections after send.
# Retries 15×/2s = 30s ceiling; exits non-zero if the port never opens.
_wait-payload-ready:
	@echo "Waiting for the helper's AVA1 port 9120 ..."
	@i=0; while [ $$i -lt 15 ]; do \
		if nc -z -w 1 "$(PS5_HOST)" 9120 >/dev/null 2>&1; then \
			echo "✓ runtime port open"; exit 0; \
		fi; \
		i=$$((i+1)); sleep 2; \
	done; \
	echo "ERROR: PS5 AVA1 port 9120 did not open within 30s"; exit 1

#──────────────────────────────────────────────────────────────────────────────
# check-no-ftx2: the retired protocol (FTX2) and its ports (9113 transfer, 9114
# management) must not reappear anywhere in the repository.
#
# The scope is the whole tree, payload and the host C tests included, except the history the
# project keeps on purpose and the files that must name the legacy protocol (listed below). The
# pattern is case-insensitive "ftx2" or a bare port 9113/9114. Digits on either side are excluded
# so the engine's own 19113 is not a match. Binary files and lockfiles are not searched.
#
# Exceptions (each is a path excluded from the search; keep this list to files that must name
# the legacy protocol, and reword anything else to AVA1 terms):
#   CHANGELOG.md                     history keeps FTX2 by design.
#   site/releases.json               the website's copy of the release notes (same history).
#   protocol/ps5upload-ava1/         the method checklist describes the migration from FTX2.
#   Makefile, .github/workflows/engine-ci.yml
#                                    name this check and its pattern.
#   .../ps5upload-ctest/tests/payload_cutover.rs
#                                    asserts the payload no longer says FTX2 or 9113/9114.
#   .../ps5upload-engine/src/ava1_only_tests.rs
#                                    the tests that pin the engine free of the retired symbols.
#   client/src/lib/addr.ts,
#   client/src/lib/humanizeError.ts  tolerate a stale host:9113 / host:9114 a user or an
#                                    older engine message still carries.
#   *.test.ts, *.test.tsx            use 9113/9114 as fixture addresses for that same tolerance.
#   engine .../install/, fakelibs_api.rs, icon_cache.rs, ps5upload-ava1 pool.rs, the lab's
#   main.rs and bench.rs             old-port strip logic and its fixtures.
#   payload/third_party              vendored code (zlib's CRC table holds 0x...9113 words).
CHECK_NO_FTX2_PATTERN := ftx2|(^|[^0-9])911[34]([^0-9]|$$)
CHECK_NO_FTX2_SCOPE := .
CHECK_NO_FTX2_EXCEPT := \
	':!engine/crates/ps5upload-ctest/tests/payload_cutover.rs' \
	':!CHANGELOG.md' ':!site/releases.json' \
	':!CONTRIBUTING.md' ':!TESTING.md' \
	':!protocol/ps5upload-ava1' ':!third_party/ava1' \
	':!Makefile' \
	':!.github/workflows/engine-ci.yml' \
	':!*Cargo.lock' ':!*package-lock.json' \
	':!engine/crates/ps5upload-engine/src/ava1_only_tests.rs' \
	':!engine/crates/ps5upload-engine/src/install' \
	':!engine/crates/ps5upload-engine/src/fakelibs_api.rs' \
	':!engine/crates/ps5upload-engine/src/icon_cache.rs' \
	':!engine/crates/ps5upload-ava1/src/pool.rs' \
	':!engine/crates/ps5upload-lab/src/main.rs' \
	':!engine/crates/ps5upload-lab/src/bench.rs' \
	':!client/src/lib/addr.ts' \
	':!client/src/lib/humanizeError.ts' \
	':!client/src/**/*.test.ts' ':!client/src/**/*.test.tsx' \
	':!payload/third_party'

check-no-ftx2:
	@if git grep -n -I -i -E '$(CHECK_NO_FTX2_PATTERN)' -- $(CHECK_NO_FTX2_SCOPE) $(CHECK_NO_FTX2_EXCEPT); then \
		echo "ERROR: FTX2 or port 9113/9114 found above (see the check-no-ftx2 notes in the Makefile)"; exit 1; \
	else echo "✓ check-no-ftx2: clean"; fi

validate: check-no-ftx2 send-payload _wait-payload-ready
	@echo ""
	@echo "── Running smoke suite ────────────────────────────────"
	@npm run --silent smoke:hardware
	@echo ""
	@echo "── Running sweep (default profiles) ────────────────────"
	@node bench/run-sweep.mjs --spawn-engine --gen-fixtures
	@echo ""
	@echo "✓ validate complete — see bench/reports/ for the full report"

validate-xl: send-payload _wait-payload-ready
	@echo ""
	@echo "── Running smoke suite ────────────────────────────────"
	@npm run --silent smoke:hardware
	@echo ""
	@echo "── Running sweep (INCLUDING XL 200k-file stress) ───────"
	@node bench/run-sweep.mjs --spawn-engine --gen-fixtures --xl
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

dist-win: ENGINE_TARGET := x86_64-pc-windows-msvc
dist-win: payload setup-client
	@$(MAKE) _engine-release ENGINE_TARGET=$(ENGINE_TARGET)
	@echo "Building Windows distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:win
	@echo "✓ Windows packages built: $(CLIENT_DIR)/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/"

dist-win-arm: ENGINE_TARGET := aarch64-pc-windows-msvc
dist-win-arm: payload setup-client
	@$(MAKE) _engine-release ENGINE_TARGET=$(ENGINE_TARGET)
	@echo "Building Windows ARM64 distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:win-arm
	@echo "✓ Windows ARM64 packages built: $(CLIENT_DIR)/src-tauri/target/aarch64-pc-windows-msvc/release/bundle/"

dist-mac: ENGINE_TARGET := aarch64-apple-darwin
dist-mac: payload setup-client
	@$(MAKE) _engine-release ENGINE_TARGET=$(ENGINE_TARGET)
	@echo "Building macOS Apple Silicon distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:mac
	@echo "✓ macOS Apple Silicon packages built: $(CLIENT_DIR)/src-tauri/target/aarch64-apple-darwin/release/bundle/"

dist-mac-x64: ENGINE_TARGET := x86_64-apple-darwin
dist-mac-x64: payload setup-client
	@$(MAKE) _engine-release ENGINE_TARGET=$(ENGINE_TARGET)
	@echo "Building macOS Intel distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:mac-x64
	@echo "✓ macOS Intel packages built: $(CLIENT_DIR)/src-tauri/target/x86_64-apple-darwin/release/bundle/"

dist-linux: ENGINE_TARGET := x86_64-unknown-linux-gnu
dist-linux: payload setup-client
	@$(MAKE) _engine-release ENGINE_TARGET=$(ENGINE_TARGET)
	@echo "Building Linux x64 distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:linux
	@echo "✓ Linux x64 packages built: $(CLIENT_DIR)/src-tauri/target/x86_64-unknown-linux-gnu/release/bundle/"

dist-linux-arm: ENGINE_TARGET := aarch64-unknown-linux-gnu
dist-linux-arm: payload setup-client
	@$(MAKE) _engine-release ENGINE_TARGET=$(ENGINE_TARGET)
	@echo "Building Linux ARM64 distribution (Tauri)..."
	@cd $(CLIENT_DIR) && npm run dist:linux-arm
	@echo "✓ Linux ARM64 packages built: $(CLIENT_DIR)/src-tauri/target/aarch64-unknown-linux-gnu/release/bundle/"

#──────────────────────────────────────────────────────────────────────────────
# Android (Tauri mobile). Like the `dist` targets but produces an APK. The
# engine is linked in-process (engine_mobile.rs) rather than spawned, and only
# the PS5 payload is bundled — so `payload` runs first to embed a current ELF.
# gen/ is gitignored, so the recipes auto-run `android-init` if it's missing.
#──────────────────────────────────────────────────────────────────────────────

android-deps:
	@command -v rustup >/dev/null 2>&1 || { echo "ERROR: rustup is required for Android cross-compilation (Homebrew 'rust' ships only the host std). Install from https://rustup.rs"; exit 1; }
	@test -n "$(ANDROID_HOME)" || { echo "ERROR: ANDROID_HOME is not set — point it at your Android SDK (e.g. ~/Library/Android/sdk)"; exit 1; }
	@test -n "$(ANDROID_NDK_HOME)" || { echo "ERROR: no NDK under $(ANDROID_HOME)/ndk — install via Android Studio or 'sdkmanager \"ndk;28.0.12916984\"'"; exit 1; }
	@test -n "$(ANDROID_JAVA_HOME)" || { echo "ERROR: JDK 17 not found — set JAVA_HOME or install Temurin 17"; exit 1; }
	@rustup target list --installed 2>/dev/null | grep -qx '$(ANDROID_RUST_TARGET)' || rustup target add $(ANDROID_RUST_TARGET)
	@echo "Android toolchain OK — JDK=$(ANDROID_JAVA_HOME) NDK=$(ANDROID_NDK_HOME) target=$(ANDROID_RUST_TARGET)"

# ─── Headless emulator for testing without a physical device ─────────────
# Lifecycle lives in scripts/android-emu.sh. Artifacts (screenshots, logcat,
# install logs) are written OUTSIDE the repo, to ~/.ps5upload/android-test/ —
# this repo has already leaked debug screenshots into a public repo, and a
# file that is not in the working tree cannot be committed by accident.
emu-create:
	@bash scripts/android-emu.sh create

emu-start:
	@bash scripts/android-emu.sh start

emu-stop:
	@bash scripts/android-emu.sh stop

emu-status:
	@bash scripts/android-emu.sh status

# Build the APK, then install + launch it on the emulator and capture proof.
# Depends on android-build so `make emu-test` is the single command.
emu-test: android-build
	@bash scripts/android-emu.sh test

android-init: android-deps setup-client
	@echo "Scaffolding Android project (tauri android init)..."
	@cd $(CLIENT_DIR) && $(ANDROID_ENV) npx tauri android init
	@echo "Allowing loopback cleartext so the in-process engine is reachable..."
	@bash scripts/release/android-postinit-patch.sh

android-build: android-deps payload setup-client
	@test -d $(ANDROID_GEN_DIR) || $(MAKE) android-init
	@# Re-apply the (idempotent) post-init patches on every build. They used to
	@# run only at init, so an existing gen/ never picked up a new patch step and
	@# a local APK silently differed from the CI one, which inits fresh.
	@bash scripts/release/android-postinit-patch.sh
	@echo "Building Android APK (debug, $(ANDROID_TARGET))..."
	@cd $(CLIENT_DIR) && $(ANDROID_ENV) npx tauri android build --debug --apk --target $(ANDROID_TARGET)
	@apk=$$(find "$(ANDROID_GEN_DIR)/app/build/outputs/apk" -name 'app-*-debug.apk' -print -quit 2>/dev/null); \
		echo "✓ APK built: $${apk:-under $(ANDROID_GEN_DIR)/app/build/outputs/apk}"

# iPhone: an unsigned .ipa for sideloading (AltStore, Sideloadly), as the release
# builds it. macOS with Xcode only; Xcode needs its iOS platform once
# (`xcodebuild -downloadPlatform iOS`).
IOS_GEN_DIR := $(CLIENT_DIR)/src-tauri/gen/apple

ios-init: setup-client
	@echo "Scaffolding iOS project (tauri ios init)..."
	@cd $(CLIENT_DIR) && npx tauri ios init --ci
	@bash scripts/release/ios-postinit-patch.sh

ios-build: payload setup-client
	@test -d $(IOS_GEN_DIR) || $(MAKE) ios-init
	@bash scripts/release/ios-postinit-patch.sh
	@echo "Building unsigned iPhone IPA (arm64)..."
	@cd $(CLIENT_DIR) && npx tauri ios build --ci --target aarch64 --no-sign
	@python3 scripts/release/verify-ios-ipa.py output/PS5Upload-ios-unsigned.ipa

# iPhone Simulator: see the app on a simulated iPhone, with live reload, like run-android.
# Reuses a booted iPhone simulator or boots one (scripts/ios-sim.sh), pre-grants the app's
# privacy permissions, and on Ctrl-C stops the dev server and shuts down only a simulator it booted. PS5UPLOAD_IOS_SIM="iPhone 17 Pro" picks the device. Needs Xcode's iOS platform once:
# `xcodebuild -downloadPlatform iOS`. For a real iPhone, build the .ipa (make ios-build).
IOS_VITE_PORT ?= 1440
IOS_SIM_RUST_TARGET := $(if $(filter arm64,$(shell uname -m)),aarch64-apple-ios-sim,x86_64-apple-ios)

run-ios: payload setup-client
	@command -v xcodebuild >/dev/null 2>&1 || { echo "ERROR: run-ios needs macOS with Xcode"; exit 1; }
	@rustup target list --installed 2>/dev/null | grep -qx '$(IOS_SIM_RUST_TARGET)' || rustup target add $(IOS_SIM_RUST_TARGET)
	@test -d $(IOS_GEN_DIR) || $(MAKE) ios-init
	@bash scripts/release/ios-postinit-patch.sh
	@out=$$(bash $(CURDIR)/scripts/ios-sim.sh ensure) || exit 1; \
	state=$$(printf '%s' "$$out" | cut -f1); name=$$(printf '%s' "$$out" | cut -f2); udid=$$(printf '%s' "$$out" | cut -f3); \
	if [ "$$state" = "booted" ]; then \
		trap 'trap - EXIT INT TERM; echo ""; echo "Shutting down the simulator this run booted..."; PS5UPLOAD_IOS_SIM_UDID='"$$udid"' bash $(CURDIR)/scripts/ios-sim.sh stop' EXIT INT TERM; \
	else \
		echo "  (leaving the running simulator up)"; \
	fi; \
	PS5UPLOAD_IOS_SIM_UDID="$$udid" bash $(CURDIR)/scripts/ios-sim.sh grant; \
	echo "Launching on $$name (tauri ios dev)..."; \
	cd $(CLIENT_DIR) && PS5UPLOAD_VITE_PORT=$(IOS_VITE_PORT) npx tauri ios dev "$$name" \
		--config '{"build":{"devUrl":"http://localhost:$(IOS_VITE_PORT)"}}'

# Boots the headless test emulator when nothing is attached, and shuts down
# ONLY an emulator this invocation booted -- a phone you plugged in, or an
# emulator you already had running, is left alone. The trap covers Ctrl-C,
# which is how `tauri android dev` normally ends.
# run-android: the emulator boots WITH its window (foreground) so the app can be
# seen and used. run-android-background: it boots headless, as emu-test does.
# Either way the dev server stays attached here for live reload.
# Android dev's Vite ports (live reload on the next one up), separate from
# desktop dev's 1420/1421 so `make run-client` and `make run-android` can run
# at the same time.
ANDROID_VITE_PORT ?= 1430

run-android: EMU_WINDOW := 1
run-android-background: EMU_WINDOW := 0
# Ctrl-C ends it cleanly: the dev server and the permission helper stop, any Gradle or Kotlin
# daemon this run started is stopped (by PID; daemons that were already running, such as Android
# Studio's, are left alone), and the emulator is shut down only when this run booted it.
GRADLE_DAEMON_RE := org.gradle.launcher.daemon.bootstrap.GradleDaemon|KotlinCompileDaemon
run-android run-android-background: android-deps payload setup-client
	@test -d $(ANDROID_GEN_DIR) || $(MAKE) android-init
	@bash scripts/release/android-postinit-patch.sh >/dev/null
	@state=$$(PS5UPLOAD_EMU_WINDOW=$(EMU_WINDOW) bash $(CURDIR)/scripts/android-emu.sh ensure) || exit 1; \
	grant=""; daemons_before=" $$(pgrep -f '$(GRADLE_DAEMON_RE)' | tr '\n' ' ') "; \
	trap 'trap - EXIT INT TERM; [ -n "$$grant" ] && kill $$grant 2>/dev/null; \
		for p in $$(pgrep -f "$(GRADLE_DAEMON_RE)"); do case "'"$$daemons_before"'" in *" $$p "*) ;; *) kill $$p 2>/dev/null;; esac; done; \
		if [ "'"$$state"'" = booted ]; then echo ""; echo "Shutting down the emulator this run booted..."; bash $(CURDIR)/scripts/android-emu.sh stop; fi' EXIT INT TERM; \
	[ "$$state" = booted ] || echo "  (leaving the existing device/emulator running)"; \
	bash $(CURDIR)/scripts/android-emu.sh grant --wait & grant=$$!; \
	echo "Launching on the attached Android device/emulator (tauri android dev)..."; \
	cd $(CLIENT_DIR) && PS5UPLOAD_VITE_PORT=$(ANDROID_VITE_PORT) $(ANDROID_ENV) npx tauri android dev \
		--config '{"build":{"devUrl":"http://localhost:$(ANDROID_VITE_PORT)"}}'

#──────────────────────────────────────────────────────────────────────────────
# Docker — self-hosted engine image. Mirrors what .github/workflows/
# docker-engine.yml publishes to ghcr.io/<owner>/ps5upload-engine on each
# release tag (multi-arch there; single-arch host build here). The REPO ROOT is
# the build context (the Dockerfile copies payload/ and third_party/ too); the
# Dockerfile is a scratch image wrapping the static binary.
#
# SECURITY: the engine API is unauthenticated. Bind to a LAN interface and set
# PS5UPLOAD_ALLOW_IP only on a trusted network — never expose it to the
# internet. See the header of engine/Dockerfile.
#──────────────────────────────────────────────────────────────────────────────

DOCKER ?= docker
DOCKER_ENGINE_IMAGE ?= ps5upload-engine
# Who may reach the engine through the published port. With `-p` the container
# sees Docker's bridge gateway, not loopback, so without this every request got
# "403 loopback only" (engine/Dockerfile header). 172.17.0.1 is the default
# bridge on Linux, 192.168.65.x the VM gateway on Docker Desktop; override with
# your LAN range (DOCKER_ALLOW_IP=192.168.1.0/24) to use it from other machines.
DOCKER_ALLOW_IP ?= 172.16.0.0/12,192.168.65.0/24
# This host's LAN IP, which the PS5 fetches package bytes from during a stream
# install. Required on Docker Desktop (macOS/Windows), where the container's
# own guess is a NAT address the PS5 cannot reach; on Linux prefer
# `docker run --network host` (engine/compose.yaml) and leave it empty.
DOCKER_PKG_HOST_IP ?=

docker-engine:
	@command -v $(DOCKER) >/dev/null 2>&1 || { echo "ERROR: docker not found on PATH."; exit 1; }
	@echo "Building $(DOCKER_ENGINE_IMAGE) image (context: repo root, as in CI)..."
	@$(DOCKER) build -t $(DOCKER_ENGINE_IMAGE) -f $(ENGINE_DIR)/Dockerfile .
	@echo "✓ Built image $(DOCKER_ENGINE_IMAGE) — run with: make docker-engine-run"

# Run the locally-built engine image. Binds the published port and points it at
# the PS5 (AVA1, port 9120). Override PS5_HOST, DOCKER_ALLOW_IP and
# DOCKER_PKG_HOST_IP as needed.
docker-engine-run: docker-engine
	@echo "Running $(DOCKER_ENGINE_IMAGE) — engine on :19113, PS5 at $(PS5_HOST), allowed from $(DOCKER_ALLOW_IP) ..."
	@$(DOCKER) run --rm -p 19113:19113 -e PS5_ADDR=$(PS5_HOST) \
		-e PS5UPLOAD_ALLOW_IP=$(DOCKER_ALLOW_IP) \
		$(if $(DOCKER_PKG_HOST_IP),-e PS5UPLOAD_PKG_HOST_IP=$(DOCKER_PKG_HOST_IP),) \
		$(DOCKER_ENGINE_IMAGE)

#──────────────────────────────────────────────────────────────────────────────
# Testing
#──────────────────────────────────────────────────────────────────────────────

test: test-root test-engine test-desktop test-payload test-client
	@echo ""
	@echo "✓ All tests passed"
	@echo ""

# Known-vulnerable dependencies and committed secrets (cargo audit, npm audit, gitleaks).
.PHONY: security
security:
	@bash scripts/security-scan.sh

lint: lint-scripts lint-client

lint-scripts:
	@npm run scripts:check

lint-client:
	@cd $(CLIENT_DIR) && npm run lint

audit-scripts:
	@npm run scripts:audit

coverage:
	@npm run coverage

coverage-engine:
	@npm run coverage -- --engine-only

coverage-client:
	@npm run coverage -- --client-only

quality:
	@npm run validate

quality-full:
	@npm run validate:full

# quality and the e2e suite at once, then test-payload; see scripts/release/release-check.sh.
release-check:
	@bash scripts/release/release-check.sh

quality-hardware:
	@npm run validate:hardware

ci: quality

ci-full: quality-full

# Root-level tests: syntax-check the node scripts that back the bench + smoke
# harnesses. Hardware smoke coverage lives in `make sweep` / `make validate`.
test-root:
	@echo "Syntax-checking root node scripts..."
	@node --check tests/smoke-hardware.mjs
	@node --check bench/run-sweep.mjs
	@node --check scripts/gen-fixtures.mjs
	@node --check scripts/check-lockfile.mjs
	@node scripts/check-lockfile.mjs --self-test
	@echo "Checking the client lockfile kept its libc metadata..."
	@node scripts/check-lockfile.mjs
	@echo "✓ Root scripts valid"

test-engine: setup-engine
	@echo "Running Rust engine tests..."
	@# With cargo-nextest installed this runs the same tests the way scripts/validate-repo.mjs
	@# does (and CI, except for the C interop suite): test binaries side by side (most sit in
	@# network sleeps), each test in its own process, so the C interop tests' one server per
	@# process holds without --test-threads=1; engine/.config/nextest.toml serializes the
	@# timing-sensitive binaries.
	@# nextest skips doctests, so they run separately. Without nextest: cargo test, serially.
	@# The RAR tests start the ps5upload-unrar helper from the target directory: build it first.
	@cd $(ENGINE_DIR) && $(CARGO) build -p ps5upload-unrar
	@if $(CARGO) nextest --version >/dev/null 2>&1; then \
		cd $(ENGINE_DIR) && $(CARGO) nextest run --workspace --exclude ps5upload-ctest && \
		$(CARGO) test --workspace --exclude ps5upload-ctest --doc && \
		$(CARGO) nextest run -p ps5upload-ctest; \
	else \
		cd $(ENGINE_DIR) && $(CARGO) test --workspace --exclude ps5upload-ctest && \
		$(CARGO) test -p ps5upload-ctest -- --test-threads=1; \
	fi
	@echo "✓ Engine tests passed"
	@# Android cross-compile check. The engine cfg's some code out on
	@# Android (RAR, which needs the ps5upload-unrar helper program), so a call site added
	@# without the matching cfg builds fine on every desktop target and fails
	@# ONLY here. That is exactly how v5.31.0 shipped with no APK: every local
	@# gate was green on macOS while the Android build was broken, and it was
	@# not caught until the release workflow. `cargo check` is enough — this
	@# needs to catch a compile error, not produce an artifact.
	@if rustup target list --installed 2>/dev/null | grep -q aarch64-linux-android; then \
		echo "Checking engine against the Android target..."; \
		cd $(ENGINE_DIR) && $(CARGO) check -p ps5upload-engine --target aarch64-linux-android; \
		echo "✓ Engine compiles for Android"; \
	else \
		echo "⚠ aarch64-linux-android target not installed — SKIPPING the Android check."; \
		echo "  CI still runs it, so a cfg mistake will surface there instead of here."; \
		echo "  Install with: rustup target add aarch64-linux-android"; \
	fi

test-engine-coverage:
	@$(MAKE) coverage-engine

test-desktop: setup-engine
	@echo "Checking Tauri Rust shell..."
	@# No separate `cargo check --all-targets`: clippy --all-targets type-checks the same code
	@# (CI and scripts/validate-repo.mjs dropped it for the same reason).
	@cd $(CLIENT_DIR)/src-tauri && $(CARGO) clippy --all-targets -- -D warnings
	@cd $(CLIENT_DIR)/src-tauri && $(CARGO) test
	@echo "✓ Desktop Rust checks passed"

# Each host self-test runs under a time limit (scripts/run-limited.pl): one that hangs fails by
# name with a stack sample instead of silently eating CI's 20 minutes (twice in Oct 2026 a macOS
# runner hung in this target, and a cancelled job keeps no log).
SELFTEST_LIMIT ?= 240
RUN_LIMITED = perl $(CURDIR)/scripts/run-limited.pl $(SELFTEST_LIMIT)

test-payload: payload
	@echo "Validating payload binaries..."
	@for elf in "$(PAYLOAD_ELF)" "$(INSTALLER_ELF)"; do \
		if [ ! -s "$$elf" ]; then \
			echo "ERROR: $$elf is missing or empty."; \
			exit 1; \
		fi; \
		file "$$elf" | grep -q "ELF.*FreeBSD" || { \
			echo "ERROR: $$elf is not a PS5/FreeBSD ELF file."; \
			exit 1; \
		}; \
		if [ ! -s "$$elf.gz" ]; then \
			echo "ERROR: $$elf.gz is missing or empty."; \
			exit 1; \
		fi; \
		gzip -t "$$elf.gz" || { \
			echo "ERROR: $$elf.gz is not a valid gzip resource."; \
			exit 1; \
		}; \
	done
	@echo "✓ Main payload and PS5Upload installer are PS5 ELFs with gzip resources"
	@echo "Running the archive extractor's tests (host build)..."
	@$(MAKE) -C $(PAYLOAD_DIR)/extractor host-test
	@echo "Running play-time launch/resume self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-activity-selftest \
		$(PAYLOAD_DIR)/tests/activity_launch_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-activity-selftest
	@echo "Running play-time title pick self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-play-pick-selftest \
		$(PAYLOAD_DIR)/tests/play_pick_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-play-pick-selftest
	@echo "Running app-info layout self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-app-info-selftest \
		$(PAYLOAD_DIR)/tests/app_info_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-app-info-selftest
	@echo "Running uninstall outcome self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-unregister-outcome-selftest \
		$(PAYLOAD_DIR)/tests/unregister_outcome_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-unregister-outcome-selftest
	@echo "Running metadata title-id normalizer self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-tmdb-id-selftest \
		$(PAYLOAD_DIR)/tests/tmdb_id_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-tmdb-id-selftest
	@echo "Running console port scan self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-net-ports-selftest \
		$(PAYLOAD_DIR)/tests/net_ports_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-net-ports-selftest
	@echo "Running fan servo self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-fan-servo-selftest \
		$(PAYLOAD_DIR)/tests/fan_servo_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-fan-servo-selftest
	@echo "✓ fan target writes keep the rest of the profile; the guard writes only after a reset and gives up after 5 failures"
	@echo "Running cheat XML UTF-16 normalizer self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-xml-encoding-selftest \
		$(PAYLOAD_DIR)/tests/xml_encoding_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-xml-encoding-selftest
	@echo "✓ UTF-16 SHN/MC4 cheat files convert so their cheats appear"
	@echo "Running web UI HTTP parser self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-webui-http-selftest \
		$(PAYLOAD_DIR)/tests/webui_http_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-http-selftest
	@echo "Running webui strings lookup self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-webui-strings-selftest \
		$(PAYLOAD_DIR)/tests/webui_strings_selftest.c $(PAYLOAD_DIR)/src/webui_strings.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-strings-selftest
	@echo "✓ GET /strings/<lang>.json answers only the embedded languages, never a crafted path"
	@echo "Running console package reader self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-pkg-info-selftest \
		$(PAYLOAD_DIR)/tests/pkg_info_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-pkg-info-selftest
	@echo "✓ the console page's package preview reads PS4/PS5 headers and refuses out-of-range offsets"
	@echo "Running webui offline copy lookup self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-webui-offline-selftest \
		$(PAYLOAD_DIR)/tests/webui_offline_selftest.c $(PAYLOAD_DIR)/src/webui_offline.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-offline-selftest
	@echo "✓ GET /sw.js and /cache.appcache answer with their own types; nothing else matches"
	@echo "Running webui auth self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-webui-auth-selftest \
		$(PAYLOAD_DIR)/tests/webui_auth_selftest.c $(PAYLOAD_DIR)/src/webui_auth.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-auth-selftest
	@echo "Running webui server self-test (host build, ~25 s)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include -I$(AVA1_C) \
		-o /tmp/ps5upload-webui-server-selftest \
		$(PAYLOAD_DIR)/tests/webui_server_selftest.c $(PAYLOAD_DIR)/src/webui_server.c \
		$(PAYLOAD_DIR)/src/webui_http.c $(PAYLOAD_DIR)/src/webui_auth.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-server-selftest
	@echo "✓ silent, half-sent and flooding web clients never stall the helper"
	@echo "Running webui codec self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-webui-codec-selftest \
		$(PAYLOAD_DIR)/tests/webui_codec_selftest.c $(PAYLOAD_DIR)/src/webui_codec.c \
		$(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-codec-selftest
	@echo "✓ Unicode, quoted and very large folders survive the web UI's JSON both ways"
	@echo "Running webui file transfer self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-webui-files-selftest \
		$(PAYLOAD_DIR)/tests/webui_files_selftest.c $(PAYLOAD_DIR)/src/webui_files.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-files-selftest
	@echo "✓ text saves and uploads go through a same-folder temp + rename; a cut-off upload leaves no file; folders stream as a valid .tar"
	@echo "Running SMB daemon path/address/JSON self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/smb -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-smbd-path-selftest $(PAYLOAD_DIR)/tests/smbd_path_selftest.c \
		$(PAYLOAD_DIR)/smb/smbd_path.c $(PAYLOAD_DIR)/smb/smbd_json.c
	@$(RUN_LIMITED) /tmp/ps5upload-smbd-path-selftest
	@echo "✓ share paths, server addresses and non-ASCII names parse; bad ones are refused"
	@echo "Running SMB daemon copy state-machine self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/smb -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-smbd-copy-selftest $(PAYLOAD_DIR)/tests/smbd_copy_selftest.c \
		$(PAYLOAD_DIR)/smb/smbd_copy.c $(PAYLOAD_DIR)/smb/smbd_path.c $(PAYLOAD_DIR)/smb/smbd_localfs.c
	@$(RUN_LIMITED) /tmp/ps5upload-smbd-copy-selftest
	@echo "✓ share copies go through a part file, survive a dropped server, cancel and refuse cleanly"
	@echo "Running game library scan self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/smb -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-smbd-library-selftest $(PAYLOAD_DIR)/tests/smbd_library_selftest.c \
		$(PAYLOAD_DIR)/smb/smbd_library.c $(PAYLOAD_DIR)/smb/smbd_localfs.c $(PAYLOAD_DIR)/smb/smbd_path.c \
		$(PAYLOAD_DIR)/smb/smbd_json.c $(PAYLOAD_DIR)/src/pkg_info.c
	@$(RUN_LIMITED) /tmp/ps5upload-smbd-library-selftest
	@echo "✓ the library scan finds packages, folder dumps and images with their covers, stays inside its limits and never walks a dump"
	@echo "Running console page network-share routes self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include -I$(PAYLOAD_DIR)/smb \
		-I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen -I$(AVA1_C)/third_party/monocypher \
		-o /tmp/ps5upload-webui-smb-selftest $(PAYLOAD_DIR)/tests/webui_smb_selftest.c \
		$(PAYLOAD_DIR)/src/webui_smb.c $(PAYLOAD_DIR)/src/smb_shares.c $(PAYLOAD_DIR)/smb/smbd_json.c \
		$(PAYLOAD_DIR)/smb/smbd_path.c $(AVA1_C)/third_party/monocypher/monocypher.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-smb-selftest
	@echo "✓ saved shares keep passwords out of the page and the file; copies obey the helper's path policy"
	@echo "Running file index name-match self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-index-glob-selftest $(PAYLOAD_DIR)/tests/index_glob_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-index-glob-selftest
	@echo "✓ index name patterns match in linear time, whatever the pattern"
	@echo "Running file manager prefs self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-files-prefs-selftest \
		$(PAYLOAD_DIR)/tests/files_prefs_selftest.c $(PAYLOAD_DIR)/src/files_prefs.c $(PAYLOAD_DIR)/src/webui_codec.c \
		$(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-files-prefs-selftest
	@echo "✓ pinned/recent folders stay bounded; a garbled prefs file reads as empty; saves are atomic"
	@echo "Running multi-device mesh self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-mesh-selftest \
		$(PAYLOAD_DIR)/tests/mesh_selftest.c $(PAYLOAD_DIR)/src/mesh.c $(PAYLOAD_DIR)/src/webui_codec.c \
		$(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-mesh-selftest
	@echo "✓ devices connect, are listed, renamed, blocked and pruned; locks lapse; shared documents keep their revision"
	@echo "Running Show on TV / Show on PC handoff self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-files-handoff-selftest \
		$(PAYLOAD_DIR)/tests/files_handoff_selftest.c $(PAYLOAD_DIR)/src/files_handoff.c $(PAYLOAD_DIR)/src/files_prefs.c \
		$(PAYLOAD_DIR)/src/webui_codec.c $(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-files-handoff-selftest
	@echo "✓ a handoff opens once, expires after two minutes, and the PC reads the same prefs as the console"
	@echo "Running the PC keyboard and mouse (vinput) self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-vinput-selftest \
		$(PAYLOAD_DIR)/tests/vinput_selftest.c $(PAYLOAD_DIR)/src/vinput.c \
		$(PAYLOAD_DIR)/src/webui_codec.c $(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-vinput-selftest
	@echo "✓ the stub says not supported; sessions open, hand over and close; a missed heartbeat or a stop lets go of every key"
	@echo "Running the keyboard & mouse companion + relay self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -DVWIRE_DIR='"/tmp"' -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-vinput-relay-selftest \
		$(PAYLOAD_DIR)/tests/vinput_relay_selftest.c $(PAYLOAD_DIR)/src/vinput_relay.c $(PAYLOAD_DIR)/src/vinput_companion.c \
		$(PAYLOAD_DIR)/src/vinput_wire.c $(PAYLOAD_DIR)/src/vinput.c \
		$(PAYLOAD_DIR)/src/webui_codec.c $(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-vinput-relay-selftest
	@echo "✓ the companion takes only its token and one helper; the relay starts it, kills a silent one, starts it again with the devices, and gives up after three"
	@echo "Running webui settings + tile self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -DWEBUI_TILE_HOST_TEST -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-webui-settings-selftest \
		$(PAYLOAD_DIR)/tests/webui_settings_selftest.c $(PAYLOAD_DIR)/src/webui_settings.c \
		$(PAYLOAD_DIR)/src/webui_tile.c $(PAYLOAD_DIR)/src/webui_codec.c \
		$(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-settings-selftest
	@echo "✓ a missing or garbled settings file falls back to defaults; the tile is written only when it differs"
	@echo "Running webui input log self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-webui-inputlog-selftest \
		$(PAYLOAD_DIR)/tests/webui_inputlog_selftest.c $(PAYLOAD_DIR)/src/webui_inputlog.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-inputlog-selftest
	@echo "✓ the controller input log takes only plain short lines and never grows past its 64 KiB ring"
	@echo "Running webui cheat download store self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-webui-cheats-store-selftest \
		$(PAYLOAD_DIR)/tests/webui_cheats_store_selftest.c $(PAYLOAD_DIR)/src/webui_cheats_store.c \
		$(PAYLOAD_DIR)/src/webui_files.c $(PAYLOAD_DIR)/src/webui_codec.c \
		$(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-webui-cheats-store-selftest
	@echo "✓ a downloaded cheat is stored only under a plain name with its format's extension, through a same-folder temp + rename"
	@echo "Running AVA1 in-flight RPC gate self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(AVA1_C) \
		-o /tmp/ps5upload-ava1-rpc-gate-selftest \
		$(PAYLOAD_DIR)/tests/ava1_rpc_gate_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-ava1-rpc-gate-selftest
	@echo "✓ the helper's exit waits for web UI calls and refuses new ones"
	@echo "Running cheat title list self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-cheats-list-selftest \
		$(PAYLOAD_DIR)/tests/cheats_list_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-cheats-list-selftest
	@echo "✓ a large or badly encoded cheat pack lists as valid, bounded JSON"
	@echo "Running cheat apply-plan self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-cheats-plan-selftest \
		$(PAYLOAD_DIR)/tests/cheats_plan_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-cheats-plan-selftest
	@echo "✓ a cheat's code caves are written before the jumps into them, and removed after"
	@echo "Running cheat engine guard self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-cheats-guard-selftest \
		$(PAYLOAD_DIR)/tests/cheats_guard_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-cheats-guard-selftest
	@echo "✓ the cheat engine switch survives a helper restart, and a game its cheats keep crashing is paused"
	@echo "Running XML game patch self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-game-patch-selftest \
		$(PAYLOAD_DIR)/tests/game_patch_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-game-patch-selftest
	@echo "✓ PS-Game-Patch XML entries parse to the addresses, patterns and bytes GoldHEN would write"
	@echo "Running title dump self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include -I$(AVA1_C) -I$(AVA1_C)/gen -I$(PAYLOAD_DIR)/gen \
		-o /tmp/ps5upload-app-dump-selftest \
		$(PAYLOAD_DIR)/tests/app_dump_selftest.c $(PAYLOAD_DIR)/src/app_dump_core.c $(PAYLOAD_DIR)/src/app_dump_pkg.c \
		$(PAYLOAD_DIR)/src/app_dump_fself.c $(PAYLOAD_DIR)/src/app_dump_self.c $(PAYLOAD_DIR)/src/app_dump_sha256.c \
		$(PAYLOAD_DIR)/src/webui_codec.c $(AVA1_C)/gen/ava1_gen.c $(PAYLOAD_DIR)/gen/ava1_app_gen.c $(AVA1_C)/ava1_wire.c
	@$(RUN_LIMITED) /tmp/ps5upload-app-dump-selftest
	@echo "✓ a running title dumps with its package entries, appmeta, trophy and DLC; a stop leaves dump-incomplete.txt; decryption failures are counted, an unknown firmware keeps the plain copy"
	@echo "Running console clean-up rules self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-cleanup-rules-selftest \
		$(PAYLOAD_DIR)/tests/cleanup_rules_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-cleanup-rules-selftest
	@echo "✓ clean-up removes only its categories' paths: never the package library, saves, installed games or another drive's system folders"
	@echo "Checking per-console isolation..."
	@./scripts/check-per-console-isolation.sh
	@echo "✓ one console's data cannot be shown under another's name"
	@echo "Checking Sony-API lock coverage..."
	@./scripts/check-sony-api-lock.sh $(PAYLOAD_DIR)/src
	@echo "✓ every sceUserService/sceRegMgr caller is serialized"
	@echo "Checking /dev/mp1 stays read-only..."
	@./payload/tools/check-mp1-readonly.sh $(PAYLOAD_DIR)
	@echo "✓ no clock-changing /dev/mp1 request anywhere in the payload"
	@echo "Running GPU/CPU clock (mp1) self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-mp1-clock-selftest \
		$(PAYLOAD_DIR)/tests/mp1_clock_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-mp1-clock-selftest
	@echo "✓ clock reads are bounded, cached for 2 s, stop for good after one refusal, and never change a clock"
	@echo "Running measured CPU clock self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-cpu-measure-selftest \
		$(PAYLOAD_DIR)/tests/cpu_measure_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-cpu-measure-selftest
	@echo "✓ each core measured once on a worker thread, best run wins, cached for 2 s, off for good when no CPU takes the pin"
	@echo "Running hw-guard fault-recovery self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-hw-guard-selftest \
		$(PAYLOAD_DIR)/tests/hw_guard_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-hw-guard-selftest
	@echo "✓ hw-guard recovers Sony-getter faults without dropping the helper"
	@echo "Running wake-watchdog safety self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-wake-watchdog-selftest \
		$(PAYLOAD_DIR)/tests/wake_watchdog_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-wake-watchdog-selftest
	@echo "✓ clock adjustments and unknown firmware state cannot trigger kernel writes"
	@echo "Running process-identity self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-proc-identity-selftest \
		$(PAYLOAD_DIR)/tests/proc_identity_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-proc-identity-selftest
	@echo "✓ only our own threads are ever in SIGKILL range"
	@echo "Running prior-instance verdict self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-instance-verdict-selftest \
		$(PAYLOAD_DIR)/tests/instance_verdict_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-instance-verdict-selftest
	@echo "✓ an externally-killed predecessor is reported, never guessed at"
	@echo "Running ptrace timeout-recovery self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-ptrace-recovery-selftest \
		$(PAYLOAD_DIR)/tests/ptrace_recovery_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-ptrace-recovery-selftest
	@echo "✓ ptrace timeout recovery never resumes injected registers"
	@echo "Running app.db raw-scan self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-appdb-scan-selftest \
		$(PAYLOAD_DIR)/tests/appdb_scan_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-appdb-scan-selftest
	@echo "✓ app.db scan recovers titles without sqlite"
	@echo "Running content-database self-test (host build)..."
	@mkdir -p /tmp/ps5upload-cdb
	@cc -O1 -w -c -o /tmp/ps5upload-cdb/sqlite3.o \
		-DSQLITE_OS_UNIX=1 -DSQLITE_THREADSAFE=1 -DSQLITE_TEMP_STORE=2 \
		-DSQLITE_OMIT_LOAD_EXTENSION=1 -DSQLITE_OMIT_DEPRECATED=1 \
		-DSQLITE_OMIT_JSON=1 -DSQLITE_DEFAULT_MEMSTATUS=0 -DSQLITE_DQS=0 \
		-DSQLITE_UNTESTABLE=1 \
		$(PAYLOAD_DIR)/third_party/sqlite3/sqlite3.c
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-I$(PAYLOAD_DIR)/third_party/sqlite3 \
		-DCONTENT_DB_APP='"/tmp/ps5upload-cdb/app.db"' \
		-DCONTENT_DB_APPINFO='"/tmp/ps5upload-cdb/appinfo.db"' \
		-DCONTENT_DB_APP_BACKUP='"/tmp/ps5upload-cdb/app.db.before-hide"' \
		-o /tmp/ps5upload-content-db-selftest \
		$(PAYLOAD_DIR)/src/content_db.c \
		$(PAYLOAD_DIR)/tests/content_db_selftest.c \
		/tmp/ps5upload-cdb/sqlite3.o
	@$(RUN_LIMITED) /tmp/ps5upload-content-db-selftest
	@echo "✓ content databases read via SQL, with guarded writes"
	@echo "Running Game Activity JSON self-test (host build)..."
	@mkdir -p /tmp/ps5upload-actjson
	@cc -O2 -Wall -Wextra -Werror -pthread \
		-I$(PAYLOAD_DIR)/include -I$(PAYLOAD_DIR)/tests/hostshim \
		-I$(PAYLOAD_DIR)/third_party/sqlite3 \
		-DKERN_PROC_PROC=8 \
		-DCONTENT_DB_APP='"/tmp/ps5upload-actjson/app.db"' \
		-DCONTENT_DB_APPINFO='"/tmp/ps5upload-actjson/appinfo.db"' \
		-DACTIVITY_FILE='"/tmp/ps5upload-actjson/activity.json"' \
		-DACTIVITY_DIR='"/tmp/ps5upload-actjson"' \
		-DACTIVITY_SL2_DB='"/tmp/ps5upload-actjson/sl2_log.db"' \
		-o /tmp/ps5upload-actjson/selftest \
		$(PAYLOAD_DIR)/src/content_db.c \
		$(PAYLOAD_DIR)/tests/activity_json_selftest.c \
		/tmp/ps5upload-cdb/sqlite3.o
	@$(RUN_LIMITED) /tmp/ps5upload-actjson/selftest
	@echo "✓ every Game Activity tab names its titles, in the keys the engine reads"
	@echo "Running timed initializer serialization self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-timed-init-selftest \
		$(PAYLOAD_DIR)/tests/timed_init_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-timed-init-selftest
	@echo "✓ timed-out Sony initialization cannot overlap a retry"
	@echo "Running installer daemon logic self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/installer \
		-o /tmp/ps5upload-installer-selftest \
		$(PAYLOAD_DIR)/tests/installer_selftest.c \
		$(PAYLOAD_DIR)/installer/protocol.c \
		$(PAYLOAD_DIR)/installer/pathsafe.c \
		$(PAYLOAD_DIR)/installer/jobs.c
	@$(RUN_LIMITED) /tmp/ps5upload-installer-selftest
	@echo "✓ installer request parse, replies, path safety and admission are sound"
	@echo "Running installer HTTP/coverage self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/installer \
		-o /tmp/ps5upload-installer-http-selftest \
		$(PAYLOAD_DIR)/tests/installer_http_selftest.c \
		$(PAYLOAD_DIR)/installer/http_range.c \
		$(PAYLOAD_DIR)/installer/jobs.c
	@$(RUN_LIMITED) /tmp/ps5upload-installer-http-selftest
	@echo "✓ installer range parse, headers, coverage and job ring are sound"
	@echo "Running installer loopback server self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/installer \
		-o /tmp/ps5upload-installer-loopback-selftest \
		$(PAYLOAD_DIR)/tests/installer_loopback_selftest.c \
		$(PAYLOAD_DIR)/installer/loopback.c \
		$(PAYLOAD_DIR)/installer/listen_guard.c \
		$(PAYLOAD_DIR)/installer/http_range.c \
		$(PAYLOAD_DIR)/installer/pathsafe.c \
		$(PAYLOAD_DIR)/installer/jobs.c
	@$(RUN_LIMITED) /tmp/ps5upload-installer-loopback-selftest
	@echo "✓ loopback server ranges, 404s foreign paths and .crc, serves a real sidecar"
	@echo "Running installer hint-table self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/installer \
		-o /tmp/ps5upload-installer-hints-selftest \
		$(PAYLOAD_DIR)/tests/installer_hints_selftest.c \
		$(PAYLOAD_DIR)/installer/hints.c
	@$(RUN_LIMITED) /tmp/ps5upload-installer-hints-selftest
	@echo "✓ installer hint table maps known Sony codes without third-party names"
	@echo "Running installer listener/stop/init-bound self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/installer \
		-o /tmp/ps5upload-installer-guard-selftest \
		$(PAYLOAD_DIR)/tests/installer_guard_selftest.c \
		$(PAYLOAD_DIR)/installer/listen_guard.c \
		$(PAYLOAD_DIR)/installer/jobs.c
	@$(RUN_LIMITED) /tmp/ps5upload-installer-guard-selftest
	@echo "✓ installer accept loops back off, stop waits for installs, init bound is monotonic"
	@echo "Running param.json SDK-rewrite self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-sdk-param-selftest \
		$(PAYLOAD_DIR)/tests/sdk_param_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-sdk-param-selftest
	@echo "✓ SDK version rewrite reports what it actually changed"
	@echo "Running ELF param-segment self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-elf-param-selftest \
		$(PAYLOAD_DIR)/tests/elf_param_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-elf-param-selftest
	@echo "✓ SDK patcher targets param segments, not stray magic bytes"
	@echo "Running sandbox library-overlay unmount self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-sandbox-unmount-selftest \
		$(PAYLOAD_DIR)/tests/sandbox_unmount_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-sandbox-unmount-selftest
	@echo "✓ only a game sandbox's unionfs lib overlay may be force-unmounted"
	@echo "Running SDK version-pair self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-sdk-pairs-selftest \
		$(PAYLOAD_DIR)/tests/sdk_pairs_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-sdk-pairs-selftest
	@echo "✓ an SDK downgrade writes both halves of the version pair"
	@echo "Running libc.prx backport-patch self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-libc-backport-selftest \
		$(PAYLOAD_DIR)/tests/libc_backport_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-libc-backport-selftest
	@echo "✓ the libc.prx symbol swap is same-length and idempotent"
	@echo "Running fakelib overlay path/refusal self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-fakelib-overlay-selftest \
		$(PAYLOAD_DIR)/tests/fakelib_overlay_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-fakelib-overlay-selftest
	@echo "✓ fakelib overlay accepts only game sandbox library targets"
	@echo "Running sys_time settimeofday-fallback self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -pthread -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-sys-time-fallback-selftest \
		$(PAYLOAD_DIR)/src/sys_time.c \
		$(PAYLOAD_DIR)/tests/sys_time_fallback_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-sys-time-fallback-selftest
	@echo "✓ clock set falls back to settimeofday only when the SCE path did not take"
	@echo "Running cross-device rename-guard self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-cross-device-selftest \
		$(PAYLOAD_DIR)/tests/cross_device_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-cross-device-selftest
	@echo "✓ cross-mount renames are refused before they can panic the kernel"
	@echo "Running Remote Play registry-key self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -o /tmp/ps5upload-rp-keys-selftest \
		$(PAYLOAD_DIR)/tests/rp_keys_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-rp-keys-selftest
	@echo "✓ Remote Play keys match Sony's entry-number formula"
	@echo "Running SDK Changer filesystem-safety self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-sdk-changer-file-selftest \
		$(PAYLOAD_DIR)/tests/sdk_changer_file_selftest.c
	@$(RUN_LIMITED) /tmp/ps5upload-sdk-changer-file-selftest
	@echo "✓ SDK Changer patches only tracked sources and restores durable backups"

test-client: setup-client
	@echo "Testing client build..."
	@cd $(CLIENT_DIR) && npm run typecheck
	@cd $(CLIENT_DIR) && npm run lint
	@cd $(CLIENT_DIR) && npm test
	@cd $(CLIENT_DIR) && npm run build:vite
	@if [ ! -d "$(CLIENT_DIR)/dist" ]; then \
		echo "ERROR: client build failed - dist directory not found."; \
		exit 1; \
	fi
	@echo "✓ Client checks passed"

test-client-coverage:
	@$(MAKE) coverage-client

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
	@# The RAR helper is found next to the engine binary.
	@cd $(ENGINE_DIR) && $(CARGO) build --release -p ps5upload-unrar && $(CARGO) run --release -p ps5upload-engine

# `run-client` is the one-command dev flow. `npm run dev` in client/ is wired
# to `tauri dev`, which starts Vite + the Rust main process. The Rust setup
# hook in client/src-tauri/src/engine.rs spawns ps5upload-engine as a child
# of the window process; killing the window tears the engine down cleanly.
run-client: setup-client _payload-if-ready _check-tauri-system-deps _engine-release _kill-stale-client
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

# Pre-flight system-library check for `tauri dev`. Tauri's Rust crates
# (gdk-sys, gio-sys, javascriptcore-rs-sys, soup3-sys, ...) link against
# native GTK/WebKit on Linux, system WebKit on macOS, and MSVC + WebView2
# on Windows. Without them, cargo gets ~30s into compilation before
# emitting a wall of pkg-config errors. This target fails fast with a
# copy-pasteable install command for the host OS.
_check-tauri-system-deps:
	@os="$$(uname -s)"; \
	case "$$os" in \
	  Linux) \
	    missing=""; \
	    command -v pkg-config >/dev/null 2>&1 || missing="$$missing pkg-config"; \
	    if command -v pkg-config >/dev/null 2>&1; then \
	      pkg-config --exists webkit2gtk-4.1 2>/dev/null || missing="$$missing libwebkit2gtk-4.1-dev"; \
	      pkg-config --exists gtk+-3.0    2>/dev/null || missing="$$missing libgtk-3-dev"; \
	      pkg-config --exists librsvg-2.0 2>/dev/null || missing="$$missing librsvg2-dev"; \
	      pkg-config --exists ayatana-appindicator3-0.1 2>/dev/null || missing="$$missing libayatana-appindicator3-dev"; \
	      pkg-config --exists alsa 2>/dev/null || missing="$$missing libasound2-dev"; \
	    fi; \
	    if [ -n "$$missing" ]; then \
	      echo "ERROR: Tauri requires Linux system libraries that are not installed."; \
	      echo "  Missing:$$missing"; \
	      echo ""; \
	      if command -v apt-get >/dev/null 2>&1; then \
	        echo "  Install (Debian / Ubuntu / WSL Ubuntu):"; \
	        echo "    sudo apt update && sudo apt install -y \\"; \
	        echo "      libwebkit2gtk-4.1-dev build-essential curl wget file \\"; \
	        echo "      libxdo-dev libssl-dev libayatana-appindicator3-dev \\"; \
	        echo "      librsvg2-dev libasound2-dev pkg-config"; \
	      elif command -v dnf >/dev/null 2>&1; then \
	        echo "  Install (Fedora / RHEL):"; \
	        echo "    sudo dnf install -y webkit2gtk4.1-devel openssl-devel \\"; \
	        echo "      curl wget file libappindicator-gtk3-devel librsvg2-devel alsa-lib-devel \\"; \
	        echo "      pkgconf-pkg-config @development-tools"; \
	      elif command -v pacman >/dev/null 2>&1; then \
	        echo "  Install (Arch / Manjaro):"; \
	        echo "    sudo pacman -S --needed webkit2gtk-4.1 base-devel curl wget \\"; \
	        echo "      file openssl libappindicator-gtk3 librsvg alsa-lib pkgconf"; \
	      else \
	        echo "  See https://tauri.app/start/prerequisites/ for your distro."; \
	      fi; \
	      exit 1; \
	    fi; \
	    ;; \
	  Darwin) \
	    if ! xcode-select -p >/dev/null 2>&1; then \
	      echo "ERROR: Xcode Command Line Tools are not installed (Tauri needs the system WebKit framework)."; \
	      echo "  Install with: xcode-select --install"; \
	      exit 1; \
	    fi; \
	    ;; \
	  MINGW*|MSYS*|CYGWIN*) \
	    if ! command -v cl >/dev/null 2>&1 && ! command -v link >/dev/null 2>&1; then \
	      echo "ERROR: MSVC build tools (cl.exe / link.exe) not on PATH."; \
	      echo "  Install 'Visual Studio Build Tools 2022' with the C++ workload:"; \
	      echo "    https://visualstudio.microsoft.com/visual-cpp-build-tools/"; \
	      echo "  Then run this from a 'Developer Command Prompt' or 'x64 Native Tools' shell."; \
	      echo "  WebView2 is pre-installed on Windows 11; on Windows 10 install via:"; \
	      echo "    https://developer.microsoft.com/microsoft-edge/webview2/"; \
	      exit 1; \
	    fi; \
	    ;; \
	esac

# Kill any stale ps5upload processes + Vite on :1420 that belongs to us.
# Narrow enough not to touch unrelated node servers. Invoked as a dep of
# run-client so repeated launches after a mis-terminated session don't
# fail with "Port 1420 already in use".
#
# Implementation lives in scripts/kill-stale-client.mjs because the native
# tooling differs across the three supported host OSes (pkill/lsof/ps on
# Linux+macOS vs. taskkill/netstat/PowerShell on Windows), and a pure-shell
# recipe doing both branches becomes unreadable. Node is already a hard
# dependency for run-client (via setup-client), so this adds no new tools.
.PHONY: _kill-stale-client
_kill-stale-client:
	@node scripts/kill-stale-client.mjs

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
	@echo "  Pinned PS5 SDK: $(PS5_SDK_TAG)"
	@echo "  PS5_PAYLOAD_SDK: $(PS5_PAYLOAD_SDK)"
ifeq ($(shell uname -s),Darwin)
	@echo "  LLVM_CONFIG: $(LLVM_CONFIG)"
endif
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
	@echo '  echo "Code or docs changed. Update README.md / FAQ.md / CHANGELOG.md if user-visible."' >> .git/hooks/pre-commit
	@echo 'fi' >> .git/hooks/pre-commit
	@chmod +x .git/hooks/pre-commit
	@echo "✓ Git hooks installed"

# Convenience aliases
dev: run-client
start: run-client

release-post:
	@node scripts/release-posts.js $(ARGS)

promo-post:
	@node scripts/promo-posts.js $(ARGS)

# The AVA1 protocol library (github.com/phantomptr/ava1) is the third_party/ava1 submodule; its own
# tests, fuzzers and sanitizer runs live there (`make -C third_party/ava1 test`). Here: ps5upload's
# side of it (the helper's management methods over AVA1, the glue) against the library's C.
AVA1_C := third_party/ava1/c

test-ava1:
	# The C server and the data layer are process-wide singletons
	# (one CServer at a time, one ava1_data_start): serial, not two shells.
	cd engine && cargo test -p ps5upload-ctest -- --test-threads=1

# ps5upload-ctest with ASan + UBSan on every C unit, LSan included on Linux (see the library's
# `make sanitize` for why macOS is not used). lsan.supp lists the intentional leaks.
CLANG ?= clang
test-ava1-sanitize:
	cd engine && CC=$(CLANG) AVA1_CTEST_SANITIZE=1 RUSTFLAGS="-Clinker=$(CLANG) $$RUSTFLAGS" \
		ASAN_OPTIONS=$${ASAN_OPTIONS:-detect_leaks=$$([ "$$(uname)" = Linux ] && echo 1 || echo 0)} \
		LSAN_OPTIONS=suppressions=$(CURDIR)/engine/crates/ps5upload-ctest/lsan.supp \
		cargo test -p ps5upload-ctest -- --test-threads=1

# The Rust side takes the `ava1` crate from crates.io, the payload builds the C side from the
# submodule: both must be the same release. Fails when engine/Cargo.toml or
# client/src-tauri/Cargo.toml pins another version than the submodule's.
ava1-version-check:
	@want=$$(sed -n 's/^version = "\(.*\)"/\1/p' third_party/ava1/Cargo.toml | head -1); \
	for f in engine/Cargo.toml client/src-tauri/Cargo.toml; do \
		got=$$(sed -n 's/^ava1 = "=\{0,1\}\([^"]*\)"/\1/p' $$f); \
		if [ "$$got" != "$$want" ]; then \
			echo "$$f pins ava1 $$got, the third_party/ava1 submodule is $$want" >&2; exit 1; \
		fi; \
	done; echo "ava1 $$want: crates.io pin matches the submodule"

# Regenerates ps5upload's AVA1 application codecs from protocol/ps5upload-ava1/app.toml.
ava1-app-gen:
	cargo run -q --manifest-path third_party/ava1/Cargo.toml -p ava1-gen -- --ext app \
		--schema protocol/ps5upload-ava1/app.toml --rust engine/crates/ps5upload-ava1/src/app_gen.rs \
		--c-dir payload/gen --source protocol/ps5upload-ava1/app.toml --regenerate 'run `make ava1-app-gen`' $(ARGS)
