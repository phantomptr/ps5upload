# PS5 Upload - Fast App Transfer

<p align="center">
  <img src="logo.png" alt="PS5 Upload logo" width="500" />
</p>

PS5 Upload is a fast, reliable way to move apps and homebrew to your PS5 without the pain of slow transfers.

It is optimized for thousands of small files on a local network (where it outperforms FTP), but works over any standard network connection.

![Upload](https://github.com/user-attachments/assets/79e1c599-afad-4740-8e12-cc32296d2892)
![File Management](https://github.com/user-attachments/assets/41b3d285-1dbe-49c2-8c81-0a0e614d4aae)

## Why use this?

If you've ever tried to upload a homebrew app containing 10,000 small assets via FTP, you know the pain. It takes forever.

This tool fixes that by bundling files into efficient "packs" on your computer and streaming them directly to the PS5's disk. No per-file handshakes, no temporary archive steps.

## Supported Platforms & Requirements

| Platform | OS Version | Architecture | Notes |
| :--- | :--- | :--- | :--- |
| **Windows** | 10 / 11 | x64 | **Zero-Install:** No DLLs or WinRAR required. Fully portable. |
| **macOS** | 12 (Monterey)+ | x64, ARM64 (M1+) | **Universal:** Works on Intel and Apple Silicon natively. |
| **Linux** | GLIBC 2.31+ | x64, ARM64 | **Portable:** Tested on Ubuntu, Arch, Fedora, and Steam Deck. |

### Feature Support Matrix
- **Instant Streaming:** Supported on all platforms (ZIP, 7Z, RAR, and Folders).
- **Zero-Install Archives:** ZIP and 7Z are supported via pure-Rust libraries (no external software needed). RAR is statically linked into the binary for maximum portability.
- **AV Clean:** Optimized release builds with stripped symbols and proper manifests to minimize false positives.

## Quick Start Guide

### 1. Load the Payload (PS5)
First, your PS5 needs to be listening for the connection. You need to send the `ps5upload.elf` file to your console.

**Option A: The Command Line (Netcat)**
If you're on Linux or macOS, this is the fastest way. Replace the IP with your PS5's IP.
```bash
# Send to port 9021 (common for payload loaders)
nc -w 1 192.168.0.xxx 9021 < payload/ps5upload.elf
```

**Option B: ELF Loader Tool**
Use any standard "ELF Loader" or "Payload Sender" GUI tool for Windows/Android. Point it to `ps5upload.elf` and send it to your console's IP.

**Option C: Built-in Payload Sender**
In the left panel of the app, under the **Payload** section, you can browse for a local `.elf` file and send it, or automatically download and send the latest payload.

**Success?**
You should see a notification pop up on your TV: **"PS5 Upload Server - Ready on port 9113"**.

### 2. Connect the App (Computer)
1.  In the left panel of the app, go to the **Connect** section.
2.  Type your PS5's IP address (e.g., `192.168.0.105`).
3.  Click **Connect**.
4.  You'll see the available storage space on your console show up in the **Storage** section.

### 3. Send Your App
1.  In the main panel, make sure you are on the **Transfer** tab.
2.  **Source:** Click **Browse** and pick the folder containing your app on your computer.
3.  **Destination:**
    *   Pick a drive (like `/mnt/usb0` or `/data`).
    *   Pick a preset location (like `homebrew` or `etaHEN/games`).
    *   Give your folder a name.
4.  **Upload:** Click **Upload** in the bottom right. The bar will track real-time progress.

### 4. Manage Files (Optional)
Open the **Manage** tab to browse your PS5's storage. You can rename, move, copy, delete, and `chmod 777` files or folders. You can also download files and folders from your PS5 to your computer.

### 5. Resume Transfers (Optional)
If a transfer was interrupted, you can enable **Resume** mode in the **Transfer** tab's settings. The next time you upload the same content to the same destination, it will skip files that are already there.
* **Skip by size (fast)** — quickest.
* **Skip by size + time (medium)** — more accurate.
* **Verify SHA256 (slow)** — most accurate, slowest.

### 6. Chat (Optional)
Open the **Chat** tab to talk with other PS5Upload users. It uses a built-in key, auto-picks a display name, and requires an internet connection.

## FAQ

**Q: "Connection Refused" or it won't connect?**
*   Did you load the payload first? The PS5 stops listening if you reboot or rest mode.
*   Is your computer's firewall blocking the app?
*   Are you on the same network? (You don't *have* to be, but you need a route to the IP).

**Q: Do I need to use a LAN cable?**
*   Not strictly, but WiFi is much slower and less stable. For the best "high performance" experience, plug in an Ethernet cable.

**Q: Can I use this over the Internet?**
*   Yes, technically. If you forward port `9113` to your PS5, it will work. However, the protocol is optimized for speed, not security. We don't recommend exposing your exploited PS5 to the open web.

**Q: "No writable storage found"?**
*   The tool protects you from trying to write to read-only system partitions. If you want to use a USB drive, make sure it's formatted (exFAT is best) and plugged in *before* you load the payload.

**Q: macOS: "App is damaged" or "Unidentified Developer"?**
*   This is normal for unsigned apps. Right-click the app, select **Open**, and then click **Open** in the dialog.
*   If macOS still blocks it, go to **System Settings -> Privacy & Security** and click **Open Anyway** for PS5 Upload.
*   Last resort: remove the quarantine flag (run once):
    ```bash
    xattr -dr com.apple.quarantine /Applications/PS5Upload.app
    ```

**Q: macOS: Terminal window opens when I run the app?**
*   This happens if you run the raw binary. Use the `.app` bundle included in the release zip to avoid this.

**Q: Linux: A terminal pops up when I launch the app?**
*   Use the `PS5Upload.desktop` launcher from the Linux release zip (or create it with `make bundle-linux`).

**Q: Where are config and logs saved?**
*   The desktop app uses the OS app data directory:
    *   **Windows:** `%APPDATA%\\ps5upload`
    *   **macOS:** `~/Library/Application Support/ps5upload`
    *   **Linux:** `~/.local/share/ps5upload`
*   Logs are stored in the `logs` subfolder when **Save Logs** is enabled in the Logs tab.

**Q: Does this work on PS4?**
*   The logic is similar, but the payload is compiled specifically for the PS5 environment. It won't run on a PS4 as-is.

**Q: What if files are missing after a fast transfer?**
*   Set **Connections** to `1` for maximum reliability.
*   Use **Resume** to re-send only missing files.

## Disclaimer

This software is for educational purposes. It is intended for use with legally obtained software and homebrew applications on hardware you own.

**Use at your own risk.** The authors are not responsible for any data loss or issues that arise from using this tool.

## Third-Party Libraries

This software uses the following open-source packages:

**Desktop App (Tauri + Rust Core + React):**
*   [Tauri](https://tauri.app/) - Desktop app framework
*   [React](https://react.dev/) - UI library
*   [tokio](https://tokio.rs/) - Asynchronous runtime for Rust
*   [serde](https://serde.rs/) - Serialization framework
*   [anyhow](https://github.com/dtolnay/anyhow) - Flexible error handling

**Payload:**
*   [PS5 Payload SDK](https://github.com/ps5-payload-dev/sdk) - Open source SDK for PS5 payload development

## License

GNU General Public License v3.0 (GPLv3).
Free to use, free to modify.

## Credits

Created by **PhantomPtr**.
*   [Follow me on X (@phantomptr)](https://x.com/phantomptr)

## Support

If you find this tool useful, consider buying me a coffee!

[![Support me on Ko-fi](https://storage.ko-fi.com/cdn/kofi3.png?v=3)](https://ko-fi.com/B0B81S0WUA)
