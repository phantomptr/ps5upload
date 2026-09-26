import { isAndroid } from "./platform";

// Android screen keep-awake, via the W3C Screen Wake Lock API.
//
// The Rust inhibitor in `keep_awake.rs` (caffeinate / systemd-inhibit /
// SetThreadExecutionState) has NO Android implementation — its
// `acquire_inhibitor()` falls through to `Ok(None)` on Android, so the
// keep-awake commands are silent no-ops there. The Android-native
// equivalent is `navigator.wakeLock.request("screen")`, which holds the
// screen on while the document is visible. Keeping the screen on keeps the
// app in the foreground (Android never Dozes a visible foreground app and
// the screen can't time out into a backgrounded state), so a long upload
// over Wi-Fi doesn't die to the phone sleeping mid-stream — the same
// failure the desktop inhibitor guards against.
//
// Scope (Phase 1): foreground only. This keeps the device awake while the
// app is open and on-screen — the common "I started a big upload and set
// the phone down" case. It does NOT survive a manual screen lock or an
// app switch (the OS hides the document and releases the lock by spec);
// true background uploads would need a foreground service (a deliberate
// follow-up, gated on the Tauri MainActivity-leak caveat — tauri#11609).
//
// Two design points the spec forces:
//   1. The sentinel is auto-released whenever the page is hidden, so we
//      re-acquire on `visibilitychange` → visible while any reason is held.
//   2. Acquisition can reject (no user-visible document, policy) — it's
//      best-effort; a transfer must never fail because the lock didn't take.
//
// Refcounted by reason to mirror `keep_awake.rs`: the automatic "transfer"
// hold (active uploads/installs) and the manual "manual" hold (the Settings
// toggle) coexist; the screen lock stays engaged while ANY reason is held,
// so ending a transfer can't drop a hold the user set in Settings.

interface WakeLockSentinelLike {
  released: boolean;
  release(): Promise<void>;
  addEventListener(type: "release", listener: () => void): void;
}
interface WakeLockLike {
  request(type: "screen"): Promise<WakeLockSentinelLike>;
}

function wakeLockApi(): WakeLockLike | null {
  if (typeof navigator === "undefined") return null;
  const wl = (navigator as Navigator & { wakeLock?: WakeLockLike }).wakeLock;
  return wl ?? null;
}

/** True when this platform can hold a screen wake lock (Android + the
 *  WebView exposes the API). Drives the Settings toggle's `supported`. */
export function screenWakeSupported(): boolean {
  return isAndroid() && wakeLockApi() !== null;
}

const reasons = new Set<string>();
let sentinel: WakeLockSentinelLike | null = null;
// All acquire/release work runs through a single promise chain so a
// release can't land before its acquire and leave the lock stuck.
let chain: Promise<unknown> = Promise.resolve();
let visibilityWired = false;

async function acquire(): Promise<void> {
  if (reasons.size === 0) return; // released out from under us meanwhile
  if (sentinel && !sentinel.released) return; // already held
  const api = wakeLockApi();
  if (!api) return;
  try {
    const s = await api.request("screen");
    // The OS auto-releases on page-hide; drop our ref so the next
    // visibility=visible re-acquires instead of thinking it's still held.
    s.addEventListener("release", () => {
      if (sentinel === s) sentinel = null;
    });
    sentinel = s;
  } catch {
    // Best-effort: no visible document, blocked by policy, etc.
  }
}

async function release(): Promise<void> {
  const s = sentinel;
  sentinel = null;
  if (s && !s.released) {
    try {
      await s.release();
    } catch {
      // Already gone / never fully acquired — nothing to do.
    }
  }
}

function wireVisibility(): void {
  if (visibilityWired || typeof document === "undefined") return;
  visibilityWired = true;
  document.addEventListener("visibilitychange", () => {
    // Re-acquire the spec-mandated auto-release when we come back to the
    // foreground and a reason is still held. Hiding needs no action —
    // the OS already released the lock.
    if (document.visibilityState === "visible" && reasons.size > 0) {
      chain = chain.then(acquire);
    }
  });
}

/**
 * Add (`active=true`) or drop (`active=false`) a named reason for holding
 * the screen awake. The lock is acquired on the empty→non-empty edge and
 * released on non-empty→empty. No-op off Android (desktop uses the native
 * Rust inhibitor, which is strictly better — it works while minimized).
 */
export function setScreenWakeReason(reason: string, active: boolean): void {
  if (!isAndroid()) return;
  wireVisibility();
  const had = reasons.size > 0;
  if (active) reasons.add(reason);
  else reasons.delete(reason);
  const has = reasons.size > 0;
  if (had === has) return; // no edge — nothing to reconcile
  chain = chain.then(has ? acquire : release);
}
