# PS5 Upload - Root Makefile
# Makes building, testing, and setup simple

.PHONY: all setup build test clean help
.PHONY: payload client
.PHONY: setup-payload setup-client
.PHONY: test-payload test-client
.PHONY: clean-payload clean-client

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
	@echo "  make build          - Build payload and verify client"
	@echo "  make test           - Run all tests"
	@echo "  make clean          - Clean all build artifacts"
	@echo ""
	@echo "Detailed Targets:"
	@echo "  make payload        - Build PS5 payload only"
	@echo "  make client         - Build Rust GUI client only"
	@echo "  make setup-payload  - Check/setup payload build environment"
	@echo "  make setup-client   - Check Rust toolchain for GUI client"
	@echo "  make test-payload   - Test payload build"
	@echo "  make test-client    - Test client installation"
	@echo "  make run-gui        - Run GUI client"
	@echo ""
	@echo "Examples:"
	@echo "  make setup          # First time setup"
	@echo "  make build          # Build everything"
	@echo "  make run-gui        # Launch GUI"
	@echo ""

#──────────────────────────────────────────────────────────────────────────────
# Setup - First Time Installation
#──────────────────────────────────────────────────────────────────────────────

setup: setup-payload setup-client
	@echo ""
	@echo "✓ Setup complete!"
	@echo ""
	@echo "Next steps:"
	@echo "  1. make build       - Build the payload"
	@echo "  2. Load ps5upload.elf on your PS5"
	@echo "  3. make run-gui     - Start uploading games!"
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

setup-client:
	@echo "Checking Rust toolchain..."
	@command -v cargo >/dev/null 2>&1 || { \
		echo "ERROR: cargo is not installed!"; \
		echo "Install Rust from https://rustup.rs"; \
		exit 1; \
	}
	@echo "✓ Rust toolchain found: $$(cargo --version)"

#──────────────────────────────────────────────────────────────────────────────
# Build
#──────────────────────────────────────────────────────────────────────────────

build: payload client
	@echo ""
	@echo "✓ Build complete!"
	@echo ""
	@echo "Output files:"
	@echo "  - payload/ps5upload.elf   (Load this on your PS5)"
	@echo "  - client/target/release/ps5upload (Run with: make run-gui)"
	@echo ""

payload: setup-payload
	@echo "Building PS5 payload..."
	@$(MAKE) -C payload
	@echo "✓ Payload built: payload/ps5upload.elf"

client: setup-client
	@echo "Building Rust GUI client..."
	@cd client && cargo build --release
	@echo "✓ Rust GUI client built: client/target/release/ps5upload"

bundle-macos: client
	@echo "Creating macOS .app bundle..."
	@mkdir -p dist/PS5Upload.app/Contents/MacOS
	@mkdir -p dist/PS5Upload.app/Contents/Resources
	@cp client/target/release/ps5upload dist/PS5Upload.app/Contents/MacOS/
	@echo '<?xml version="1.0" encoding="UTF-8"?>' > dist/PS5Upload.app/Contents/Info.plist
	@echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '<plist version="1.0"><dict>' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '<key>CFBundleExecutable</key><string>ps5upload</string>' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '<key>CFBundleIdentifier</key><string>com.phantomptr.ps5upload</string>' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '<key>CFBundleName</key><string>PS5 Upload</string>' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '<key>CFBundlePackageType</key><string>APPL</string>' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '<key>CFBundleShortVersionString</key><string>$(shell cat VERSION)</string>' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '<key>LSUIElement</key><false/>' >> dist/PS5Upload.app/Contents/Info.plist
	@echo '</dict></plist>' >> dist/PS5Upload.app/Contents/Info.plist
	@chmod +x dist/PS5Upload.app/Contents/MacOS/ps5upload
	@if command -v codesign >/dev/null 2>&1; then \
		echo "Ad-hoc signing .app bundle..."; \
		codesign --force --deep --sign - dist/PS5Upload.app; \
	fi
	@echo "✓ macOS Bundle created: dist/PS5Upload.app"

bundle-linux: client
	@echo "Creating Linux desktop launcher..."
	@mkdir -p dist
	@cp client/target/release/ps5upload dist/ps5upload
	@printf '%s\n' \
		'[Desktop Entry]' \
		'Type=Application' \
		'Name=PS5 Upload' \
		'Exec=sh -c '"'"'exec "$(dirname "%k")/ps5upload"'"'" \
		'Terminal=false' \
		'Categories=Utility;' \
		> dist/PS5Upload.desktop
	@echo "✓ Linux launcher created: dist/PS5Upload.desktop"

#──────────────────────────────────────────────────────────────────────────────
# Testing
#──────────────────────────────────────────────────────────────────────────────

test: test-payload test-client
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

test-client: client
	@echo "Testing Rust client build..."
	@cd client && cargo build --release
	@echo "✓ Client build succeeded"

#──────────────────────────────────────────────────────────────────────────────
# Run
#──────────────────────────────────────────────────────────────────────────────

run-gui: client
	@echo "Starting PS5 Upload GUI..."
	@cd client && cargo run --release

#──────────────────────────────────────────────────────────────────────────────
# Clean
#──────────────────────────────────────────────────────────────────────────────

clean: clean-payload clean-client
	@echo "✓ All clean!"

clean-payload:
	@echo "Cleaning payload build artifacts..."
	@$(MAKE) -C payload clean
	@echo "✓ Payload cleaned"

clean-client:
	@echo "Cleaning client build artifacts..."
	@rm -rf client/target
	@rm -rf dist
	@echo "✓ Client cleaned"

#──────────────────────────────────────────────────────────────────────────────
# Development / Utility
#──────────────────────────────────────────────────────────────────────────────

info:
	@echo "PS5 Upload - Build Information"
	@echo ""
	@echo "Environment:"
	@echo "  PS5_PAYLOAD_SDK: $(PS5_PAYLOAD_SDK)"
	@echo "  Rust: $$(cargo --version 2>/dev/null || echo 'Not found')"
	@echo "  Make: $$(make --version | head -1)"
	@echo ""
	@echo "Files:"
	@echo "  Payload:"
	@[ -f "payload/ps5upload.elf" ] && echo "    ✓ ps5upload.elf exists" || echo "    ✗ ps5upload.elf missing (run: make payload)"
	@echo "  Client:"
	@[ -f "client/target/release/ps5upload" ] && echo "    ✓ Rust GUI exists" || echo "    ✗ GUI missing (run: make client)"
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
