# Changelog

What's new in ps5upload, written for humans.

---

## 2.2.0

**Highlights**

- **In-app update check.** Settings → Updates checks GitHub once a day.
  When a new version is out, click Download and the archive lands in
  your Downloads folder — replace the app manually, no installer.
- **Resume survives app restart.** Close ps5upload mid-upload, reopen,
  and it picks up where it left off instead of starting over. Works
  for up to 24 hours per folder.
- **Wider PS5 firmware support.** One build now works on firmware 1.00
  through 12.70. Core transfer, mount, and file features work on every
  supported firmware; advanced process listing needs 9.00–11.60.

**Better uploads**

- **Folder uploads keep their name.** Drag `/Users/you/my-folder` onto
  `/data/homebrew` and it lands at `/data/homebrew/my-folder/`, not
  spilled into the parent. Won't clobber other titles.
- **Destination preview.** The Upload screen shows the exact PS5 path
  your files are about to land at, as you type.
- **Better error messages.** Upload and Volumes screens translate
  payload jargon into something actionable (“try again in a second”
  instead of `fs_list_volumes_getmntinfo_failed`).
- **Override retries too.** The Override upload path now retries on
  network flakes, matching Resume behavior.

**Fixes**

- **Resume actually resumes.** A payload bug was wiping half-finished
  transfers on reconnect. Fixed — the PS5 now holds onto partial data
  until you come back.
- **Fewer race conditions.** Thread-safety fixes in the payload,
  engine, and desktop app so concurrent operations don't step on
  each other.
- **Partial downloads clean up.** If an update download fails
  mid-stream, the leftover `.part` file gets removed instead of
  cluttering your Downloads folder.

**Cleanup**

- **Simpler release packaging.** Three downloads, one per platform:
  `.dmg` for macOS, `.zip` for Windows (portable `.exe` inside), and
  `.zip` for Linux (`.AppImage` inside). No installers to manage.
- **Removed Install / Launch / Uninstall UI.** Sony's install API
  wedges a userland payload; register games from a PS5-side tool
  instead.
- **Scene-tools strip.** Only shows payloads that are actually
  supported. NineS and kldload are no longer probed.
