# PS5 Upload FAQ

Welcome! This FAQ covers setup, features, troubleshooting, and platform‑specific tips.
Latest release: **v1.5.3**.

---

## Quick Start (All Platforms)

**Q: Do I need the payload?**  
Yes. The PS5 must be running `ps5upload.elf` on port **9113**.

**Q: How do I send the payload?**  
Use any payload sender (e.g., Netcat or a GUI loader). Example:
```bash
nc -w 1 <PS5-IP> 9021 < ps5upload.elf
```
You should see a PS5 notification: **“PS5 Upload Server Ready on port 9113”**.

---

## Installation & Running

### Windows
1. Download `PS5Upload-<version>-win-x64.zip` (or arm64 if needed).
2. Extract anywhere (portable).
3. Run `PS5Upload.exe`.

### macOS
1. Download `PS5Upload-<version>-mac-<arch>.dmg`.
2. Drag **PS5Upload** to Applications.
3. First run: right‑click → **Open** (unsigned app).

If macOS blocks it:
- System Settings → Privacy & Security → **Open Anyway**
- Optional: remove quarantine
```bash
xattr -dr com.apple.quarantine /Applications/PS5Upload.app
```

### Linux
1. Download `PS5Upload-<version>-linux-<arch>.tar.gz`.
2. Extract:
```bash
tar -xzf PS5Upload-<version>-linux-<arch>.tar.gz
```
3. Run with sandbox disabled:
```bash
./ps5upload-desktop --no-sandbox
```

### Browser App Mode (Optional)
1. From repo root, run:
```bash
make run-app
```
2. Open `http://0.0.0.0:10331` (or your configured host/port).
3. To expose on LAN:
```bash
APP_HOST=0.0.0.0 APP_PORT=10331 make run-app
```

**Important:** In browser mode, file/folder browsing for upload/download uses paths on the machine running the app service, not the remote browser device.

**Q: Is there a release zip for browser app mode?**  
Yes. Download `PS5Upload-app-<version>.zip` from Releases, extract it, then run:
```bash
cd ps5upload-app/app
npm install --no-audit --no-fund
npm start -- --host 0.0.0.0 --port 10331
```

---

## Connect & Storage

**Q: How do I connect?**  
Use the **Connect** panel, enter your PS5 IP, then click **Connect**.

**Q: Where can I upload?**  
Typical writable paths:
- `/data` (internal)
- `/mnt/usb0..usb7` (USB)
- `/mnt/ext0`, `/mnt/ext1` (external)

**Q: Storage doesn’t show up?**  
Plug drives **before** loading the payload and ensure they are writable.

---

## Games Tab

**Q: Which paths does Games scan use?**  
Per storage root (for example `/data`, `/mnt/ext0`, `/mnt/ext1`, `/mnt/usb0`), Games scans:
- `etaHEN/games`
- `homebrew`
- plus any custom scan paths you add in the Games tab.

**Q: Can I hide/show games by storage?**  
Yes. Use the storage toggle buttons in the Games tab header.

**Q: Can I search the game list?**  
Yes. The Games search box matches title, path, folder name, content ID, title ID, version, and marker file info.

**Q: How are duplicates detected?**  
Duplicates are grouped by:
1. `content_id` (preferred),
2. `title_id`,
3. folder name (last resort).

**Q: Why is “Scan files/size” limited to one at a time?**  
To reduce payload load and improve stability, only one per-game file/size scan runs at a time.

**Q: Why does game delete use an in-app popup instead of native confirm?**  
The app uses a styled in-app modal so the confirmation matches the desktop theme and UX.

---

## Language & UI

**Q: How do I change the UI language?**  
Use the language selector in the top-right of the app window.  
Supported: English, 简体中文, 繁體中文, Français, Español, العربية, Tiếng Việt, हिन्दी, বাংলা, Português (Brasil), Русский, 日本語, Türkçe, Bahasa Indonesia, ไทย, 한국어, Deutsch, Italiano.

**Q: Why do I see squares/boxes instead of text in some languages?**  
Update to **v1.3.6** or newer. We bundle Noto fonts so Hindi/Bengali/Thai/Korean text renders properly on Windows, macOS, and Linux.

---

## Transfers & Settings

### Upload (single transfer)
- **Source:** folder or archive
- **Destination:** storage + preset + folder name
- **Override if conflict found:** must be ON to overwrite
- **Resume Mode:** skips existing files (size-only / thresholded hash / full SHA256)
- **Compression:** Auto / None / LZ4 / ZSTD
- **Connections:** managed automatically (defaults: Payload 4, FTP 10; auto‑tune adjusts as needed)
- Payload connections are capped at 4 for stability on PS5/FreeBSD 11.

### Upload Modes (Payload / FTP / Mix)
**Q: What’s the difference between Payload, FTP, and Mix?**  
- **Payload:** Streams packed data directly to the PS5 via the payload. Best for many small files.
- **FTP:** Uses FTP for file-by-file uploads. Reliable and compatible with standard FTP workflows. Requires ftpsrv or the etaHEN FTP service enabled.
- **Mix:** Runs both at once. Payload always pulls the **smallest** remaining file, FTP always pulls the **largest** remaining file, and both keep going until they meet. No threshold and no duplicate uploads.

**Q: If one side fails in Mix, what happens?**  
If FTP is unavailable, Mix aborts and asks you to enable FTP (ftpsrv/etaHEN). If a side fails mid-transfer, remaining files are handed to the other side once so the transfer can complete.

### Scan & Optimize
- **Scan** estimates size and file count quickly.
- **Optimize** suggests best compression and enables auto‑tune for tougher file mixes.
- **Scan is required for Optimize.**
- **Auto-tune** adjusts connections, packing, and pacing during upload; it may enable Optimize for small-file batches.
- Archives aren’t scanned.

### Bottleneck & Tuning
**Q: What does the Bottleneck indicator mean?**  
It’s a best‑guess of what’s limiting speed right now: network, payload CPU, payload disk write, or the client side. It updates as conditions change, so a flip between “Network” and “Payload disk write” during heavy uploads is normal.

**Q: What are “Suggested pack / pace / rate”?**  
These are payload‑side recommendations to keep transfers stable. When auto‑tune is enabled, the client will follow them. When auto‑tune is off, the client still applies safety‑only limits to avoid overload.

**Q: The payload exits or gets killed mid‑transfer. What can I do?**  
PS5 runs a FreeBSD 11-based OS with tighter process/kernel budget constraints than a desktop. Update to the latest payload/client first: recent versions bound socket buffers, cap worker concurrency, and set explicit thread stack sizes to reduce memory pressure. If you build custom payloads, keep the `FTX_*` values in `payload/config.h` conservative and avoid increasing worker thread counts/queue depths aggressively.

**Q: What is Mad Max mode? Should I use it?**  
Mad Max is an aggressive single-file upload profile. It can improve throughput on some setups but may destabilize the PS5 payload (crashes/stalls). Use it only when you explicitly need it, and prefer the default (non-Mad Max) transfer settings for stability.

**Q: How do uploads recover from payload hiccups?**  
The client uses Upload V4, which includes per‑pack ACKs and a replay window. If the payload stalls or restarts, the client waits for it to recover (short window), then resumes by checking which files already exist and re-uploading only what’s missing.

### Best Speed (Direct Ethernet)
**Q: How do I get the fastest transfer speeds?**  
Speed varies based on disk, CPU, file count, and network. The most reliable way is a direct Ethernet cable between your PC and PS5 (1 Gb/s or better).

Use these static IPs:
- **PC:** `192.168.137.1`
- **PS5:** `192.168.137.2`
- **Subnet mask:** `255.255.255.0`
- **Gateway:** `192.168.137.1`
- **DNS:** not required (anything is fine)

**Windows (PC side)**
1. Control Panel → Network and Internet → Network and Sharing Center → Change adapter settings.
2. Right‑click your Ethernet adapter → Properties → Internet Protocol Version 4 (TCP/IPv4).
3. Set:
   - IP address: `192.168.137.1`
   - Subnet mask: `255.255.255.0`
   - Default gateway: `192.168.137.1`
   - DNS: leave blank or any value
4. Apply, then connect the Ethernet cable directly to the PS5.

**macOS (PC side)**
1. System Settings → Network → select Ethernet.
2. Configure IPv4: **Manually**.
3. Set:
   - IP address: `192.168.137.1`
   - Subnet mask: `255.255.255.0`
   - Router: `192.168.137.1`
4. Apply, then connect the Ethernet cable directly to the PS5.

**Linux (PC side)**
1. Network settings → Wired → IPv4 → **Manual**.
2. Set:
   - Address: `192.168.137.1`
   - Netmask: `255.255.255.0`
   - Gateway: `192.168.137.1`
3. Apply, then connect the Ethernet cable directly to the PS5.

**PS5 side**
1. Settings → Network → Set Up Internet Connection → Use a LAN Cable.
2. Choose **Manual**.
3. Set:
   - IP address: `192.168.137.2`
   - Subnet mask: `255.255.255.0`
   - Default gateway: `192.168.137.1`
   - DNS: any or leave empty

### Resume Mode
Modes:
- **Fastest** (size only)
- **Faster** (hash files ≥ 1 GB)
- **Fast** (hash files ≥ 128 MB)
- **Normal** (hash all files, SHA256)

When **Resume** is enabled, **Override** is disabled (resume avoids overwriting).
For extremely large folders, the client may skip the compatibility listing phase to avoid long startup scans and proceed directly with upload.
Payload uploads may also pre-create directories up front for speed; this is capped and will be skipped on very large trees.

---

## Queues (Upload + Extraction)

### Upload Queue
Add items from **Transfer** or **Manage**.
- **Start / Stop** queue
- **Reorder** items
- **Clear Completed / Clear Failed / Clear Queue**
- **Info** button shows the exact upload parameters for that item

### Extraction Queue
Archive extractions happen on the PS5 and appear here.
- **Start / Stop**
- **Refresh**
- **Requeue** failed items
- **Clear Completed / Clear Failed / Clear Queue**
- **Clear tmp**: deletes `/ps5upload/tmp` on all storage roots
Note: extraction progress updates are periodic. A short delay before progress appears is normal; the item will show "Waiting for payload status..." and then "Starting extraction..." before bytes begin to move.
Stop retries automatically—no need to click multiple times.

---

## Archives & Extraction

**Q: How does RAR extraction work?**  
RAR extraction runs in a single turbo mode for maximum speed. The archive is uploaded to a temp folder and extracted on the PS5.  
If **RAR Temp Storage** is set, the temp folder is `<selected storage>/ps5upload/tmp` (e.g. `/mnt/usb0/ps5upload/tmp`); otherwise it uses the destination’s storage root.  
This requires a payload that supports the TMP override (older payloads will ignore the selection).

**Q: Why keep failed RARs?**  
Failed extractions keep the archive so you can **Requeue**.  
Temp files are deleted only on **success** or **Clear tmp**.

**Q: My archive upload looks stuck at the beginning. Is it frozen?**  
Large archives may pause briefly before progress appears while the payload signals READY and the client primes the read pipeline.  
If the source file is on a shared/network drive (for example `hgfs`), the first read can be slow.  
Check the Transfer log for stall hints; they indicate whether the delay is from payload READY or source reads.

---

## Manage Tab (PS5 Browser)

Use **Manage** to:
- browse storage
- copy / move / rename
- delete files or folders
- `chmod 777`
- extract RAR archives
- upload files/folders into the queue
- download files/folders (folder downloads run sequentially for stability)

---

## Logs & Debugging

**Logs panel**:
- Filter by **Debug / Info / Warn / Error**
- Optional **Save Logs**
- Resume scans now log a summary (skipped vs. remaining) in client logs after scanning.
  
**Automatic maintenance**:
When idle (no active transfer or extraction), PS5 Upload performs safe cleanup (buffer cleanup, log rotation, and temp cleanup) to reduce the chance of payload slowdowns.

**Log locations**:
- Windows: `%APPDATA%\\ps5upload\\logs`
- macOS: `~/Library/Application Support/ps5upload/logs`
- Linux: `~/.local/share/ps5upload/logs`

**In‑app help**:
- The **FAQ** tab is bundled with the desktop app and works offline.
- Use **Refresh** in the FAQ tab to reload it after updates.

**Payload logs**:
- `/data/ps5upload/payload.log`
- Rotated per launch in `/data/ps5upload/logs/`

---

## Common Errors

**“Transfer already running”**  
Payload is busy. Stop current transfer or use the queue.

**“Invalid path”**  
Only `/data` or `/mnt/*` are allowed.

**“Extraction pending forever”**  
Start the extraction queue, refresh, or **Clear tmp**.

**“Connection refused / timed out”**  
Reload the payload and reconnect.

**“api.gamesScanStats is not a function”**  
Restart the desktop app after updating. This means the preload bridge is stale and still using the old API surface.

**Q: My transfer hangs or my PS5 freezes with a large folder.**
This can happen with folders containing tens of thousands of small files. As of **v1.3.11**, the payload batches file writes to prevent overwhelming the PS5. As of **v1.4.3**, it also pauses recv under backpressure, verifies full writes, and reuses file descriptors for chunked writes. As of **v1.5.1**, the payload runs 2 parallel file writer threads with a shared directory cache, and the client enforces an 8MB pack floor during small-file runs to keep throughput high. If you still experience issues:
*   Ensure you are using the payload from v1.5.3 or newer.
*   The transfer may appear to pause momentarily—this is backpressure working to keep the PS5 stable. It will resume automatically.


---

## Performance Tips

- Use **Ethernet**
- Let auto‑tune manage connections (defaults: Payload 4, FTP 10)
- Avoid Wi‑Fi for large folder uploads
- Use **Scan + Optimize** for large folder sets
- For folders with many small files (< 64KB each), v1.5.1+ uses parallel writer threads and larger packs automatically — no configuration needed
- Resumed uploads are faster in v1.5.2+: existing directories are detected via `stat()` instead of re-running full `mkdir` walks
- v1.5.3+ further reduces per-file overhead with deferred close, batch signaling, and parallel client reads

---

## Support

Report issues with:
- Logs
- PS5 firmware + payload version
- Steps to reproduce

Discord: https://discord.gg/fzK3xddtrM
