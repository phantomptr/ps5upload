# Changelog

What's new in ps5upload, written for humans.

---

## 2.2.1

**Fixes**

- **Multi-GiB single-file uploads no longer load the file into RAM.**
  The 2.2.0 fix used a memory map; this one streams a single shard-
  sized buffer (~32 MiB) for the entire upload instead. Avoids the
  Windows page-cache pressure and 32-bit address-space failures that
  could surface on huge `.exfat` / `.ffpkg` images. Peak RAM is now
  bounded by the shard size, not the file size.
- **Management calls now route to the right port.** `addr` overrides
  passed from the desktop client to handlers like `cleanup`,
  `list-dir`, `volumes`, `fs/*`, `hw/*` were going to the transfer
  port `:9113` instead of the management port `:9114`. Routing is
  now consistent across every endpoint.
- **Windows folder uploads land at the right PS5 path.** Backslashes
  in relative paths were being preserved as-is; they're now
  normalized to forward slashes before joining onto the PS5
  destination root.

**Quality of life**

- **Folder uploads can exclude files.** The desktop client's
  exclude-rules UI now feeds an `excludes` list end-to-end through
  reconcile and walk, with `.DS_Store`, `Thumbs.db`, `desktop.ini`,
  `.git/**`, and `*.esbak` as the default suggestions.
- **Image uploads can mount on completion.** After the transfer
  commits, the desktop UI offers to mount the staged image via the
  PS5 kernel's LVD backend in one step.
- **Six platform bundles, not three.** `make dist-mac-x64`,
  `make dist-win-arm`, and `make dist-linux-arm` now build for the
  three architectures the 2.2.0 release dropped silently. The
  Tauri build script picks the right per-target sidecar binary.
- **Engine sidecar restarts cleanly.** If the desktop app finds a
  registered child but no listener on `:19113`, it now kills the
  stale child and respawns instead of failing the readiness probe.

**CI**

- New target-matrix job runs `cargo check` for every shipped OS/arch
  combination on every PR (compile-only, fast). Cross-arch test
  execution stays on the host job to keep PR feedback under a minute.

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
