import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { openExternalUrl } from "../lib/openExternalUrl";
import {
  UPDATE_DOWNLOAD_PROGRESS_EVENT,
  updateCheck,
  updateDownload,
  updateDownloadCancel,
  updateInstall,
  updateInstallOnQuit,
  updateLastInstall,
  updateForgetFailure,
  updateOldDownloads,
  updateRemoveOldDownloads,
  updateCanSelfInstall,
  type LastInstall,
  type UpdateCheck,
  type UpdateDownload,
  type UpdateDownloadProgress,
} from "../api/ps5";
import { isIOS, isMobile } from "../lib/platform";
import { shellErrorCode, shellErrorText } from "../lib/shellError";
import { isTauriEnv } from "../lib/tauriEnv";
import { apkUpdaterAvailable, downloadAndInstallApk } from "./apkUpdate";
import { safeGetItem, safeRemoveItem, safeSetItem } from "../lib/safeStorage";
import { t as translate } from "../i18n";
import { useLangStore } from "./lang";
import { pushNotification, useNotificationsStore } from "./notifications";
import { appIsForeground, sendOsNotification } from "../lib/osNotify";
import { log } from "./logs";

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

/** A phone could not open the download link. */
function mobileLinkFailedMessage(): string {
  const lang = useLangStore.getState().lang;
  const say = (key: string, fallback: string) => {
    const out = translate(lang, key, undefined);
    return out === key ? fallback : out;
  };
  return isIOS()
    ? say(
        "update_ios_link_failed",
        "Couldn't open the download link. Get the new .ipa from the GitHub releases page and install it with your sideloading app.",
      )
    : say(
        "update_apk_link_failed",
        "Couldn't open the download link. Tap “Get APK in browser”, or grab it from the GitHub releases page.",
      );
}

const CHECK_TTL_MS = 24 * 60 * 60 * 1000; // 1 day
const CACHE_KEY = "ps5upload.update-check-v2";
// Auto-check preference + notify-dedup live in localStorage (NOT the
// sessionStorage check cache): the toggle must survive an app restart, and
// the "already told the user about vX" marker must persist so reopening the
// app doesn't re-fire the same update notification every launch.
const AUTOCHECK_KEY = "ps5upload.update.autocheck";
// Which releases the updater will offer. Every release is published as a
// pre-release and promoted by hand once it has been checked on hardware, so
// "stable" means a human signed it off. Default stable; opting in is explicit
// and must survive a restart, hence localStorage rather than the session cache.
const CHANNEL_KEY = "ps5upload.update.channel";
const NOTIFIED_KEY = "ps5upload.update.notified-version";
// When that notice was posted (ms). Releases can come several a day: at most one notice a day,
// a newer version inside that day only updates the bar and the sidebar dot.
const NOTIFIED_AT_KEY = "ps5upload.update.notified-at";
const NOTIFY_EVERY_MS = 24 * 60 * 60 * 1000;
// The version the "installs when you quit from the tray" hint was shown for (once per version).
const TRAY_HINT_KEY = "ps5upload.update.tray-hint-version";
// Install updates without asking (desktop copies that can, commands/self_install.rs). Off unless
// the user turns it on: it quits and restarts the app.
const AUTOINSTALL_KEY = "ps5upload.update.autoinstall";
/** How often an automatic install waiting for the app to be idle looks again. */
const IDLE_RECHECK_MS = 60_000;
// Install a downloaded update when the app quits (no restart). On unless turned off ("0").
const INSTALL_ON_QUIT_KEY = "ps5upload.update.install-on-quit";
// A version the user chose to skip: not offered in the banner, the sidebar dot or the
// automatic install until a newer one comes out. Settings still shows it.
const SKIPPED_KEY = "ps5upload.update.skipped-version";

function loadInstallOnQuit(): boolean {
  if (typeof localStorage === "undefined") return true;
  return safeGetItem(INSTALL_ON_QUIT_KEY) !== "0";
}

function loadSkipped(): string | null {
  if (typeof localStorage === "undefined") return null;
  return safeGetItem(SKIPPED_KEY) || null;
}

function loadAutoInstall(): boolean {
  if (typeof localStorage === "undefined") return false;
  return safeGetItem(AUTOINSTALL_KEY) === "1";
}

/** Nothing would be cut short by a restart: no queue running and no one-off transfer. Read
 *  lazily so this store does not pull the transfer stores in at start. */
/** Quick lookups that never hold the update back. */
const LIGHT_TASKS = new Set(["icon-fetch", "cheat-download"]);

/** Whether a task in the Activity list is moving data (an upload, copy, install, backup…):
 *  started from any screen, not only the upload queue (#660). */
export function busyTasks(tasks: ReadonlyArray<{ kind: string; status: string }>): boolean {
  return tasks.some((t) => (t.status === "running" || t.status === "queued") && !LIGHT_TASKS.has(t.kind));
}

export async function appIdle(): Promise<boolean> {
  const [
    { useUploadQueueStore },
    { useTransferStore },
    { useTaskStore },
    { useRemotePlayStore },
    { activeJoin },
    { useGoLiveStore },
    { isLive },
  ] = await Promise.all([
    import("./uploadQueue"),
    import("./transfer"),
    import("./tasks"),
    import("./remotePlay"),
    import("./joinSession"),
    import("./goLive"),
    import("../lib/goLive"),
  ]);
  if (useUploadQueueStore.getState().running) return false;
  if (busyTasks(useTaskStore.getState().tasks)) return false;
  // Someone is playing or broadcasting, and no task says so: the restart would end the stream
  // (a host's Play Together friends with it), a friend's game, or the broadcast.
  if (useRemotePlayStore.getState().stream) return false;
  if (activeJoin()) return false;
  if (isLive(useGoLiveStore.getState().status?.live)) return false;
  return !Object.values(useTransferStore.getState().phasesByHost).some(
    (p) => p.kind === "starting" || p.kind === "running",
  );
}

/** How often an app left running looks whether its daily check is due. */
const CHECK_AGAIN_MS = 60 * 60 * 1000;

/** Runs the TTL-gated check now, every hour, and when the window comes back to the front (a
 *  hidden window's timers can be held up for as long as it stays hidden). The tray keeps the
 *  app running for days, and one check per launch would never see a release. Returns a stop. */
export function startUpdateChecks(ensureChecked: () => Promise<void> | void): () => void {
  const check = () => void ensureChecked();
  check();
  const timer = setInterval(check, CHECK_AGAIN_MS);
  const onFront = () => {
    if (document.visibilityState === "visible") check();
  };
  document.addEventListener("visibilitychange", onFront);
  window.addEventListener("focus", onFront);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onFront);
    window.removeEventListener("focus", onFront);
  };
}

/** Read the auto-check preference. Defaults to ON when unset (first run) —
 *  the feature is opt-out, not opt-in. */
function loadAutoCheck(): boolean {
  if (typeof localStorage === "undefined") return true;
  return safeGetItem(AUTOCHECK_KEY) !== "0";
}

export type UpdateChannel = "stable" | "prerelease";

/** Read the update channel. Anything unrecognised — absent, corrupt, written
 *  by an older build — means stable: an unknown value must never silently opt
 *  someone into pre-releases. */
function loadChannel(): UpdateChannel {
  if (typeof localStorage === "undefined") return "stable";
  return safeGetItem(CHANNEL_KEY) === "prerelease" ? "prerelease" : "stable";
}

function saveChannel(channel: UpdateChannel) {
  if (typeof localStorage === "undefined") return;
  safeSetItem(CHANNEL_KEY, channel);
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

/** Whether to post the "update available" notice for `version` now: never twice for one
 *  version, and at most once a day (`NOTIFY_EVERY_MS`) whatever the version, so three releases
 *  in a day are one notice. A clock set back past the last notice does not hold it off. */
export function shouldNotifyUpdate(
  version: string,
  lastVersion: string | null,
  lastAtMs: number | null,
  nowMs: number,
): boolean {
  if (!version || version === lastVersion) return false;
  if (lastAtMs == null || !Number.isFinite(lastAtMs) || lastAtMs > nowMs) return true;
  return nowMs - lastAtMs >= NOTIFY_EVERY_MS;
}

/** The notification-center entry for an update (see `shouldNotifyUpdate`). `ready`: it was
 *  downloaded in the background and installs when the app quits. */
function notifyUpdate(result: UpdateCheck, ready: boolean) {
  if (!result.available || !result.latest_version) return;
  const ver = result.latest_version;
  const lastAt = Number(safeGetItem(NOTIFIED_AT_KEY) ?? "");
  if (
    !shouldNotifyUpdate(
      ver,
      safeGetItem(NOTIFIED_KEY),
      Number.isFinite(lastAt) && lastAt > 0 ? lastAt : null,
      Date.now(),
    )
  ) {
    return;
  }
  if (ready) {
    pushNotification(
      "info",
      trFb("update_notif_ready_title", { ver }, `v${ver} is ready`),
      {
        body: trFb(
          "update_notif_ready_body",
          undefined,
          "Downloaded in the background. It installs when you quit PS5Upload, or choose Restart now.",
        ),
        link: "/settings",
      },
    );
  } else {
    pushNotification(
      "info",
      trFb("update_notif_title", { ver }, `Update available — v${ver}`),
      {
        body: trFb(
          "update_notif_body",
          undefined,
          "A newer version of PS5Upload is ready. Open Settings → Updates to download it.",
        ),
        link: "/settings",
      },
    );
  }
  // Best effort; worst case the user sees the notice twice.
  safeSetItem(NOTIFIED_KEY, ver);
  safeSetItem(NOTIFIED_AT_KEY, String(Date.now()));
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

/** The update the banner, the sidebar dot and the automatic install offer: an available one
 *  the user has not skipped. Settings shows every phase regardless. */
export function updateOffered(s: { phase: UpdatePhase; skippedVersion: string | null }): boolean {
  return s.phase.kind === "available" && s.phase.result.latest_version !== s.skippedVersion;
}

/** Download an offered update in the background (desktop), so "install when I quit" needs no
 *  click: only with automatic checks and install-on-quit on, and not when the automatic install
 *  is on (that downloads it itself), the release has no file for this platform, or this version
 *  already failed to install. Whether this copy installs it without a password is asked of the
 *  app separately (`updateCanSelfInstall`), before anything is downloaded. */
export function backgroundDownloadWanted(
  s: Pick<
    UpdateStore,
    | "phase"
    | "skippedVersion"
    | "lastInstall"
    | "autoCheckEnabled"
    | "installOnQuitEnabled"
    | "autoInstallEnabled"
  >,
): boolean {
  if (!updateOffered(s) || s.phase.kind !== "available") return false;
  if (s.autoInstallEnabled || !s.autoCheckEnabled || !s.installOnQuitEnabled) return false;
  const r = s.phase.result;
  if (!r.download_url || !r.download_filename) return false;
  return !failedInstall(s, r.latest_version);
}

/** The version to give the one-time "installs when you quit from the tray" hint for, when the
 *  window was just closed to the tray: an update armed for the quit not hinted before. */
export function trayHintVersion(
  s: Pick<UpdateStore, "phase" | "installOnQuitArmed">,
  hinted: string | null,
): string | null {
  if (!s.installOnQuitArmed || s.phase.kind !== "downloaded") return null;
  const v = s.phase.result.latest_version;
  return v && v !== hinted ? v : null;
}

/** The failed install of `version`, if the last attempt to install exactly that version
 *  failed (#611): it is then not installed by itself again, only by "Try again". */
export function failedInstall(
  s: { lastInstall: LastInstall | null },
  version: string | null | undefined,
): LastInstall | null {
  const l = s.lastInstall;
  return l && !l.installed && version && l.version === version ? l : null;
}

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
  /** Which releases to offer. Persisted; defaults to "stable". */
  channel: UpdateChannel;
  /** Switch channel. Clears the cached result and re-checks immediately —
   *  the cached answer belongs to the other channel and would otherwise sit
   *  there looking authoritative. */
  setChannel: (channel: UpdateChannel) => void;
  /** Kick a check now if we haven't done one recently. No-op when a
   *  recent result is cached and still inside TTL, or when the user has
   *  turned auto-check off. */
  ensureChecked: () => Promise<void>;
  /** Force a check regardless of cache. Used by the Settings card's
   *  "Check now" button. */
  checkNow: () => Promise<void>;
  /** Download the update archive (the app's updates folder; ~/Downloads, revealed, when it is
   *  installed by hand).
   *  Transitions through `downloading` → `downloaded` (success) or
   *  `download-failed`. Only valid when current phase is `available`.
   *  Mobile: opens the APK download URL (or the GitHub release page)
   *  in the system browser instead — no in-app download. */
  download: () => Promise<void>;
  /** How far the download in progress is (desktop), once it has started. */
  downloadProgress: UpdateDownloadProgress | null;
  /** Stop the download in progress and delete what it saved: back to `available`. */
  cancelDownload: () => Promise<void>;
  /** Reset to the cached `available` state so the Download button is
   *  clickable again after a failure or after the user dismissed the
   *  "downloaded" panel. */
  dismissDownload: () => void;
  /** Install the downloaded update and restart into it. Only when the download says this copy
   *  can (`download.can_install`); otherwise the user installs it by hand. */
  install: () => Promise<void>;
  /** An install has started: the app is about to quit. */
  installing: boolean;
  /** Install new releases by themselves (download, wait until nothing transfers, install,
   *  restart). Off by default; desktop copies that can install only. */
  autoInstallEnabled: boolean;
  setAutoInstallEnabled: (enabled: boolean) => void;
  /** Install a downloaded update when the app quits, without starting it again. On by
   *  default; desktop copies that install without a password only. */
  installOnQuitEnabled: boolean;
  setInstallOnQuitEnabled: (enabled: boolean) => void;
  /** The downloaded update is armed to install when the app quits (nothing transfers). */
  installOnQuitArmed: boolean;
  /** Why "Install and restart" fell back to the command to copy (a .deb / .rpm whose
   *  password prompt was closed, could not be shown, or whose install failed). */
  installFallback: string | null;
  /** The version the user skipped (persisted), or null. */
  skippedVersion: string | null;
  /** Skip the version on offer: no banner, dot or automatic install for it. */
  skipVersion: () => void;
  /** Offer the skipped version again. */
  unskipVersion: () => void;
  /** What the last install came to (read once at launch), or after a failed install in this
   *  session. A failed version is not installed by itself again (`failedInstall`). */
  lastInstall: LastInstall | null;
  /** Read the last install's outcome and the old update downloads in Downloads (once). */
  loadInstallStatus: () => Promise<void>;
  /** After a failed install: forget it, download again if needed, and install. */
  retryInstall: () => Promise<void>;
  /** How many old update downloads earlier versions left in the Downloads folder. */
  oldDownloads: number;
  /** Delete them (the person clicked Remove). */
  removeOldDownloads: () => Promise<void>;
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
      const result = await updateCheck(get().channel);
      saveCache(result);
      set({
        phase: phaseFromResult(result),
        lastCheckedMs: Date.now(),
      });
      // A notification-center entry (at most one a day, `shouldNotifyUpdate`): the toast bar is
      // dismissible and session-scoped, this is the durable "you have an update" record. An
      // update downloaded in the background says so once it is ready instead.
      const offered = updateOffered(get());
      if (offered && get().autoInstallEnabled && !isMobile()) {
        notifyUpdate(result, false);
        void autoInstall();
      } else if (offered && isTauriEnv() && !isMobile() && backgroundDownloadWanted(get())) {
        void backgroundDownload();
      } else if (offered) {
        notifyUpdate(result, false);
      }
    } catch (e) {
      set({
        phase: {
          kind: "error",
          message: shellErrorText(e instanceof Error ? e.message : String(e), trFb),
        },
        lastCheckedMs: Date.now(),
      });
    }
  };

  let backgroundRunning = false;
  /** Download the offered update in the background, once nothing transfers, when this copy
   *  installs it on quit by itself (`backgroundDownloadWanted`, `updateCanSelfInstall`). Says
   *  so once it is ready ("installs when you quit"); otherwise the usual "update available". A
   *  failed download goes back to plain "available": nobody clicked Download. */
  const backgroundDownload = async () => {
    const start = get().phase;
    if (backgroundRunning || start.kind !== "available") return;
    backgroundRunning = true;
    const result = start.result;
    const version = result.latest_version;
    const stillWanted = () => {
      const p = get().phase;
      return p.kind === "available" && p.result.latest_version === version && backgroundDownloadWanted(get());
    };
    // A job that stepped aside (a new check, a skip, a click on Download) leaves the notice to
    // whoever took over; otherwise it says what came of it.
    let steppedAside = false;
    const aside = () => {
      steppedAside = true;
    };
    try {
      if (statusLoaded) await statusLoaded;
      if (!stillWanted()) return aside();
      let quiet = false;
      try {
        quiet = await updateCanSelfInstall(result.download_filename);
      } catch {
        // An app without the command: the person downloads it as before.
      }
      if (!quiet) return;
      // Never while something transfers: the download would compete with it.
      while (!(await appIdle())) {
        await new Promise((r) => setTimeout(r, IDLE_RECHECK_MS));
        if (!stillWanted()) return aside();
      }
      if (!stillWanted()) return aside();
      await get().download();
      const p = get().phase;
      if (p.kind === "download-failed" && p.result.latest_version === version) {
        log.warn("update", `background download of v${version} failed: ${p.message}`);
        set({ phase: { kind: "available", result: p.result } });
      }
    } finally {
      backgroundRunning = false;
      const p = get().phase;
      const ready =
        p.kind === "downloaded" &&
        p.result.latest_version === version &&
        !!p.download.can_install &&
        !p.download.needs_password;
      if (!steppedAside && version !== get().skippedVersion) notifyUpdate(result, ready);
    }
  };

  /** Settles once the last install's outcome is known: the automatic install waits for it. */
  let statusLoaded: Promise<void> | null = null;
  let autoInstallRunning = false;
  /** Download, wait for the app to be idle, install. One at a time; gives up quietly (the
   *  update stays offered as usual) when this copy cannot install by itself. */
  const autoInstall = async () => {
    if (autoInstallRunning) return;
    autoInstallRunning = true;
    try {
      if (statusLoaded) await statusLoaded;
      // #611: an install of this version already failed. Trying again by itself would close the
      // app on every launch; the person decides ("Try again").
      if ("result" in get().phase) {
        const p = get().phase as Extract<UpdatePhase, { result: UpdateCheck }>;
        if (failedInstall(get(), p.result.latest_version)) return;
      }
      if (get().phase.kind === "available") await get().download();
      const phase = get().phase;
      // A .deb / .rpm asks for a password: a prompt out of nowhere is not "automatic".
      if (phase.kind !== "downloaded" || !phase.download.can_install || phase.download.needs_password) {
        return;
      }
      if (phase.result.latest_version === get().skippedVersion) return;
      while (get().autoInstallEnabled && !(await appIdle())) {
        await new Promise((r) => setTimeout(r, IDLE_RECHECK_MS));
      }
      if (!get().autoInstallEnabled) return;
      pushNotification(
        "info",
        trFb(
          "update_autoinstall_now",
          { ver: phase.result.latest_version },
          `Installing v${phase.result.latest_version}: PS5Upload restarts in a moment`,
        ),
      );
      await get().install();
    } finally {
      autoInstallRunning = false;
    }
  };

  return {
    phase: initialPhase,
    lastCheckedMs: initialLastChecked,
    downloadProgress: null,
    installing: false,
    autoInstallEnabled: loadAutoInstall(),
    installOnQuitEnabled: loadInstallOnQuit(),
    installOnQuitArmed: false,
    installFallback: null,
    skippedVersion: loadSkipped(),
    lastInstall: null,
    oldDownloads: 0,

    loadInstallStatus() {
      statusLoaded ??= (async () => {
        try {
          const last = await updateLastInstall();
          if (last) set({ lastInstall: last });
          if (last && !last.installed && last.new) {
            pushNotification(
              "warning",
              trFb("update_install_failed_title", undefined, "Update didn't install"),
              {
                body: trFb(
                  "update_install_failed_body",
                  { version: last.version },
                  `v${last.version} couldn't be installed, so PS5Upload is still on the old version.`,
                ),
                link: "/settings",
              },
            );
          }
        } catch {
          // No record to read: nothing to say.
        }
        try {
          set({ oldDownloads: (await updateOldDownloads()).count });
        } catch {
          // No Downloads folder: nothing to offer.
        }
      })();
      return statusLoaded;
    },

    async retryInstall() {
      try {
        await updateForgetFailure();
      } catch {
        // Nothing recorded: trying again needs nothing forgotten.
      }
      set({ lastInstall: null });
      if (get().phase.kind === "available" || get().phase.kind === "download-failed") {
        await get().download();
      }
      const phase = get().phase;
      if (phase.kind === "downloaded" && phase.download.can_install && !phase.download.needs_password) {
        await get().install();
      }
    },

    async removeOldDownloads() {
      try {
        await updateRemoveOldDownloads();
      } finally {
        try {
          set({ oldDownloads: (await updateOldDownloads()).count });
        } catch {
          set({ oldDownloads: 0 });
        }
      }
    },

    setInstallOnQuitEnabled(enabled: boolean) {
      if (typeof localStorage !== "undefined") safeSetItem(INSTALL_ON_QUIT_KEY, enabled ? "1" : "0");
      set({ installOnQuitEnabled: enabled });
    },

    skipVersion() {
      const phase = get().phase;
      if (!("result" in phase) || !phase.result.available) return;
      const version = phase.result.latest_version;
      if (typeof localStorage !== "undefined") safeSetItem(SKIPPED_KEY, version);
      set({ skippedVersion: version });
    },

    unskipVersion() {
      if (typeof localStorage !== "undefined") safeRemoveItem(SKIPPED_KEY);
      set({ skippedVersion: null });
    },

    setAutoInstallEnabled(enabled: boolean) {
      if (typeof localStorage !== "undefined") safeSetItem(AUTOINSTALL_KEY, enabled ? "1" : "0");
      set({ autoInstallEnabled: enabled });
      const phase = get().phase;
      if (enabled && !isMobile() && (phase.kind === "available" || phase.kind === "downloaded")) {
        void autoInstall();
      }
    },

    async install() {
      const phase = get().phase;
      if (phase.kind !== "downloaded" || !phase.download.can_install || get().installing) return;
      set({ installing: true, installFallback: null });
      try {
        await updateInstall(phase.download.path, phase.result.latest_version);
        // The app quits from here; nothing else to do.
      } catch (e) {
        const message = shellErrorText(e instanceof Error ? e.message : String(e), trFb);
        // A .deb / .rpm through pkexec: the prompt was closed, could not be shown, or the
        // package manager failed. Nothing changed; show the command to run instead.
        if (phase.download.needs_password) {
          set({
            installing: false,
            installFallback: message,
            phase: {
              kind: "downloaded",
              result: phase.result,
              download: { ...phase.download, can_install: false, needs_password: false },
            },
          });
          return;
        }
        // The download is fine; the install is what failed, and the app is still open on the
        // old version. Not tried again by itself (#611): "Try again" or the download page.
        set({
          installing: false,
          lastInstall: { version: phase.result.latest_version, installed: false, message, new: false },
        });
      }
    },
    autoCheckEnabled: loadAutoCheck(),
    channel: loadChannel(),

    setAutoCheckEnabled(enabled: boolean) {
      saveAutoCheck(enabled);
      set({ autoCheckEnabled: enabled });
    },

    setChannel(channel: UpdateChannel) {
      if (get().channel === channel) return;
      saveChannel(channel);
      // Drop the cached result: it answered a different question.
      try {
        sessionStorage.removeItem(CACHE_KEY);
      } catch {
        /* private mode / storage disabled — the TTL check just re-fetches */
      }
      set({ channel, phase: { kind: "idle" }, lastCheckedMs: 0 });
      void get().checkNow();
    },

    async ensureChecked() {
      // Respect the user's opt-out: the automatic checks are the only
      // callers of this; manual "Check now" goes through checkNow().
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
      // An app left running comes back here later: a download in progress or in hand stands.
      if (cur.phase.kind === "downloading" || cur.phase.kind === "downloaded") return;
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
        // Android with the installer bridge: download here and open Android's
        // "Update?" dialog (state/apkUpdate.ts). The browser below stays the
        // fallback, and the way for everything else.
        if (apkUpdaterAvailable() && result.download_url) {
          set({ phase: { kind: "downloading", result } });
          const error = await downloadAndInstallApk(result);
          set({
            phase: error
              ? { kind: "download-failed", result, message: error }
              : { kind: "available", result },
          });
          return;
        }
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
                message: mobileLinkFailedMessage(),
              },
        });
        return;
      }
      if (!result.download_url || !result.download_filename) {
        set({
          phase: {
            kind: "download-failed",
            result,
            message: trFb(
              "update_no_platform_bundle",
              undefined,
              "No download for your platform in this release.",
            ),
          },
        });
        return;
      }
      set({ phase: { kind: "downloading", result }, downloadProgress: null });
      let unlisten: (() => void) | undefined;
      try {
        unlisten = await listen<UpdateDownloadProgress>(UPDATE_DOWNLOAD_PROGRESS_EVENT, (e) => {
          set({ downloadProgress: e.payload });
        });
      } catch {
        // No progress events: the download still runs, the bar just stays empty.
      }
      try {
        const download = await updateDownload(
          result.download_url,
          result.download_filename,
          result.sha256,
        );
        set({ phase: { kind: "downloaded", result, download } });
      } catch (e) {
        const raw = e instanceof Error ? e.message : String(e);
        set({
          // Cancelled by the person: nothing failed, the update is on offer as before.
          phase:
            shellErrorCode(raw) === "update_cancelled"
              ? { kind: "available", result }
              : { kind: "download-failed", result, message: shellErrorText(raw, trFb) },
        });
      } finally {
        unlisten?.();
        set({ downloadProgress: null });
      }
    },

    async cancelDownload() {
      try {
        await updateDownloadCancel();
      } catch {
        // Nothing running, or a shell without the command: the download ends by itself.
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

// ─── Install on quit ─────────────────────────────────────────────────
//
// The Rust side holds at most one armed update (commands/self_install.rs) and installs it from
// `RunEvent::Exit` without starting the app again. This keeps it in step with the store: armed
// only while a downloaded update this copy installs without a password is waiting, the
// setting is on, it isn't skipped and nothing transfers (the same check the automatic
// install waits on). A transfer starting disarms it; finishing arms it again.

/** What should be armed right now, or null. */
export function installOnQuitTarget(
  s: Pick<
    UpdateStore,
    "phase" | "installOnQuitEnabled" | "installing" | "skippedVersion" | "lastInstall"
  >,
  idle: boolean,
): string | null {
  const p = s.phase;
  if (!s.installOnQuitEnabled || s.installing || !idle || p.kind !== "downloaded") return null;
  if (!p.download.can_install || p.download.needs_password) return null;
  if (p.result.latest_version === s.skippedVersion) return null;
  // Already failed once: not again on every quit.
  if (failedInstall(s, p.result.latest_version)) return null;
  return p.download.path;
}

let armedPath: string | null = null;
let syncing = false;
let syncAgain = false;
let watchingTransfers = false;

async function watchTransfers() {
  if (watchingTransfers) return;
  watchingTransfers = true;
  const [{ useUploadQueueStore }, { useTransferStore }] = await Promise.all([
    import("./uploadQueue"),
    import("./transfer"),
  ]);
  useUploadQueueStore.subscribe(() => void syncInstallOnQuit());
  useTransferStore.subscribe(() => void syncInstallOnQuit());
}

async function syncInstallOnQuit(): Promise<void> {
  if (syncing) {
    syncAgain = true;
    return;
  }
  syncing = true;
  try {
    do {
      syncAgain = false;
      const s = useUpdateStore.getState();
      // Only a downloaded update needs the (lazily loaded) transfer stores.
      const waiting = s.phase.kind === "downloaded";
      if (waiting) void watchTransfers();
      const want = waiting ? installOnQuitTarget(s, await appIdle()) : null;
      if (want === armedPath) continue;
      let armed = false;
      try {
        const version = s.phase.kind === "downloaded" ? s.phase.result.latest_version : null;
        armed = await updateInstallOnQuit(want, version);
        armedPath = want;
      } catch {
        // Not a downloaded update any more (moved or deleted): nothing is armed.
        armedPath = null;
      }
      if (useUpdateStore.getState().installOnQuitArmed !== armed) {
        useUpdateStore.setState({ installOnQuitArmed: armed });
      }
    } while (syncAgain);
  } finally {
    syncing = false;
  }
}

if (isTauriEnv() && !isMobile()) {
  useUpdateStore.subscribe((s, prev) => {
    if (
      s.phase !== prev.phase ||
      s.installOnQuitEnabled !== prev.installOnQuitEnabled ||
      s.installing !== prev.installing ||
      s.skippedVersion !== prev.skippedVersion ||
      s.lastInstall !== prev.lastInstall
    ) {
      void syncInstallOnQuit();
    }
  });
  // What the last install came to, before the launch check can start another.
  void useUpdateStore.getState().loadInstallStatus();
  // Closed to the tray (src-tauri/src/tray.rs): the app keeps running, so an update armed for
  // the quit waits until it is quit from the tray icon. Say so, once per version.
  void listen("tray-closed", () => {
    // Let the hidden window's blur / visibility arrive, so the notice goes to the system too.
    setTimeout(trayClosedHint, 500);
  }).catch(() => {});
}

function trayClosedHint() {
  const s = useUpdateStore.getState();
  const ver = trayHintVersion(s, safeGetItem(TRAY_HINT_KEY));
  if (!ver) return;
  safeSetItem(TRAY_HINT_KEY, ver);
  const title = trFb("update_tray_hint_title", { ver }, `v${ver} installs when you quit`);
  const body = trFb(
    "update_tray_hint_body",
    undefined,
    "PS5Upload is still running in the tray. Quit it from the tray icon to install the update, or open it and choose Restart now.",
  );
  const foreground = appIsForeground();
  pushNotification("info", title, { body, link: "/settings" });
  // The inbox mirrors to the system only in the background; a hidden window some systems still
  // report as focused gets it here, since the window that would show the inbox is gone.
  if (foreground && useNotificationsStore.getState().osNotifyEnabled) {
    void sendOsNotification("info", title, body);
  }
}
