import { invoke } from "./invokeLogged";
import { setScreenWakeReason } from "./androidScreenWake";

// Programmatic keep-awake hold for active transfers.
//
// While any upload / download / install is in flight we ask the OS to
// skip idle + display sleep, so a long transfer doesn't die to the
// machine sleeping mid-stream (the originally-reported failure: a Mac
// idle-sleeping during a multi-hour game upload dropped the FTX2
// connection). The Rust side (`keep_awake.rs`) reference-counts holds by
// reason, so this "transfer" hold is INDEPENDENT of the manual Settings
// toggle ("manual") — releasing it can't turn off a hold the user set in
// Settings, and vice versa.
//
// Two backends, picked by platform:
//   • Desktop — the Rust inhibitor (`keep_awake_acquire/release`), which
//     keeps the machine awake even while minimized.
//   • Android — a screen wake lock (`setScreenWakeReason`); the Rust
//     inhibitor is a no-op there, so we hold the screen on instead, which
//     keeps the app foreground (no Doze) for the upload's duration.
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
    const cmd = desired ? "keep_awake_acquire" : "keep_awake_release";
    try {
      await invoke(cmd, { reason: REASON });
    } catch {
      // Best-effort — keep-awake is a nicety, not a transfer dependency.
    }
  });
}
