# PS5 Upload FAQ

Welcome! This FAQ covers setup, features, troubleshooting, and platform‑specific tips.
Latest release: **v1.4.0**.

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
- **Connections:** number of parallel streams

### Upload Modes (Payload / FTP / Mix)
**Q: What’s the difference between Payload, FTP, and Mix?**  
- **Payload:** Streams packed data directly to the PS5 via the payload. Best for many small files.
- **FTP:** Uses FTP for file-by-file uploads. Reliable and compatible with standard FTP workflows.
- **Mix:** Runs both at once. Payload always pulls the **smallest** remaining file, FTP always pulls the **largest** remaining file, and both keep going until they meet. No threshold and no duplicate uploads.

**Q: If one side fails in Mix, what happens?**  
Remaining files are handed to the other side once so the transfer can complete.

### Scan & Optimize
- **Scan** estimates size and file count quickly.
- **Optimize** suggests best compression/connections based on scan.
- **Scan is required for Optimize.**
- **Auto-tune** adjusts packing and pacing during upload; it may enable Optimize for small-file batches.
- Archives aren’t scanned.

### Bottleneck & Tuning
**Q: What does the Bottleneck indicator mean?**  
It’s a best‑guess of what’s limiting speed right now: network, payload CPU, payload disk write, or the client side. It updates as conditions change, so a flip between “Network” and “Payload disk write” during heavy uploads is normal.

**Q: What are “Suggested pack / pace / rate”?**  
These are payload‑side recommendations to keep transfers stable. When auto‑tune is enabled, the client will follow them. When auto‑tune is off, the client still applies safety‑only limits to avoid overload.

**Q: How do uploads recover from payload hiccups?**  
The client uses Upload V3 when available, which includes per‑pack ACKs and a replay window. If the payload stalls or restarts, the client can resend any unacknowledged packs and continue without corrupting data.

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
If **RAR Extract Mode** is enabled, the archive is uploaded to a temp folder and extracted on the PS5.  
If **RAR Temp Storage** is set, the temp folder is `<selected storage>/ps5upload/tmp` (e.g. `/mnt/usb0/ps5upload/tmp`); otherwise it uses the destination’s storage root.  
This requires a payload that supports the TMP override (older payloads will ignore the selection).

**Q: Why keep failed RARs?**  
Failed extractions keep the archive so you can **Requeue**.  
Temp files are deleted only on **success** or **Clear tmp**.

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

**Q: My transfer hangs or my PS5 freezes with a large folder.**
This can happen with folders containing tens of thousands of small files. As of **v1.3.11**, the payload has been significantly improved to handle this. It now batches file writes to prevent overwhelming the PS5's operating system. If you still experience issues:
*   Ensure you are using the payload from v1.3.11 or newer.
*   The transfer may appear to pause momentarily—this is the new backpressure system working to keep the PS5 stable. It will resume automatically.


---

## Performance Tips

- Use **Ethernet**
- Keep connections moderate (4–8)
- Avoid Wi‑Fi for large folder uploads
- Use **Scan + Optimize** for large folder sets

---

## Support

Report issues with:
- Logs
- PS5 firmware + payload version
- Steps to reproduce

Discord: https://discord.gg/fzK3xddtrM
