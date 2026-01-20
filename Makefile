# PS5 Upload - Root Makefile
# Makes building, testing, and setup simple

.PHONY: all setup build test clean help
.PHONY: payload desktop
.PHONY: setup-payload setup-desktop
.PHONY: test-payload test-desktop
.PHONY: clean-payload clean-desktop

TAURI_CMD ?= $(shell if command -v cargo-tauri >/dev/null 2>&1; then echo "cargo tauri"; elif command -v tauri >/dev/null 2>&1; then echo "tauri"; fi)
JOBS ?= $(shell getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 4)

# Default target
all: build

#──────────────────────────────────────────────────────────────────────────────
# Help
#──────────────────────────────────────────────────────────────────────────────

help:
	@echo "PS5 Upload - Build System"
	@echo ""
	@echo "Quick Start:"
	@echo "  make setup          - Install all dependencies (payload SDK + Rust)"
	@echo "  make build          - Build payload and verify desktop app"
	@echo "  make test           - Run all tests"
	@echo "  make clean          - Clean all build artifacts"
	@echo ""
	@echo "Detailed Targets:"
	@echo "  make payload        - Build PS5 payload only"
	@echo "  make desktop        - Build desktop app (Tauri + React)"
	@echo "  make setup-payload  - Check/setup payload build environment"
	@echo "  make setup-desktop  - Check toolchain for desktop app"
	@echo "  make test-payload   - Test payload build"
	@echo "  make test-desktop   - Test desktop app build"
	@echo "  make run-desktop    - Run desktop app (Tauri dev)"
	@echo ""
	@echo "Examples:"
	@echo "  make setup          # First time setup"
	@echo "  make build          # Build everything"
	@echo "  make run-desktop    # Launch desktop app"
	@echo ""

#──────────────────────────────────────────────────────────────────────────────
# Setup - First Time Installation
#──────────────────────────────────────────────────────────────────────────────

setup: setup-payload setup-desktop
	@echo ""
	@echo "✓ Setup complete!"
	@echo ""
	@echo "Next steps:"
	@echo "  1. make build       - Build the payload"
	@echo "  2. Load ps5upload.elf on your PS5"
	@echo "  3. make run-desktop - Start uploading games!"
	@echo ""

setup-payload:
	@echo "Checking PS5 Payload SDK..."
	@if [ -z "$(PS5_PAYLOAD_SDK)" ]; then \
		echo ""; \
		echo "ERROR: PS5_PAYLOAD_SDK is not set!"; \
		echo ""; \
		echo "Please install the PS5 Payload SDK:"; \
		echo "  1. Download from: https://github.com/ps5-payload-dev/sdk/releases/latest"; \
		echo "  2. Extract to /opt/ps5-payload-sdk (or your preferred location)"; \
		echo "  3. Run: export PS5_PAYLOAD_SDK=/opt/ps5-payload-sdk"; \
		echo "  4. Add to ~/.bashrc to make permanent"; \
		echo ""; \
		exit 1; \
	fi
	@echo "✓ PS5_PAYLOAD_SDK found: $(PS5_PAYLOAD_SDK)"
	@if [ ! -f "$(PS5_PAYLOAD_SDK)/toolchain/prospero.mk" ]; then \
		echo ""; \
		echo "ERROR: SDK files not found at $(PS5_PAYLOAD_SDK)"; \
		echo "Please verify your SDK installation."; \
		echo ""; \
		exit 1; \
	fi
	@echo "✓ PS5 Payload SDK is properly installed"

setup-desktop:
	@echo "Checking desktop app toolchain..."
	@command -v cargo >/dev/null 2>&1 || { \
		echo "ERROR: cargo is not installed!"; \
		echo "Install Rust from https://rustup.rs"; \
		exit 1; \
	}
	@command -v npm >/dev/null 2>&1 || { \
		echo "ERROR: npm is not installed!"; \
		echo "Install Node.js from https://nodejs.org"; \
		exit 1; \
	}
	@if [ -z "$(TAURI_CMD)" ]; then \
		echo "ERROR: Tauri CLI is not installed!"; \
		echo "Install with: cargo install tauri-cli"; \
		exit 1; \
	fi
	@echo "✓ Toolchain found: $$(cargo --version), $$(npm --version)"

#──────────────────────────────────────────────────────────────────────────────
# Build
#──────────────────────────────────────────────────────────────────────────────

build: payload desktop
	@echo ""
	@echo "✓ Build complete!"
	@echo ""
	@echo "Output files:"
	@echo "  - payload/ps5upload.elf   (Load this on your PS5)"
	@echo "  - desktop/src-tauri/target/release/ps5upload-desktop (Run with: make run-desktop)"
	@echo ""

payload: setup-payload
	@echo "Building PS5 payload..."
	@$(MAKE) -C payload -j$(JOBS)
	@echo "✓ Payload built: payload/ps5upload.elf"

desktop: setup-desktop
	@echo "Building desktop app (Tauri)..."
	@cd desktop && npm install
	@cd desktop && $(TAURI_CMD) build
	@echo "✓ Desktop app built: desktop/src-tauri/target/release/ps5upload-desktop"

bundle-macos: desktop
	@echo "Tauri bundle output:"
	@echo "  desktop/src-tauri/target/release/bundle/macos/PS5 Upload.app"

bundle-linux: desktop
	@echo "Tauri bundle output:"
	@echo "  desktop/src-tauri/target/release/bundle/appimage/ps5upload-desktop*.AppImage"

#──────────────────────────────────────────────────────────────────────────────
# Testing
#──────────────────────────────────────────────────────────────────────────────

test: test-payload test-desktop
	@echo ""
	@echo "✓ All tests passed!"
	@echo ""

test-payload: payload
	@echo "Testing payload build..."
	@if [ ! -f "payload/ps5upload.elf" ]; then \
		echo "ERROR: ps5upload.elf not found!"; \
		exit 1; \
	fi
	@echo "✓ Payload binary exists"
	@file payload/ps5upload.elf | grep -q "ELF" || { \
		echo "ERROR: ps5upload.elf is not a valid ELF file!"; \
		exit 1; \
	}
	@echo "✓ Payload is valid ELF binary"

test-desktop: desktop
	@echo "Testing desktop app build..."
	@cd desktop && npm install
	@cd desktop && $(TAURI_CMD) build
	@echo "✓ Desktop app build succeeded"

#──────────────────────────────────────────────────────────────────────────────
# Run
#──────────────────────────────────────────────────────────────────────────────

run-desktop: setup-desktop
	@echo "Starting PS5 Upload desktop app..."
	@cd desktop && npm install
	@cd desktop && $(TAURI_CMD) dev

#──────────────────────────────────────────────────────────────────────────────
# Clean
#──────────────────────────────────────────────────────────────────────────────

clean: clean-payload clean-desktop
	@echo "✓ All clean!"

clean-payload:
	@echo "Cleaning payload build artifacts..."
	@$(MAKE) -C payload clean
	@echo "✓ Payload cleaned"

clean-desktop:
	@echo "Cleaning desktop build artifacts..."
	@rm -rf desktop/dist
	@rm -rf desktop/src-tauri/target
	@rm -rf dist
	@echo "✓ Desktop cleaned"

#──────────────────────────────────────────────────────────────────────────────
# Development / Utility
#──────────────────────────────────────────────────────────────────────────────

info:
	@echo "PS5 Upload - Build Information"
	@echo ""
	@echo "Environment:"
	@echo "  PS5_PAYLOAD_SDK: $(PS5_PAYLOAD_SDK)"
	@echo "  Rust: $$(cargo --version 2>/dev/null || echo 'Not found')"
	@echo "  Node: $$(node --version 2>/dev/null || echo 'Not found')"
	@echo "  npm: $$(npm --version 2>/dev/null || echo 'Not found')"
	@if [ -n "$(TAURI_CMD)" ]; then \
		echo "  tauri: $$($(TAURI_CMD) --version 2>/dev/null)"; \
	else \
		echo "  tauri: Not found"; \
	fi
	@echo "  Make: $$(make --version | head -1)"
	@echo ""
	@echo "Files:"
	@echo "  Payload:"
	@[ -f "payload/ps5upload.elf" ] && echo "    ✓ ps5upload.elf exists" || echo "    ✗ ps5upload.elf missing (run: make payload)"
	@echo "  Desktop:"
	@[ -f "desktop/src-tauri/target/release/ps5upload-desktop" ] && echo "    ✓ Desktop app exists" || echo "    ✗ Desktop app missing (run: make desktop)"
	@echo ""

install-hooks:
	@echo "Installing git hooks..."
	@mkdir -p .git/hooks
	@echo '#!/bin/bash' > .git/hooks/pre-commit
	@echo 'echo "Checking documentation..."' >> .git/hooks/pre-commit
	@echo 'git diff --cached --name-only | grep -E "\.(c|py|h)$$" > /dev/null' >> .git/hooks/pre-commit
	@echo 'if [ $$? -eq 0 ]; then' >> .git/hooks/pre-commit
	@echo '  echo "Code files modified. Remember to update documentation!"' >> .git/hooks/pre-commit
	@echo 'fi' >> .git/hooks/pre-commit
	@chmod +x .git/hooks/pre-commit
	@echo "✓ Git hooks installed (will remind you to update docs)"
