# PS5 Upload - Fast App Transfer

<p align="center">
  <img src="logo.png" alt="PS5 Upload logo" width="500" />
</p>

PS5 Upload is a tool designed to get your apps and homebrew onto your console without the headache of slow transfers.

While it's optimized to chew through thousands of small files on a local network (where it smokes traditional FTP), it works over any standard network connection.


<img width="1682" height="1112" alt="ps5upload1" src="https://github.com/user-attachments/assets/79e1c599-afad-4740-8e12-cc32296d2892" />
Upload

<img width="1682" height="1112" alt="ps5upload" src="https://github.com/user-attachments/assets/41b3d285-1dbe-49c2-8c81-0a0e614d4aae" />
File Management

## Why use this?

If you've ever tried to upload a homebrew app containing 10,000 small assets via FTP, you know the pain. It takes forever.

This tool fixes that by bundling files into efficient "packs" on your computer and streaming them directly to the PS5's disk. No temp files, no waiting for handshakes between every single file.

## Supported Platforms

The client works on:
- **Windows** (10/11)
- **Linux** (Ubuntu, Arch, Fedora, etc.)
- **macOS** (Intel & Apple Silicon)

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

**Option C: Built-in "Send Payload" button**
Open the client, click **Send Payload**, pick `ps5upload.elf`, and it will send to port **9021**.

**Success?**
You should see a notification pop up on your TV: **"PS5 Upload Server - Ready on port 9113"**.

### 2. Connect the Client (Computer)
1.  Open the **PS5 Upload** app.
2.  Type your PS5's IP address (e.g., `192.168.0.105`).
3.  Click **Connect**.
4.  You'll see the available storage space on your console.

### 3. Send Your App
1.  **Source:** Click **Browse** and pick the folder containing your app on your computer.
2.  **Destination:**
    *   Pick a drive (like `/mnt/usb0` or `/data`).
    *   Pick a preset location (like `homebrew` or `etaHEN/games`).
    *   Give your folder a name.
3.  **Upload:** Click **Start Upload**. The bar will track real-time progress.

## FAQ

**Q: "Connection Refused" or it won't connect?**
*   Did you load the payload first? The PS5 stops listening if you reboot or rest mode.
*   Is your computer's firewall blocking the client?
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

**Q: Does this work on PS4?**
*   The logic is similar, but the payload is compiled specifically for the PS5 environment. It won't run on a PS4 as-is.

## Disclaimer

This software is for educational purposes. It is intended for use with legally obtained software and homebrew applications on hardware you own.

**Use at your own risk.** The authors are not responsible for any data loss or issues that arise from using this tool.

## Third-Party Libraries

This software uses the following open-source packages:

**Client (Rust):**
*   [eframe / egui](https://github.com/emilk/egui) - Immediate mode GUI framework
*   [tokio](https://tokio.rs/) - Asynchronous runtime for Rust
*   [serde](https://serde.rs/) - Serialization framework
*   [walkdir](https://github.com/BurntSushi/walkdir) - Efficient recursive directory walking
*   [rfd](https://github.com/PolyMeilex/rfd) - Native file dialogs
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
