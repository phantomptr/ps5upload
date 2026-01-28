# PS5 Upload FAQ

Welcome! This FAQ covers setup, features, troubleshooting, and platform‑specific tips.

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

## Transfers & Settings

### Upload (single transfer)
- **Source:** folder or archive
- **Destination:** storage + preset + folder name
- **Override if conflict found:** must be ON to overwrite
- **Resume Mode:** skips existing files (size/time/hash)
- **Compression:** Auto / None / LZ4 / ZSTD
- **Connections:** number of parallel streams

### Scan & Optimize
- **Scan** estimates size and file count quickly.
- **Optimize** suggests best compression/connections based on scan.
- **Scan is required for Optimize.**
- Archives aren’t scanned.

### Resume Mode
Modes:
- **Size** (fastest)
- **Size+Time** (balanced)
- **SHA256** (slowest, most accurate)

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
Note: extraction progress updates are periodic. A short delay before progress appears is normal; the item will show "Waiting for payload status..." until the first update arrives.

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

---

## Logs & Debugging

**Logs panel**:
- Filter by **Debug / Info / Warn / Error**
- Optional **Save Logs**

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
