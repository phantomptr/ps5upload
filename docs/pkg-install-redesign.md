# Design: PKG install UX — answers + redesign

Status: proposal (not yet implemented). Triggered by a user report on FW 12.00.

## The report

> "pkg upload but when I go to install on PS5, system software gets a crash —
> nothing works for 5 sec, then black screen, then it comes back with the pkg
> installation in the PS5's queue. Install and run perfectly. Do I need to send
> ezremote.elf — I think your payload has it within? Also: maybe do the install
> automatically (delete pkg / integrate the pkg tab with the upload tab — all in
> one queue)."

Two questions and one feature request. Answers first, then the redesign.

---

## Q1: Do I need to send ezremote.elf?

**No.** ps5upload already bundles and auto-deploys the standalone installer.

- It ships as `payload/dpi/ezremote-dpi.elf` (the "DPI" = Direct Package
  Installer daemon, `payload/dpi/ezremote_dpi.c`). It listens on TCP `:9040`,
  takes a `.pkg` path/URL, and calls `sceAppInstUtilInstallByPackage()` from a
  **pristine elfldr-loaded process** — not from the main payload's heavily
  jailbroken context, which newer firmware's installer rejects.
- The engine auto-sends it when needed: the client's install path tries the
  **main payload** first; on rejection it calls `dpi_ensure` (Tauri command →
  engine), which loads `ezremote-dpi.elf` onto `:9040` if it isn't already
  there, then hands it the staged `.pkg`. See `client/src/state/pkgLibrary.ts`
  `install()` (the two-tier fallback) and `engine .../pkg_install.rs`.

So: nothing for the user to send. If anything, the *visible* DPI hand-off is a
sign the main-payload path was refused on that firmware and the fallback kicked
in — which is the design working, just not transparently (see Q2 + redesign).

## Q2: Why does ShellUI crash for ~5 s on FW 12.00, then recover with the pkg queued?

The install is a 3-tier fallback (`payload/src/bgft.c`):

1. **Tier 1 — in-process `sceAppInstUtilInstallByPackage`** from the main
   payload (after swapping to a forged ShellCore authid under `sony_api_lock`).
2. **Tier 2 — ShellUI-RPC**: ptrace into `SceShellUI` and issue the install
   from inside its process (`APPINST_VIA_SHELLUI_FLAG`).
3. **DPI daemon** on `:9040` (the ezremote path above).

The 5-second freeze → black screen → recover, ending with the pkg in **Sony's
own install queue**, is consistent with the install touching **SceShellUI /
SceShellCore** on FW 12.00: either Tier 2's ptrace-attach to ShellUI (which
momentarily destabilizes the UI process — the same "screen blank / brief
power blip" class of symptom the sensor-ptrace path documented), or Sony's
installer itself re-spawning the foreground app on the newer firmware.

**The key point: it's working.** The recovery + "pkg shows in the PS5 install
queue" + "install and run perfectly" means the install registered with Sony's
background installer successfully. The 5 s ShellUI blip is a cosmetic rough
edge of doing this from a jailbroken context on a firmware Sony hardened.

### Mitigations (cheap → involved)

1. **~~Prefer the DPI daemon on FW ≥ 12~~ — REJECTED after the user report.**
   The original idea was to skip the ShellUI-touch path on FW 12 by going
   straight to the DPI daemon. But the user confirmed the **main-payload path
   installs and *runs perfectly*** on FW 12.00 (just with the blip), whereas the
   DPI daemon registers metadata-only on newer firmware → an **unlaunchable**
   install. Switching tiers would trade a cosmetic blip for a broken install.
   So we do NOT change the install path on FW 12.
2. **Warn before it happens (IMPLEMENTED).** On FW ≥ 12 (firmware via
   `parsePS5Firmware` from the per-host kernel), `pkgLibrary.install()` sets a
   `busyNotice`: "the PS5 screen may go black for a few seconds — that's normal,
   don't touch the console; the install finishes in the background." Removes the
   scare factor without touching the working install path.
3. **Detect + reassure after (future).** The status poller already maps BGFT
   phases (`queued/download/install/done/error`); a post-blip "Handed to the PS5
   installer — finishing in the background" line is a nice follow-up.

---

## Feature request: auto-install + unified queue

Today these are **two separate surfaces**:

- **Upload tab / upload queue** (`client/src/state/uploadQueue.ts`): a real
  queue of `QueueItem`s (`folder | game-folder | image | file | archive`), each
  with post-upload steps baked in — `registerAfterUpload` (add a game folder to
  the home screen) and `mountAfterUpload` (mount a disc image). It runs items to
  terminal state, tracks bytes/throughput, persists.
- **Install Package tab** (`pkgLibrary.ts`): upload a `.pkg`, then a *separate,
  manual* "Install" action that waits for any active upload, runs the 3-tier
  install, polls BGFT status. Not part of the queue; no auto-install; no
  auto-cleanup.

The request: make a `.pkg` a first-class queue item that **uploads → installs →
(optionally) deletes the source `.pkg`** as one unit, in the same queue as
everything else.

### Proposed model: one queue, a per-item "make it playable" finisher

The upload queue already encodes "land bytes on the PS5, then run a finisher."
A folder's finisher is *register*; an image's is *mount*. A `.pkg`'s finisher is
*install*. So this is a natural extension, not a new subsystem.

**1. Add `sourceKind: "pkg"` to `QueueItem`** with pkg-specific finisher fields:

```ts
// additions to QueueItem (uploadQueue.ts)
installAfterUpload: boolean;      // run the installer once the .pkg lands
deletePkgAfterInstall: boolean;   // free the disk: rm the staged .pkg on success
// progress/result (mirrors mountedAt / registeredAs):
installPhase: "queued" | "download" | "install" | "done" | "error" | null;
installedTitle: string | null;    // title shown in the Library after install
```

**2. Extend the finisher in `runOne()`.** After the bytes land (same transfer
path the pkg already uses to stage onto the PS5), branch on `sourceKind`:

```text
game-folder + registerAfterUpload  → appRegister()           (exists today)
image       + mountAfterUpload     → fsMount()               (exists today)
pkg         + installAfterUpload   → pkgInstall() + poll status
                                     then if deletePkgAfterInstall → fsDelete(pkg)
```

The install call is the existing two-tier logic from `pkgLibrary.install()`,
lifted into a shared helper so both the queue finisher and the legacy
manual-install button call the same code. Status polling reuses
`pkg_install_status`; the queue item shows the BGFT phase the same way it shows
upload bytes.

**3. Serialize installs.** Installs swap/ptrace the payload and can blip
ShellUI, so the queue must run **at most one install at a time** and not start
an install while a transfer is mid-flight on that console (the manual path
already waits for `transfersActive()`). The queue is the right place to enforce
this ordering for free.

**4. UI: fold "Install Package" into Upload.** Picking a `.pkg` in the Upload
flow shows the pkg finisher options inline (the same way picking a folder shows
"Register after upload" and an image shows "Mount after upload"):

- ☑ Install after upload (default on)
- ☑ Delete the .pkg after a successful install (default on — it's just a
  staging copy; the installed title is what's kept)

The standalone Install Package tab can stay as a *library* view (list installed
pkgs, re-install, delete) but the *upload→install* action becomes one queue
item. One mental model: "add it to the queue, it ends up playable."

### Why this is the right shape

- **Reuses the queue's guarantees**: ordering, persistence, retry, throughput,
  cancel, the per-console isolation already built for multi-PS5.
- **No new protocol**: the FTX2 `PKG_INSTALL` / `PKG_INSTALL_STATUS` frames and
  the DPI fallback already exist; we're only changing *when/where* they're
  called (a queue finisher) and adding an auto-delete step (existing
  `fs_delete`).
- **Matches the other finishers** users already understand (register, mount).

### Rollout

1. **DONE** — FW-gated "screen may blink" notice (Q2 mitigation #2):
   `pkgLibrary.install()` sets a `busyNotice` on FW ≥ 12. The install path is
   unchanged (mitigation #1 was rejected — see Q2).
2. **DONE** — auto-install after upload (the functional core of the request).
   Rather than lift the install into a queue finisher first, the smallest
   correct increment hooks the existing `install()` straight into the staging
   upload's completion path: `addAndUpload()` (`pkgLibrary.ts`), after the
   bytes land + `refresh()`, calls `get().install(destPath, host)` when the new
   `autoInstallAfterUpload` setting is on (default ON, opt-out, persisted in
   `installSettings.ts`). `install()` already owns waiting-for-transfers, the
   FW-12 notice, and — with `autoRemoveAfterInstall` — the post-install delete,
   so "upload → installed → staged copy removed" is now one hands-off flow on
   the existing Install Package screen. A toggle sits next to the auto-delete
   toggle (`screens/InstallPackage/index.tsx`, i18n `pkglib.autoInstall`).
   Tests: `installSettings.test.ts` pins the opt-out default + persistence.
3. **PENDING (larger, needs HW install validation)** — fold this into the
   single upload queue as `sourceKind:"pkg"` with finisher flags, so a `.pkg`
   is a first-class queue item alongside folders/images. This is the visual
   "one queue" unification; the *behaviour* it would deliver (auto-install +
   auto-delete, serialized) is already achieved by step 2. Defer until the
   maintainer wants the queue-tab merge, since it touches the critical install
   path and warrants on-console validation that mutates console state.

Steps 1–2 ship the report's actual pain points (FW-12 scare + manual second
click) without touching the HW-verified install mechanism; step 3 is a pure UX
reorganization on top.
