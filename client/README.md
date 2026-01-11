# PS5 Upload - Client

This is the desktop app you run on your computer. It's built with Rust and designed to be simple, fast, and cross-platform.

## What it does
It takes a folder on your computer, packs the files into a highly efficient stream, and shoots them over to your PS5. It handles all the messy stuff like creating directories and calculating speeds for you.

## Supported Platforms
We officially support the "Big Three":
*   **Windows** (10, 11)
*   **Linux** (Debian, Ubuntu, Arch, etc.)
*   **macOS** (Verified on Apple Silicon & Intel)

## How to Build it
You probably just want to download the release, but if you want to build it yourself (or modify it), here's how.

**Prerequisites:**
You need [Rust](https://rustup.rs/) installed.

**The Steps:**
1.  Open your terminal/command prompt.
2.  Go to the client folder:
    ```bash
    cd client
    ```
3.  Build the release version:
    ```bash
    cargo build --release
    ```
3. **Run:**
   ```bash
   cargo run --release
   # OR run the binary directly from target/release/ps5upload
   ```

## Usage

1. Enter your PS5's **IP Address**.
2. Click **Connect**.
3. (Optional) Click **Send Payload** to send `ps5upload.elf` to port **9021**.
4. Select a **Source Folder** (your App/Game folder).
5. Select a **Destination** (e.g., `/mnt/usb0` -> `homebrew`).
6. Click **Start Upload**.

## Troubleshooting

- **macOS Terminal Window:** 
  If you run the binary directly, it may open a terminal window. To avoid this, use the bundled `.app`.
  If building from source, you can generate it with `make bundle-macos`.
  
  **"App is damaged" or "Unidentified Developer":**
  Since this is an open-source tool and not signed with a paid Apple ID:
  1. Move `PS5Upload.app` to your Applications folder.
  2. **Right-click** (or Control-click) the app.
  3. Select **Open**.
  4. Click **Open** in the dialog box. You only need to do this once.
  5. If macOS still blocks it, go to **System Settings -> Privacy & Security** and click **Open Anyway** for PS5 Upload.
  6. Last resort (run once):
     ```bash
     xattr -dr com.apple.quarantine /Applications/PS5Upload.app
     ```

- **Linux**: You may need GTK3 development libraries (e.g., `libgtk-3-dev` on Ubuntu).
  To avoid a terminal popping up, launch via the `PS5Upload.desktop` file included in the release zip
  (or generate it with `make bundle-linux`).

## Tips
*   **Custom Paths:** If the presets (like `etaHEN`) don't match where you want to put files, just choose "Custom" in the dropdown and type whatever folder path you like.
*   **Safety:** The app will warn you if you're about to overwrite an existing folder. No accidental deletions!
