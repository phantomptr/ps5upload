import { invoke } from "./invokeLogged";
import { setScreenWakeReason } from "./androidScreenWake";
import { isTauriEnv } from "./tauriEnv";

// Programmatic keep-awake hold for active transfers.
//
// While any upload / download / install is in flight we ask the OS to
// skip idle + display sleep, so a long transfer doesn't die to the
// machine sleeping mid-stream (the originally-reported failure: a Mac
// idle-sleeping during a multi-hour game upload dropped the
// transfer connection). The Rust side (`keep_awake.rs`) reference-counts holds by
// reason, so this "transfer" hold is INDEPENDENT of the manual Settings
// toggle ("manual") — releasing it can't turn off a hold the user set in
// Settings, and vice versa.
//
// Two backends, picked by platform:
//   • Desktop — the Rust inhibitor (`keep_awake_acquire/release`), which
//     keeps the machine awake even while minimized.
//   • Phones — a screen wake lock (`setScreenWakeReason`); the Rust inhibitor is a no-op there,
//     so we hold the screen on instead, while the app is on show. What keeps a transfer going
//     once the app is in the background is Android's foreground service (bg_activity.rs), not
//     this.
// Both are driven from the same edge below so a transfer engages whichever
// applies; the unused one no-ops.
//
// Best-effort by design: a transfer must NEVER fail because the OS
// declined to inhibit sleep (unsupported platform, Windows GPO, no
// systemd), so invoke errors are swallowed.

const REASON = "transfer";

// `desired` is the latest requested state; `current` is what we've
// actually told Rust. We collapse rapid flips and apply changes through
// a single promise chain so the acquire/release IPC calls can't be
// reordered (a release landing before its acquire would leave the
// inhibitor stuck on).
let desired = false;
let current = false;
let chain: Promise<unknown> = Promise.resolve();

/**
 * Engage (`active=true`) or release (`active=false`) the transfer-scoped
 * sleep inhibitor. Idempotent: only the 0↔1 edges hit the backend.
 */
export function setTransferKeepAwake(active: boolean): void {
  // Steady-state collapse: reconcileKeepAwake calls this on every byte-
  // progress tick, so without this guard a long upload would append
  // thousands of no-op promises to the chain. If the intent is unchanged
  // there's nothing to do — any in-flight chain step already drives to
  // `desired`.
  if (active === desired) return;
  desired = active;
  // Android backend (no-op on desktop): hold the screen on for the
  // transfer. Edge-only — `desired` just changed, so this runs on the
  // 0↔1 transition, not every progress tick.
  setScreenWakeReason(REASON, active);
  chain = chain.then(async () => {
    if (desired === current) return;
    current = desired;
    // The inhibitor is a native command with no browser equivalent, and the
    // self-hosted web UI runs outside Tauri. Calling it there throws a
    // "requires the Tauri desktop client" error that we swallow — but it is
    // logged first, so every browser upload seeded a warn into the app log
    // and into any bug report captured during it. Nothing was broken; the
    // noise just cost a reader time. Don't make the call at all.
    if (!isTauriEnv()) return;
    const cmd = desired ? "keep_awake_acquire" : "keep_awake_release";
    try {
      await invoke(cmd, { reason: REASON });
    } catch {
      // Best-effort — keep-awake is a nicety, not a transfer dependency.
    }
  });
}

// ── Background-work hold (#457) ─────────────────────────────────────────
// The host's background watcher (src-tauri/src/bg_activity.rs) keeps the app
// running — on Android, a foreground service — while the engine has a job, and
// also while this hold says work is queued between jobs (an upload queue
// settling before its next item), when the engine has nothing running. The host
// drops a hold not refreshed for 5 minutes, so a renderer frozen while holding
// cannot keep the service up forever: refresh it every minute while held.

const BG_REFRESH_MS = 60_000;
let bgHeld = false;
let bgRefresh: ReturnType<typeof setInterval> | null = null;

function sendBgHold(active: boolean): void {
  // Native command only; the self-hosted web UI has no host (see above).
  if (!isTauriEnv()) return;
  invoke("bg_work_hold", { active }).catch(() => {
    // Best-effort, like the keep-awake hold.
  });
}

/** Hold (`true`) or release the host's background-work hold. Edge-only, plus a
 *  refresh every minute while held. */
export function setBackgroundWorkHold(active: boolean): void {
  if (active === bgHeld) return;
  bgHeld = active;
  if (bgRefresh !== null) {
    clearInterval(bgRefresh);
    bgRefresh = null;
  }
  sendBgHold(active);
  if (active) bgRefresh = setInterval(() => sendBgHold(true), BG_REFRESH_MS);
}
