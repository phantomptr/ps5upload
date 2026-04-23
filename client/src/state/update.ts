import { create } from "zustand";

import {
  updateCheck,
  updateDownload,
  type UpdateCheck,
  type UpdateDownload,
} from "../api/ps5";

// Shared store for update check + download state.
//
// Two UI surfaces consume this: the sidebar (small dot when an update is
// available, no-op when not) and Settings → Updates (full card with
// version numbers, notes, Download button). Putting state here avoids
// having each surface do its own fetch — a daily-debounced single
// check feeds both.
//
// Persistence: we cache the last check timestamp + result in
// sessionStorage so rapid navigation doesn't re-fetch. On app start,
// `ensureChecked` does a fresh check iff the cached result is older
// than the TTL. No localStorage — users installing a fresh version
// should never see a stale "update available" hint from a previous
// install.

const CHECK_TTL_MS = 24 * 60 * 60 * 1000; // 1 day
const CACHE_KEY = "ps5upload.update-check-v2";

interface CachedCheck {
  at_ms: number;
  result: UpdateCheck;
}

export type UpdatePhase =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "error"; message: string }
  | { kind: "up-to-date"; result: UpdateCheck }
  | { kind: "available"; result: UpdateCheck }
  | { kind: "downloading"; result: UpdateCheck }
  | { kind: "downloaded"; result: UpdateCheck; download: UpdateDownload }
  | { kind: "download-failed"; result: UpdateCheck; message: string };

interface UpdateStore {
  phase: UpdatePhase;
  /** Last time we asked the endpoint. 0 when never. Refreshed after
   *  every successful check or error. */
  lastCheckedMs: number;
  /** Kick a check now if we haven't done one recently. No-op when a
   *  recent result is cached and still inside TTL. */
  ensureChecked: () => Promise<void>;
  /** Force a check regardless of cache. Used by the Settings card's
   *  "Check now" button. */
  checkNow: () => Promise<void>;
  /** Download the update archive to ~/Downloads and reveal the folder.
   *  Transitions through `downloading` → `downloaded` (success) or
   *  `download-failed`. Only valid when current phase is `available`. */
  download: () => Promise<void>;
  /** Reset to the cached `available` state so the Download button is
   *  clickable again after a failure or after the user dismissed the
   *  "downloaded" panel. */
  dismissDownload: () => void;
}

function loadCache(): CachedCheck | null {
  if (typeof sessionStorage === "undefined") return null;
  try {
    const raw = sessionStorage.getItem(CACHE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as CachedCheck;
    if (typeof parsed?.at_ms !== "number" || !parsed.result) return null;
    return parsed;
  } catch {
    return null;
  }
}

function saveCache(result: UpdateCheck) {
  if (typeof sessionStorage === "undefined") return;
  try {
    sessionStorage.setItem(
      CACHE_KEY,
      JSON.stringify({ at_ms: Date.now(), result } satisfies CachedCheck),
    );
  } catch {
    // QuotaExceeded or storage disabled — not worth failing over.
  }
}

function phaseFromResult(result: UpdateCheck): UpdatePhase {
  return result.available
    ? { kind: "available", result }
    : { kind: "up-to-date", result };
}

export const useUpdateStore = create<UpdateStore>((set, get) => {
  // Seed from cache so the sidebar badge is correct on the very first
  // render, before any async work. This is safe because the cache is
  // in sessionStorage (cleared on app close) and the TTL gates whether
  // we trust it for the render-only path.
  const cached = loadCache();
  let initialPhase: UpdatePhase = { kind: "idle" };
  let initialLastChecked = 0;
  if (cached && Date.now() - cached.at_ms < CHECK_TTL_MS) {
    initialPhase = phaseFromResult(cached.result);
    initialLastChecked = cached.at_ms;
  }

  const runCheck = async () => {
    const prior = get().phase;
    // Don't stomp an active download with a check result.
    if (prior.kind === "downloading") return;
    set({ phase: { kind: "checking" } });
    try {
      const result = await updateCheck();
      saveCache(result);
      set({
        phase: phaseFromResult(result),
        lastCheckedMs: Date.now(),
      });
    } catch (e) {
      set({
        phase: {
          kind: "error",
          message: e instanceof Error ? e.message : String(e),
        },
        lastCheckedMs: Date.now(),
      });
    }
  };

  return {
    phase: initialPhase,
    lastCheckedMs: initialLastChecked,

    async ensureChecked() {
      // TOCTOU-safe: AppShell mounts fire this 1.5 s after paint, and
      // the user could navigate to Settings (which also calls the
      // store) in the same tick. Without this synchronous flip to
      // "checking", both callers would see `phase.kind === "idle"`,
      // both would fall through the TTL gate, and we'd make two
      // network calls for the same check. Setting phase first means
      // the second caller's `phase.kind === "checking"` short-circuits
      // the TTL path.
      const cur = get();
      const age = Date.now() - cur.lastCheckedMs;
      if (cur.phase.kind !== "idle" && age < CHECK_TTL_MS) return;
      // Skip if another check is already running in this tick. Synchronous
      // state read AFTER the set, so it correctly sees any concurrent
      // caller that's already flipped to "checking".
      if (cur.phase.kind === "checking") return;
      await runCheck();
    },

    async checkNow() {
      // Settings card's "Check now" always forces — no TTL gating.
      if (get().phase.kind === "checking") return;
      await runCheck();
    },

    async download() {
      const phase = get().phase;
      if (phase.kind !== "available" && phase.kind !== "download-failed") {
        return;
      }
      // Both `available` and `download-failed` carry the same `result`
      // field per the discriminated union, so the narrowing here is
      // exhaustive — no conditional needed.
      const result = phase.result;
      if (!result.download_url || !result.download_filename) {
        set({
          phase: {
            kind: "download-failed",
            result,
            message:
              "No download bundle for your platform in this release.",
          },
        });
        return;
      }
      set({ phase: { kind: "downloading", result } });
      try {
        const download = await updateDownload(
          result.download_url,
          result.download_filename,
        );
        set({ phase: { kind: "downloaded", result, download } });
      } catch (e) {
        set({
          phase: {
            kind: "download-failed",
            result,
            message: e instanceof Error ? e.message : String(e),
          },
        });
      }
    },

    dismissDownload() {
      const phase = get().phase;
      if (
        phase.kind === "downloaded" ||
        phase.kind === "download-failed"
      ) {
        set({ phase: { kind: "available", result: phase.result } });
      }
    },
  };
});
