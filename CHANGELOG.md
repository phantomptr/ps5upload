# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0-beta] - 2026-01-11

**Faster, Safer, and Cleaner.**

### ✨ New Features
*   **High Speed Transfer**: Now uses a binary protocol ("Pack-Based") to bundle small files, drastically improving speed for homebrew apps with many assets.
*   **Large File Support**: Seamlessly handles files of any size (including large PKGs).
*   **Smart Storage**: Automatically detects available drives (USB/Internal) and hides full or read-only locations.
*   **Safety First**: Checks if an app already exists before overwriting and verifies write permissions on the fly.
*   **Custom Paths**: You can now type a custom installation path manually.

### 🛠 Improvements
*   **Modern GUI**: A polished dark theme with real-time speed, ETA, and smoother progress bars.
*   **Rebranding**: Terminology updated from "Game" to "App".
*   **Optimized Payload**: Reduced logging overhead for maximum throughput.

## [0.1.0] - 2025-01-01

### Added
- Initial proof of concept.
- Basic TCP file streaming.
- PS5 Payload SDK integration.
