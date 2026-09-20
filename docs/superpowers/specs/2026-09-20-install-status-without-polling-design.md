# Install status without polling Sony

Stop calling `sceAppInstUtilGetInstallStatus` from the payload, and stop
reporting the resulting unverified-but-accepted install as a failure. The two
changes are coupled: either alone makes things worse.

Research for this spec: our own `payload/src/bgft.c` and
`payload/dpi/ezremote_dpi.c`, the engine's install verdict machinery in
`pkg_install.rs`, itsPLK's `ps5-pkg-manager` (`src/installer.c`), and user
reports from Discord (ppeterr's repeated helper crashes on PKG install, a
"was not verified as installed" toast for a 39.6 GiB Ratchet & Clank install).

---

## What the investigation established

### F1 — `sceAppInstUtilGetInstallStatus` kills whatever process calls it

`payload/dpi/ezremote_dpi.c:257` records a controlled A/B on two consoles,
dated 2026-09-12:

> with the poll: stderr stops after `IpcFacade::appInstallByPackage`, the
> "[dpi] InstallByPackage ok" line never prints, :9040 goes dead, and the host
> reads a zero-byte reply → rc 0xffffffff → a *successful* install reported as
> a rejection. 2/2.
> without it: rc 0, daemon stays up. 1/1.

It explicitly retracts the earlier theory that only a cross-process poller is
affected: "That is wrong: it dies in-process too, on both firmwares."

### F2 — three of four AppInstUtil tiers already bypass it; one does not

`bgft_install_status` (`bgft.c:1755`) routes by task-id flag:

| Tier | Flag | Behaviour today |
|---|---|---|
| ShellUI-RPC | `APPINST_VIA_SHELLUI_FLAG` | synthetic DONE, never polls |
| Local-disk (`AppInstallPkg`) | `APPINST_VIA_LOCAL_FLAG` | synthetic DONE, never polls |
| Tier-0 | `APPINST_VIA_TIER0_FLAG` | synthetic DONE, never polls |
| **`InstallByPackage`** | `APPINST_TASK_ID_FLAG` | **calls `appinst_install_status` → polls ~1/s** |

The surviving call site is `bgft.c:866`, guarded by a ShellCore authid swap and
the `sony_api_lock`/`kernel_rw_lock` pair. It is justified by a comment at
`bgft.c:216` asserting that on firmwares where `InstallByPackage` succeeds,
"GetInstallStatus is known-safe". F1 is hardware evidence that this assertion
is wrong. Two files in our own tree reached opposite conclusions; only one of
them ran an experiment.

### F3 — this matches the reported crash

ppeterr: "sending a PKG for install makes the helper crash", repeatedly, across
reboots. His log shows `process_list_get failed: connect to …:9114` →
`helper went DOWN` → `/api/ps5/status 502` — the payload dying, not a network
blip. A ~1 Hz poll during an install is a fresh chance to take the helper down
every second.

Firmware dependence is expected: `authid.h:40` notes ShellCore authid is
required for these calls on FW < 11, while `AppInstallPkg`'s content-copy step
is gated behind SYSTEM authid on FW ≥ 11. Which tier runs, under which token,
varies by firmware — consistent with a crash some users hit every time and
others never see.

### F4 — removing the poll makes every AppInstUtil install "AcceptedUnverified"

The synthetic-DONE bypass reports `phase=DONE, downloaded=0, total=0`. The
engine's `install_verdict` (`pkg_install.rs:2407`) then has only two ways to
reach `Complete`:

* `registered == Some(true)` — the title's `app.pkg` landed on disk; or
* byte-settle — `consumed/expected >= 0.99` **and** writing has gone idle.

Byte-settle only has data when the engine served the bytes (stream installs).
A staged or local-disk install serves nothing, so `consumed` stays 0 and the
only route to `Complete` is registration.

`install_synthetic_done_grace_sec` defaults to **180 s**
(`pkg_install.rs:2023`). After that idle window the verdict becomes
`AcceptedUnverified` and polling **stops** — it is terminal.

### F5 — the UI paints that terminal state as a failure

`pkgLibrary.ts:2284` pushes a `tone: "critical"` toast reading
"<name> was not verified as installed", and `finishTask(taskId, "failed", …)`
marks the row failed with `code: "INSTALL_UNVERIFIED"`.

For a 39.6 GiB staged install, Sony's local copy routinely exceeds 180 s, so
the app declares failure while the console is still installing — exactly the
reported Ratchet & Clank screenshot. Our own field notes say the same thing in
general terms: PS5 installs are async, the app DB lags the filesystem both
ways, and historically every "install failed" of this shape was a successful
install.

**This is why the two changes must ship together.** F4 means removing the poll
routes *every* AppInstUtil install through the path F5 renders as a red
failure.

### F6 — how ps5-pkg-manager avoids the whole problem

`ps5-pkg-manager`'s `installer.c:955-1070` does poll `GetInstallStatus`
successfully, because `pkgmgr.elf` is a standalone daemon in its own process
rather than a payload injected into a hijacked one. We cannot copy that; it is
architectural. What we *can* copy is the rest of its model:

* progress comes from **bytes its own range server served**
  (`stream_served_bytes`), treated as authoritative, with any system-reported
  figure clamped to it — explicitly because base and patch share a
  `content_id`, so an early poll otherwise imports the base's size and fakes
  100%;
* completion is verified **kind-aware against app.db**: DLC by `content_id`,
  update by `installed_ver >= expected`, base by presence;
* verification starts 3 s after the stream reaches 100%, and keeps retrying.

Our engine already does the equivalent of the first two. What it lacks is the
third: it gives up rather than keeps retrying.

We also checked whether our `AppInstStatus` struct was undersized — a stack
smash would have explained the segfaults and been trivially fixable. It is not:
ours is field-identical to theirs and marginally wider (`int is_copy_only` vs
`bool`). Ruled out.

---

## Design

### 1. Delete the last poll

Route the `APPINST_TASK_ID_FLAG` tier through the same synthetic-DONE bypass
the other three tiers use, and delete `appinst_install_status` along with the
`sceAppInstUtilGetInstallStatus` extern declaration, so the call cannot be
reintroduced by accident.

Reconcile the contradictory comments: `bgft.c:216`'s "known-safe" claim is
replaced by a pointer to `ezremote_dpi.c:257`'s A/B result, and the reasoning
for why no tier polls is stated once, in one place.

The task-table slot must be released on this path exactly as the other bypasses
do (`appinst_task_release`), or the 16-slot table saturates after 16 installs.

### 2. Keep verifying after the spinner stops

`AcceptedUnverified` currently ends the row. Instead it becomes a
**non-terminal, non-failing** state: the install is "still finishing on the
console", and a low-frequency background re-check keeps asking whether the
title registered.

* The re-check reuses the existing registration probe — no new payload RPC.
* Cadence: every 30 s, for up to 30 minutes, then stop and leave the row in the
  unverified state with a manual **Recheck** action. A 100 GiB install on a slow
  internal drive is the case this must not give up on.
* On registration the row flips to done exactly as a normal completion would,
  and the staged package becomes eligible for the usual cleanup.
* The 180 s grace is unchanged in meaning — it still marks when we stop
  *blocking the UI* — but no longer marks when we stop *caring*.

### 3. Stop calling it a failure

`INSTALL_UNVERIFIED` is not an error state:

* the task finishes as a distinct `unverified` outcome, not `failed`;
* the toast becomes informational, not `critical`, and says the install was
  accepted and is still finishing on the console, with a pointer to the PS5's
  Notifications/Downloads and a **Recheck** action;
* `INSTALL_STALLED` and `INSTALL_FAILED` keep their current failing treatment —
  only the unverified case changes.

New i18n keys are added to `en.ts` and allowlisted for other locales.

### What this deliberately does not do

* No new install method, no new transport, no change to which tier is chosen.
* No attempt to poll Sony from anywhere else, including the DPI daemon — F1
  covers that case explicitly.
* No change to stream installs' byte-settle path, which already works.

---

## Testing

**Host, no console.** `install_verdict`'s treatment of the unverified state is
already unit-tested in `pkg_install.rs`; extend those tests to pin that
`AcceptedUnverified` is non-terminal and that registration after the grace
still yields `Complete`. The client's task-outcome mapping gets a test that
`INSTALL_UNVERIFIED` does not produce a failed task.

**Payload.** `make payload` must build clean with the extern removed; a grep
for `GetInstallStatus` in `payload/` must return only comments.

**Hardware — the part that actually decides this.** On the Pro (9.60) and Phat
(5.10):

1. Install a large PKG (≥ 30 GiB) by the staged path. The helper must stay up
   for the whole install — this is ppeterr's crash, and its absence is the
   headline result.
2. The row must not go red at 180 s. It must read "still finishing", then flip
   to done when the title registers.
3. Repeat with a DLC and with a patch, since those exercise the shared-
   `content_id` case that F6 warns about.
4. Confirm `stderr.log` contains no `GetInstallStatus` line at all.

**Unverifiable from here:** ppeterr's firmware is unknown. F3 is a strong
match, not a confirmed diagnosis. If his crash survives this change, the cause
is elsewhere and the next step is his `stderr.log`.
