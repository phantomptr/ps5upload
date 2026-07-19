import { create } from "zustand";

import { openExternalUrl } from "../lib/openExternalUrl";
import {
  updateCheck,
  updateDownload,
  type UpdateCheck,
  type UpdateDownload,
} from "../api/ps5";
import { isMobile } from "../lib/platform";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { t as translate } from "../i18n";
import { useLangStore } from "./lang";
import { pushNotification } from "./notifications";

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
// Auto-check preference + notify-dedup live in localStorage (NOT the
// sessionStorage check cache): the toggle must survive an app restart, and
// the "already told the user about vX" marker must persist so reopening the
// app doesn't re-fire the same update notification every launch.
const AUTOCHECK_KEY = "ps5upload.update.autocheck";
const NOTIFIED_KEY = "ps5upload.update.notified-version";

/** Read the auto-check preference. Defaults to ON when unset (first run) —
 *  the feature is opt-out, not opt-in. */
function loadAutoCheck(): boolean {
  if (typeof localStorage === "undefined") return true;
  return safeGetItem(AUTOCHECK_KEY) !== "0";
}

function saveAutoCheck(enabled: boolean) {
  if (typeof localStorage === "undefined") return;
  safeSetItem(AUTOCHECK_KEY, enabled ? "1" : "0");
}

/** Non-hook translate with inline English fallback — the store runs outside
 *  React so it can't use useTr(). Mirrors the `tr(key, vars, fallback)` shape:
 *  returns the fallback when the active locale (and English) lack the key. */
function trFb(
  key: string,
  vars: Record<string, string | number> | undefined,
  fallback: string,
): string {
  const lang = useLangStore.getState().lang;
  const out = translate(lang, key, vars);
  return out === key ? fallback : out;
}

/** Fire a one-time "update available" notification per version. Returns
 *  without notifying if we've already announced this exact version (across
 *  restarts), so the user isn't pestered on every daily check or launch. */
function notifyAvailableOnce(result: UpdateCheck) {
  if (!result.available || !result.latest_version) return;
  const already = (() => {
    try {
      return typeof localStorage !== "undefined"
        ? safeGetItem(NOTIFIED_KEY)
        : null;
    } catch {
      return null;
    }
  })();
  if (already === result.latest_version) return;
  pushNotification(
    "info",
    trFb(
      "update_notif_title",
      { ver: result.latest_version },
      `Update available — v${result.latest_version}`,
    ),
    {
      body: trFb(
        "update_notif_body",
        undefined,
        "A newer version of PS5Upload is ready. Open Settings → Updates to download it.",
      ),
      link: "/settings",
    },
  );
  try {
    if (typeof localStorage !== "undefined") {
      safeSetItem(NOTIFIED_KEY, result.latest_version);
    }
  } catch {
    // Best-effort dedup; worst case the user sees the notice twice.
  }
}

/** Browser fallback when the manifest has no asset for this platform
 *  (mobile only — the user can grab the APK from the release page). */
const RELEASES_URL = "https://github.com/phantomptr/ps5upload/releases/latest";

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
  /** Whether the app checks for updates automatically on launch. Defaults
   *  to true (opt-out). Persisted to localStorage so it survives restarts. */
  autoCheckEnabled: boolean;
  /** Toggle the auto-check-on-launch preference (Settings → Updates). */
  setAutoCheckEnabled: (enabled: boolean) => void;
  /** Kick a check now if we haven't done one recently. No-op when a
   *  recent result is cached and still inside TTL, or when the user has
   *  turned auto-check off. */
  ensureChecked: () => Promise<void>;
  /** Force a check regardless of cache. Used by the Settings card's
   *  "Check now" button. */
  checkNow: () => Promise<void>;
  /** Download the update archive to ~/Downloads and reveal the folder.
   *  Transitions through `downloading` → `downloaded` (success) or
   *  `download-failed`. Only valid when current phase is `available`.
   *  Mobile: opens the APK download URL (or the GitHub release page)
   *  in the system browser instead — no in-app download. */
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
      // Surface a notification-center entry the first time we see a given
      // new version — the toast bar is dismissible/session-scoped, this is
      // the durable "you have an update" record.
      notifyAvailableOnce(result);
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
    autoCheckEnabled: loadAutoCheck(),

    setAutoCheckEnabled(enabled: boolean) {
      saveAutoCheck(enabled);
      set({ autoCheckEnabled: enabled });
    },

    async ensureChecked() {
      // Respect the user's opt-out: the launch auto-check is the only
      // caller of this; manual "Check now" goes through checkNow().
      if (!get().autoCheckEnabled) return;
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
      // Mobile (Android): the desktop "stream to ~/Downloads + reveal in
      // file manager" flow can't work — Downloads is sandboxed and there
      // is no file manager to reveal into. Hand the APK URL (or the
      // GitHub release page when the manifest has no APK asset) to the
      // system browser instead; Android's own download manager takes it
      // from there. Phase stays `available` so the button remains usable.
      if (isMobile()) {
        const target = result.download_url || RELEASES_URL;
        const ok = await openExternalUrl(target);
        set({
          phase: ok
            ? { kind: "available", result }
            : {
                kind: "download-failed",
                result,
                // openExternalUrl already logged the underlying error; give the
                // user an actionable message + the in-panel browser button.
                message:
                  "Couldn't open the download link. Tap “Get APK in browser”, or grab it from the GitHub releases page.",
              },
        });
        return;
      }
      if (!result.download_url || !result.download_filename) {
        set({
          phase: {
            kind: "download-failed",
            result,
            message: "No download bundle for your platform in this release.",
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
      if (phase.kind === "downloaded" || phase.kind === "download-failed") {
        set({ phase: { kind: "available", result: phase.result } });
      }
    },
  };
});
