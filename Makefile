# PS5 Upload - Root Makefile
# Makes building, testing, and setup simple

.PHONY: all setup build test clean help
.PHONY: payload desktop app
.PHONY: setup-payload setup-desktop setup-app
.PHONY: test-payload test-desktop
.PHONY: clean-payload clean-desktop clean-both
.PHONY: dist dist-win dist-mac dist-linux
.PHONY: release-post
.PHONY: run-app package-app

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
	@echo "  make setup          - Install all dependencies (payload SDK + Node.js)"
	@echo "  make build          - Build payload and desktop app"
	@echo "  make test           - Run all tests"
	@echo "  make clean          - Clean all build artifacts"
	@echo "  make clean-both     - Clean, build payload, and run desktop app"
	@echo ""
	@echo "Detailed Targets:"
	@echo "  make payload        - Build PS5 payload only"
	@echo "  make desktop        - Build desktop app (Electron + React)"
	@echo "  make app            - Prepare app service (backend + frontend)"
	@echo "  make setup-payload  - Check/setup payload build environment"
	@echo "  make setup-desktop  - Check toolchain for desktop app"
	@echo "  make setup-app      - Check toolchain for app service"
	@echo "  make test-payload   - Test payload build"
	@echo "  make test-desktop   - Test desktop app build"
	@echo "  make run-desktop    - Run desktop app in development mode"
	@echo "  make run-app        - Run app service (backend + frontend)"
	@echo "  make package-app    - Create app zip bundle in dist/"
	@echo ""
	@echo "Distribution:"
	@echo "  make dist           - Build distribution packages for current platform"
	@echo "  make dist-win       - Build Windows installer"
	@echo "  make dist-mac       - Build macOS app bundle"
	@echo "  make dist-linux     - Build Linux packages (AppImage, deb)"
	@echo "  make release-post   - Generate release post copy from CHANGELOG.md"
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
	@echo "  1. make build       - Build the payload and desktop app"
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
	@command -v node >/dev/null 2>&1 || { \
		echo "ERROR: Node.js is not installed!"; \
		echo "Install Node.js from https://nodejs.org"; \
		exit 1; \
	}
	@command -v npm >/dev/null 2>&1 || { \
		echo "ERROR: npm is not installed!"; \
		echo "Install Node.js from https://nodejs.org"; \
		exit 1; \
	}
	@echo "✓ Node.js toolchain found: node $$(node --version), npm $$(npm --version)"
	@echo "Installing desktop dependencies..."
	@cd desktop && npm install
	@echo "✓ Desktop dependencies installed"

setup-app:
	@echo "Checking app service toolchain..."
	@command -v node >/dev/null 2>&1 || { \
		echo "ERROR: Node.js is not installed!"; \
		echo "Install Node.js from https://nodejs.org"; \
		exit 1; \
	}
	@command -v npm >/dev/null 2>&1 || { \
		echo "ERROR: npm is not installed!"; \
		echo "Install Node.js from https://nodejs.org"; \
		exit 1; \
	}
	@echo "✓ Node.js toolchain found: node $$(node --version), npm $$(npm --version)"
	@echo "Installing app dependencies..."
	@cd app && npm install
	@echo "✓ App dependencies installed"

#──────────────────────────────────────────────────────────────────────────────
# Build
#──────────────────────────────────────────────────────────────────────────────

build: payload desktop
	@echo ""
	@echo "✓ Build complete!"
	@echo ""
	@echo "Output files:"
	@echo "  - payload/ps5upload.elf       (Load this on your PS5)"
	@echo "  - desktop/dist/               (Web assets)"
	@echo ""
	@echo "To run the desktop app: make run-desktop"
	@echo "To create installers: make dist"
	@echo ""

payload: setup-payload
	@echo "Building PS5 payload..."
	@$(MAKE) -C payload -j$(JOBS)
	@echo "✓ Payload built: payload/ps5upload.elf"

desktop: setup-desktop
	@echo "Building desktop app (Electron)..."
	@cd desktop && npm run build:vite
	@echo "✓ Desktop app built: desktop/dist/"

app: setup-app desktop
	@echo "✓ App service ready (desktop UI bundled for web mode)"

#──────────────────────────────────────────────────────────────────────────────
# Distribution
#──────────────────────────────────────────────────────────────────────────────

dist: setup-desktop
	@echo "Building distribution packages..."
	@cd desktop && npm run dist
	@echo "✓ Distribution packages built: desktop/release/"

dist-win: setup-desktop
	@echo "Building Windows distribution..."
	@cd desktop && npm run dist:win
	@echo "✓ Windows packages built: desktop/release/"

dist-mac: setup-desktop
	@echo "Building macOS distribution..."
	@cd desktop && npm run dist:mac
	@echo "✓ macOS packages built: desktop/release/"

dist-linux: setup-desktop
	@echo "Building Linux distribution..."
	@cd desktop && npm run dist:linux
	@echo "✓ Linux packages built: desktop/release/"

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

test-desktop: setup-desktop
	@echo "Testing desktop app build..."
	@cd desktop && npm run build:vite
	@if [ ! -d "desktop/dist" ]; then \
		echo "ERROR: Desktop build failed - dist directory not found!"; \
		exit 1; \
	fi
	@echo "✓ Desktop app build succeeded"

#──────────────────────────────────────────────────────────────────────────────
# Run
#──────────────────────────────────────────────────────────────────────────────

run-desktop: setup-desktop
	@echo "Starting PS5 Upload desktop app..."
	@cd desktop && npm run dev

run-app: app
	@echo "Starting PS5 Upload app service..."
	@echo "Use APP_HOST and APP_PORT to customize bind address."
	@cd app && npm run start

package-app:
	@echo "Packaging app bundle..."
	@mkdir -p dist
	@rm -rf dist/ps5upload-app
	@mkdir -p dist/ps5upload-app
	@cp -r app dist/ps5upload-app/
	@cp -r shared dist/ps5upload-app/
	@cp -r desktop dist/ps5upload-app/
	@rm -rf dist/ps5upload-app/app/node_modules
	@rm -rf dist/ps5upload-app/desktop/node_modules
	@rm -rf dist/ps5upload-app/desktop/dist
	@rm -rf dist/ps5upload-app/desktop/release
	@cp README.md CHANGELOG.md FAQ.md Makefile VERSION dist/ps5upload-app/
	@cd dist && zip -qr ps5upload-app.zip ps5upload-app
	@echo "✓ App bundle created: dist/ps5upload-app.zip"

#──────────────────────────────────────────────────────────────────────────────
# Clean
#──────────────────────────────────────────────────────────────────────────────

clean: clean-payload clean-desktop
	@echo "✓ All clean!"

clean-both:
	@$(MAKE) clean
	@$(MAKE) payload
	@$(MAKE) run-desktop

clean-payload:
	@echo "Cleaning payload build artifacts..."
	@$(MAKE) -C payload clean
	@echo "✓ Payload cleaned"

clean-desktop:
	@echo "Cleaning desktop build artifacts..."
	@rm -rf desktop/dist
	@rm -rf desktop/release
	@rm -rf desktop/node_modules/.vite
	@echo "✓ Desktop cleaned"

#──────────────────────────────────────────────────────────────────────────────
# Development / Utility
#──────────────────────────────────────────────────────────────────────────────

info:
	@echo "PS5 Upload - Build Information"
	@echo ""
	@echo "Environment:"
	@echo "  PS5_PAYLOAD_SDK: $(PS5_PAYLOAD_SDK)"
	@echo "  Node: $$(node --version 2>/dev/null || echo 'Not found')"
	@echo "  npm: $$(npm --version 2>/dev/null || echo 'Not found')"
	@echo "  Make: $$(make --version | head -1)"
	@echo ""
	@echo "Files:"
	@echo "  Payload:"
	@[ -f "payload/ps5upload.elf" ] && echo "    ✓ ps5upload.elf exists" || echo "    ✗ ps5upload.elf missing (run: make payload)"
	@echo "  Desktop:"
	@[ -d "desktop/dist" ] && echo "    ✓ Desktop build exists" || echo "    ✗ Desktop build missing (run: make desktop)"
	@[ -d "desktop/release" ] && echo "    ✓ Distribution packages exist" || echo "    ✗ Distribution missing (run: make dist)"
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

# Convenience aliases
dev: run-desktop
start: run-desktop

release-post:
	@./scripts/release-posts.sh
