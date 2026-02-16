# PS5 Upload - Fast App Transfer

<p align="center">
  <img src="logo.png" alt="PS5 Upload logo" width="500" />
</p>

PS5 Upload is a fast, reliable way to move apps and homebrew to your PS5 without the pain of slow transfers.
Current release: **v1.5.4**.

New UI highlights:
- Cleaner Transfer and Manage layouts with clearer transfer settings.
- Built-in quick scan + optimize options for large folders.
- Unified Queues area with upload/extraction tabs, progress, and status.
- Extraction queue with progress, metadata, and cover art where available.
- New metrics panels (System, Network, Queue, Transfer, Tuning) with a live bottleneck indicator.
- Payload tuning suggestions (pack size, pacing, rate) surfaced during transfers.
- Upload V4 protocol with per-pack ACKs and replay for recovery after payload hiccups.
- Fast payload paths for large folders and files (parallel `UPLOAD_FAST` + lane-based single-file uploads).
- Payload-side backpressure and write validation for safer large-folder uploads.
- Progress UI with ETA, average speed, elapsed time, and last update.
- Upload queue item info popup with per-item transfer parameters.
- Smoother transfer/extraction updates and less spiky speed readout.
- Expanded UI language support (Vietnamese, Hindi, Bengali, Portuguese‑BR, Russian, Japanese, Turkish, Indonesian, Thai, Korean, German, Italian).
- Bundled Noto fonts so non‑Latin UI text renders correctly on Windows, macOS, and Linux.
- Keep Awake now supports Auto mode (sleep allowed after 15 minutes idle).
- Automatic maintenance when idle to keep the payload healthy.
- Extraction Stop retries automatically; progress messaging now shows “Starting extraction…” before bytes appear.
- History resume workflow with 4 resume tiers (size-only, thresholded hash, full SHA256).
- Auto-tune adapts connections, pack size, and pacing; defaults are Payload 4 / FTP 10 and auto-tune adjusts as needed.
- Payload connections are capped at 4 for stability on PS5/FreeBSD 11.
- Parallel file writer threads on the payload with shared directory caching for faster small-file transfers.
- Resumed uploads skip directory creation overhead via `stat()` fast path.
- Client-side 8MB pack floor during small-file runs to keep packs full even under backpressure.
- Batch signaling and deferred close on the payload for lower per-file overhead on FAT/exFAT.
- Client pre-reads up to 64 tiny files in parallel to keep the payload pipeline fed.
- Uploads and archives now accept folder names with spaces, tabs, and brackets.
- Log level filtering with color-coded badges in the Logs panel.
- FAQ tab with an embedded, scrollable help panel.
- Manage “Reset UI” action for quick recovery from stuck states.
- Games tab with targeted storage scans (`etaHEN/games`, `homebrew`), custom scan paths, and duplicate detection.
- Games tools: storage filters, search, per-game size/file scan, and delete with confirmation.

It is optimized for thousands of small files on a local network, and supports both payload and FTP transfer modes (including a hybrid mix mode). FTP requires ftpsrv or the etaHEN FTP service enabled. Payload uploads auto-recover after a payload restart (within a short window) and resume missing files automatically.

PS5 runs a FreeBSD 11-based OS. The payload defaults are intentionally conservative to stay within kernel/process budget constraints: socket buffer sizes are bounded, worker concurrency is capped, and payload worker threads use an explicit (smaller) stack size to reduce memory pressure. If you build custom payloads, see `payload/config.h` for the `FTX_*` tuning knobs.

<img width="1911" height="1166" alt="Screenshot 2026-01-24 225424" src="https://github.com/user-attachments/assets/2773fd4b-ef2c-4b95-936c-1be3ab34e977" />
<img width="1913" height="1163" alt="Screenshot 2026-01-24 225409" src="https://github.com/user-attachments/assets/59376d5d-1dc6-484b-917a-3aec349053c7" />
<img width="1916" height="1162" alt="Screenshot 2026-01-24 225359" src="https://github.com/user-attachments/assets/e7684da2-c0ee-438e-9d6b-88aeb3d67005" />
<img width="1908" height="1151" alt="Screenshot 2026-01-24 225349" src="https://github.com/user-attachments/assets/93788efd-63d3-434a-8656-8f6788c6cea2" />

## Why use this?

If you've ever tried to upload a homebrew app containing 10,000 small assets, you know the pain. It takes forever.

This tool fixes that by bundling files into efficient "packs" on your computer and streaming them directly to the PS5's disk. No per-file handshakes, no temporary archive steps.

## Supported Platforms & Requirements

| Platform | OS Version | Architecture | Notes |
| :--- | :--- | :--- | :--- |
| **Windows** | 10 / 11 | x64 | **Zero-Install:** No DLLs or WinRAR required. Fully portable. |
| **macOS** | 12 (Monterey)+ | x64, ARM64 (M1+) | **Native Builds:** Separate Intel/Apple Silicon downloads. |
| **Linux** | GLIBC 2.31+ | x64, ARM64 | **Portable:** Tested on Ubuntu, Arch, Fedora, and Steam Deck. |

### Feature Support Matrix
- **Instant Streaming:** Supported on all platforms (ZIP, 7Z, RAR, and Folders).
- **Zero-Install Archives:** ZIP and 7Z are supported via pure-Rust libraries (no external software needed). RAR is statically linked into the binary for maximum portability.
- **AV Clean:** Optimized release builds with stripped symbols and proper manifests to minimize false positives.
- **Resizable UI:** Desktop window is resizable with responsive layout.

## Quick Start Guide

### 1. Load the Payload (PS5)
First, your PS5 needs to be listening for the connection. You need to send the `ps5upload.elf` file to your console.

**Option A: The Command Line (Netcat)**
If you're on Linux or macOS, this is the fastest way. Replace the IP with your PS5's IP.
```bash
# Send to port 9021 (common for payload loaders)
nc -w 1 192.168.137.xxx 9021 < payload/ps5upload.elf
```

**Option B: ELF Loader Tool**
Use any standard "ELF Loader" or "Payload Sender" GUI tool for Windows/Android. Point it to `ps5upload.elf` and send it to your console's IP.

**Option C: Built-in Payload Sender**
In the left panel of the app, under the **Payload** section, you can browse for a local `.elf` file and send it, or automatically download and send the latest payload.

**Success?**
You should see a notification pop up on your TV: **"PS5 Upload Server - Ready on port 9113"**.

### 2. Install & Run the App (Computer)
**Windows**
1. Download the `PS5Upload-<version>-win-<arch>.zip`.
2. Extract it anywhere (portable).
3. Run `PS5Upload.exe` inside the extracted folder.

**macOS**
1. Download the `PS5Upload-<version>-mac-<arch>.dmg`.
2. Open the DMG and drag **PS5Upload** to Applications.
3. Launch PS5Upload from Applications.

**Linux**
1. Download the `PS5Upload-<version>-linux-<arch>.tar.gz`.
2. Extract it:
   ```bash
   tar -xzf PS5Upload-<version>-linux-<arch>.tar.gz
   ```
3. Run with sandbox disabled:
   ```bash
   ./ps5upload-desktop --no-sandbox
   ```

### Optional: Remote Browser App Mode (`app/`)
If you want browser access (backend + frontend in one process), use the app service:

```bash
make app
make run-app
```

By default it binds to `0.0.0.0:10331`.
To expose on LAN, run:

```bash
APP_HOST=0.0.0.0 APP_PORT=10331 make run-app
```

Open `http://<host-ip>:10331` in your browser.
This mode serves the desktop UI in browser form, and host-side path operations are resolved on the machine running the app service.
See `app/README.md` for full details and API endpoints.

Release bundle option:
- Download `PS5Upload-app-<version>.zip` from Releases.
- Unzip, then from the extracted `ps5upload-app/app` folder run:
  - `npm install --no-audit --no-fund`
  - `npm start -- --host 0.0.0.0 --port 10331`

### 3. Connect the App (Computer)
1.  In the left panel of the app, go to the **Connect** section.
2.  Type your PS5's IP address (e.g., `192.168.0.105`).
3.  Click **Connect**.
4.  You'll see the available storage space on your console show up in the **Storage** section.

### 4. Send Your App
1.  In the main panel, make sure you are on the **Transfer** tab.
2.  **Source:** Click **Browse** and pick the folder containing your app on your computer.
3.  **Destination:**
    *   Pick a drive (like `/mnt/usb0` or `/data`).
    *   Pick a preset location (like `homebrew` or `etaHEN/games`).
    *   Give your folder a name.
4.  **Upload:** Click **Upload** in the bottom right. The bar will track real-time progress.
5.  **RAR uploads (optional):** If your source is a `.rar`, extraction runs in a single turbo mode for maximum speed. You can still pick an optional **RAR Temp Storage** in Transfer settings to control where the archive is staged before extraction.

### 5. Manage Files (Optional)
Open the **Manage** tab to browse your PS5's storage. You can rename, move, copy, delete, and `chmod 777` files or folders. You can also download files and folders from your PS5 to your computer (folder downloads run in a safe, sequential mode for stability). If something gets stuck, use **Reset UI** in Manage to recover.

### 6. Queues (Optional)
Open the **Queues** tab to see upload and extraction queues. You can refresh the queues, monitor progress, and cancel queued work.
Tip: Click **info** on an upload queue item to view the exact parameters used for that upload.

### 7. Resume Transfers (Optional)
If a transfer was interrupted, you can enable **Resume** mode in the **Transfer** tab's settings. The next time you upload the same content to the same destination, it will skip files that are already there.
* **Fastest (size-only)** — quickest.
* **Faster/Fast** — hashes files above size thresholds.
* **Normal (SHA256)** — most accurate, slowest.

### 8. Games (Optional)
Open the **Games** tab to scan game folders across storage roots.  
You can:
- keep scans limited to `etaHEN/games` and `homebrew` (plus optional custom paths),
- filter by storage root,
- search by title/path/ID/version,
- detect duplicates,
- scan per-game total files/size,
- delete a game folder with confirmation.

### 9. FAQ (Optional)
Open the **FAQ** tab for built‑in help and troubleshooting (offline, bundled with the app).

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

**Q: Archive upload shows 0% or looks stuck at the start?**
*   Large archives can pause briefly while the payload reports READY and the client primes the read pipeline.
*   If the source is on a shared/network drive (for example `hgfs`), the first read can be slow.
*   The Transfer log now emits stall hints so you can see whether the delay is from payload READY or source reads.

**Q: macOS: "App is damaged" or "Unidentified Developer"?**
*   This is normal for unsigned apps. Right-click the app, select **Open**, and then click **Open** in the dialog.
*   If macOS still blocks it, go to **System Settings -> Privacy & Security** and click **Open Anyway** for PS5 Upload.
*   Last resort: remove the quarantine flag (run once):
    ```bash
    xattr -dr com.apple.quarantine /Applications/PS5Upload.app
    ```
*   No Apple Developer account is required; the app is intentionally unsigned.

**Q: macOS: Terminal window opens when I run the app?**
*   This happens if you run the raw binary. Use the `.app` bundle included in the release zip to avoid this.

**Q: Linux: A terminal pops up when I launch the app?**
*   Use the `PS5Upload.desktop` launcher from the Linux release zip (or create it with `make bundle-linux`).

**Q: Where are config and logs saved?**
*   The desktop app uses the OS app data directory:
    *   **Windows:** `%APPDATA%\\ps5upload`
    *   **macOS:** `~/Library/Application Support/ps5upload`
    *   **Linux:** `~/.local/share/ps5upload`
*   Logs are stored in the `logs` subfolder when **Save Logs** is enabled in the Logs tab. You can filter by level (Debug/Info/Warn/Error).

**Q: Where can I find help inside the app?**
*   Open the **FAQ** tab for bundled help and troubleshooting (works offline).

**Q: Can I browse and manage discovered games quickly?**
*   Yes. Use the **Games** tab to scan, filter, search, detect duplicates, check per-game size/file counts, and delete game folders.

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

**Desktop App (Electron + React):**

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

Discord server: https://discord.gg/fzK3xddtrM

[![Support me on Ko-fi](https://storage.ko-fi.com/cdn/kofi3.png?v=3)](https://ko-fi.com/B0B81S0WUA)
